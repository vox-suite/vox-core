import { chromium, type BrowserContext, type Page } from 'playwright-core';
import { config } from './config.js';

let context: Promise<BrowserContext> | undefined;

/**
 * One visible Chrome window on a dedicated, persistent profile, so the
 * amazon.in login survives restarts. Only one process can use the profile at
 * a time: stop the server before running `npm run login`.
 */
export function openContext(): Promise<BrowserContext> {
  context ??= chromium
    .launchPersistentContext(config.profileDir, {
      channel: 'chrome',
      headless: false,
      viewport: null,
      args: ['--start-maximized'],
    })
    .then(async (ctx) => {
      // tsx (esbuild keepNames) wraps named functions in `__name(...)`; functions
      // passed to page.evaluate run in the page, which lacks that helper.
      await ctx.addInitScript({ content: 'globalThis.__name = (fn) => fn;' });
      ctx.on('close', () => {
        context = undefined;
      });
      return ctx;
    })
    .catch((err: unknown) => {
      context = undefined;
      throw err;
    });
  return context;
}

/** The single working tab every step drives. */
export async function getPage(): Promise<Page> {
  const ctx = await openContext();
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  page.setDefaultTimeout(10_000);
  page.setDefaultNavigationTimeout(15_000);
  return page;
}
