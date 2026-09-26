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

// Vox Core waits 20 s per step, so always answer before that. A step still
// running then keeps going in the background (checkout resumes on the next call).
const RESPONSE_BUDGET_MS = 17_000;

function handle(name: string, step: (body: Record<string, unknown>) => Promise<amazon.Result>) {
  return async (req: Request, res: Response) => {
    const started = Date.now();
    let abandoned = false;
    res.on('close', () => {
      if (!res.writableFinished) abandoned = true;
    });

    const work = serial(async (): Promise<amazon.Result> => {
      // The caller gave up while this waited in the queue: don't drive the browser for nobody.
      if (abandoned) return { status: 'cancelled' };
      return step((req.body ?? {}) as Record<string, unknown>);
    });
    let timer: NodeJS.Timeout | undefined;
    const slow = new Promise<amazon.Result>((resolve) => {
      timer = setTimeout(
        () =>
          resolve({
            status: 'in_progress',
            message: 'Amazon is still loading. Tell the user it will take a moment, and call this tool again on their next message.',
          }),
        RESPONSE_BUDGET_MS,
      );
    });

    try {
      const result = await Promise.race([work, slow]);
      console.log(`${name} -> ${result.status} (${Date.now() - started} ms)`);
      if (result.status === 'in_progress')
        work.then(
          (late) => console.log(`${name} finished in the background -> ${late.status} (${Date.now() - started} ms)`),
          (err: unknown) => console.error(`${name} failed in the background`, err),
        );
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

app.get('/health', handle('health', () => amazon.health()));
// Outside the queue on purpose, so it works while a step is stuck.
app.get('/debug', async (_req, res) => {
  res.json(await amazon.debug().catch((err: unknown) => ({ status: 'error', message: String(err) })));
});
app.post('/search', handle('search', (b) => amazon.search(String(b.query ?? ''))));
app.post('/product', handle('product', (b) => amazon.openProduct(String(b.asin ?? ''))));
app.post('/select', handle('select', (b) => amazon.selectOptions(stringMap(b.options))));
app.post('/checkout', handle('checkout', (b) => amazon.checkout(Math.max(1, Number(b.quantity) || 1))));
app.post('/place', handle('place', () => amazon.placeOrder()));

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
