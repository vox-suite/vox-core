/**
 * Local HTTP API that Vox Core's shopping tools call. One request runs at a
 * time because every step drives the same browser tab.
 */
import express, { type Request, type Response } from 'express';
import * as amazon from './amazon.js';
import { getPage } from './browser.js';
import { config } from './config.js';

if (!config.token) {
  console.error('SHOPPER_TOKEN is required (set the same value as AMAZON_SHOPPER_TOKEN in vox-core).');
  process.exit(1);
}

let queue: Promise<unknown> = Promise.resolve();
function serial<T>(task: () => Promise<T>): Promise<T> {
  const run = queue.then(task, task);
  queue = run.catch(() => {});
  return run;
}

const app = express();
app.use(express.json());
app.use((req, res, next) => {
  if (req.get('authorization') !== `Bearer ${config.token}`) {
    res.status(401).json({ status: 'unauthorized' });
    return;
  }
  next();
});

// A phone turn has 30 s in total (vox-bridge -> Core), including the model's
// own thinking, and Core waits at most 20 s per step. Answer within 10 s; a step
// still running then finishes in the background and the next call picks it up.
const RESPONSE_BUDGET_MS = 10_000;
// Placing must report its real outcome in one answer: Core treats the caller's
// confirmation as used once it asks, so an `in_progress` here couldn't be retried.
const PLACE_RESPONSE_BUDGET_MS = 17_000;
const FINISHED_RESULT_TTL_MS = 120_000;

const TOOL_NAMES: Record<string, string> = {
  search: 'amazon_search',
  product: 'amazon_open_product',
  select: 'amazon_select_options',
  checkout: 'amazon_checkout',
  place: 'amazon_place_order',
};
const stillLoading = (name: string): amazon.Result => ({
  status: 'in_progress',
  message: `Amazon is still loading. Do not call any Amazon tool again in this turn. Tell the user it will take a moment, then call ${TOOL_NAMES[name] ?? 'the same tool'} again after their next message.`,
});

/** A step whose caller already got `in_progress` and that is still driving the browser. */
let background: { name: string; key: string } | null = null;
/** The result of that step once it finished, for the caller's next identical call. */
let finished: { name: string; key: string; result: amazon.Result; at: number } | null = null;

function handle(
  name: string,
  step: (body: Record<string, unknown>) => Promise<amazon.Result>,
  budgetMs = RESPONSE_BUDGET_MS,
) {
  return async (req: Request, res: Response) => {
    const started = Date.now();
    const body = (req.body ?? {}) as Record<string, unknown>;
    const key = JSON.stringify(body);

    // Never queue behind a background step: repeating it gets an instant
    // "still loading", and anything else must not start over mid-checkout.
    if (background) {
      const result: amazon.Result =
        background.name === name
          ? stillLoading(name)
          : {
              status: 'busy',
              message: `Amazon is still finishing the previous step. Do not start over. Tell the user to hold on, then call ${TOOL_NAMES[background.name]} again after their next message.`,
            };
      console.log(`${name} -> ${result.status} (${background.name} still running in the background)`);
      res.json(result);
      return;
    }
    if (finished?.name === name && finished.key === key && Date.now() - finished.at < FINISHED_RESULT_TTL_MS) {
      const result = finished.result;
      finished = null;
      console.log(`${name} -> ${result.status} (finished earlier in the background)`);
      res.json(result);
      return;
    }
    finished = null;

    let abandoned = false;
    res.on('close', () => {
      if (!res.writableFinished) abandoned = true;
    });

    const work = serial(async (): Promise<amazon.Result> => {
      // The caller gave up while this waited in the queue: don't drive the browser for nobody.
      if (abandoned) return { status: 'cancelled' };
      return step(body);
    });
    let timer: NodeJS.Timeout | undefined;
    const slow = new Promise<amazon.Result>((resolve) => {
      timer = setTimeout(() => resolve(stillLoading(name)), budgetMs);
    });

    try {
      const result = await Promise.race([work, slow]);
      console.log(`${name} -> ${result.status} (${Date.now() - started} ms)`);
      if (result.status === 'in_progress') {
        background = { name, key };
        work
          .catch((err: unknown): amazon.Result => {
            console.error(`${name} failed in the background`, err);
            return { status: 'error', message: err instanceof Error ? err.message.split('\n')[0] : String(err) };
          })
          .then((late) => {
            console.log(`${name} finished in the background -> ${late.status} (${Date.now() - started} ms)`);
            finished = { name, key, result: late, at: Date.now() };
            background = null;
          });
      }
      if (!abandoned) res.json(result);
    } catch (err) {
      // Playwright errors carry a multi-line call log; the first line is enough to speak.
      const message = err instanceof Error ? err.message.split('\n')[0] : String(err);
      console.error(`${name} failed (${Date.now() - started} ms)`, err);
      if (!abandoned) res.status(500).json({ status: 'error', message });
    } finally {
      clearTimeout(timer);
    }
  };
}

function stringMap(value: unknown): Record<string, string> {
  if (!value || typeof value !== 'object') return {};
  return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, String(v)]));
}

app.get('/health', (req, res, next) => {
  // Don't navigate away from a checkout that is finishing in the background.
  if (background) {
    res.json({ status: 'ok', busy: background.name });
    return;
  }
  next();
}, handle('health', () => amazon.health()));
// Outside the queue on purpose, so it works while a step is stuck.
app.get('/debug', async (_req, res) => {
  res.json(await amazon.debug().catch((err: unknown) => ({ status: 'error', message: String(err) })));
});
app.post('/search', handle('search', (b) => amazon.search(String(b.query ?? ''))));
app.post('/product', handle('product', (b) => amazon.openProduct(String(b.asin ?? ''))));
app.post('/select', handle('select', (b) => amazon.selectOptions(stringMap(b.options))));
app.post('/checkout', handle('checkout', (b) => amazon.checkout(Math.max(1, Number(b.quantity) || 1))));
app.post('/place', handle('place', () => amazon.placeOrder(), PLACE_RESPONSE_BUDGET_MS));

// Playwright's own signal handlers close Chrome but leave the process (and
// port) alive; exit explicitly so Ctrl+C really stops the helper.
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => process.exit(0));
}

// Open the browser up front so the first call doesn't pay Chrome's startup.
const page = await getPage();
await page.goto(config.baseUrl, { waitUntil: 'commit' }).catch(() => {});

app.listen(config.port, config.host, (err?: Error) => {
  if (err) {
    // e.g. EADDRINUSE: another helper is already running.
    console.error(`Could not listen on ${config.host}:${config.port}: ${err.message}`);
    process.exit(1);
  }
  console.log(`amazon-shopper listening on http://${config.host}:${config.port}`);
  console.log(`profile: ${config.profileDir}`);
  console.log(config.dryRun ? 'DRY RUN: orders stop before "Place your order"' : 'LIVE: orders will be placed');
});
