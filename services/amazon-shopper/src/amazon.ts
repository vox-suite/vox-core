/**
 * amazon.in steps driven in the visible browser: search, open a product, pick
 * variants, prepare checkout (default address + Pay on Delivery), place order.
 *
 * Every function returns `{ status, ... }` for handled outcomes; Vox reads the
 * status aloud. Selectors prefer roles/text and fall back to known Amazon ids,
 * because amazon.in markup varies between pages and over time.
 */
import type { Locator, Page } from 'playwright-core';
import { getPage } from './browser.js';
import { config } from './config.js';

export type Result = { status: string; [key: string]: unknown };

type Option = { label: string; selected: boolean; available: boolean; ref: string };
type Variant = { dimension: string; key: string; options: Option[] };
type Product = {
  asin: string;
  title: string;
  price: string | null;
  availability: string | null;
  variants: Variant[];
};
type Checkout = {
  total: string | null;
  address: string | null;
  delivery: string | null;
  payment: string | null;
};

type Step = 'idle' | 'product_open' | 'checkout_ready' | 'placed';
const state: { step: Step; total: string | null } = { step: 'idle', total: null };

const RESULT_CARD = 'div[data-component-type="s-search-result"][data-asin]:not([data-asin=""])';
// Longer than one HTTP request's budget: if the server answers `in_progress`,
// checkout keeps going in the background and the next call resumes it.
const CHECKOUT_BUDGET_MS = 30_000;

// ---------------------------------------------------------------- helpers

async function visible(locator: Locator): Promise<boolean> {
  return locator.isVisible().catch(() => false);
}

/** Clicks the first match if it shows up within `waitMs`; reports whether it clicked. */
async function clickIfVisible(locator: Locator, waitMs = 0): Promise<boolean> {
  const target = locator.first();
  if (waitMs > 0) await target.waitFor({ state: 'visible', timeout: waitMs }).catch(() => {});
  if (!(await visible(target))) return false;
  await target.click().catch(() => {});
  return true;
}

/**
 * Short pause for the page to react. Deliberately not a load-state wait:
 * amazon.in product pages can take 20 s+ to fire domcontentloaded, so steps
 * wait for the specific element they need instead.
 */
async function settle(page: Page, ms = 600): Promise<void> {
  await page.waitForTimeout(ms);
}

const BLOCKER_MESSAGES: Record<string, string> = {
  sign_in: 'Amazon is asking to sign in again. Complete it in the browser window, then try again.',
  verification:
    'Amazon wants a verification code (OTP). Complete it in the browser window, then try again.',
  captcha: 'Amazon is showing a CAPTCHA. Solve it in the browser window, then try again.',
};

/** Pages a person must finish by hand; the window is visible, so the presenter can. */
async function blocker(page: Page): Promise<Result | null> {
  const url = page.url();
  let reason: string | null = null;
  if (/\/ap\/(signin|challenge)/.test(url)) reason = 'sign_in';
  else if (/\/ap\/(mfa|cvf)/.test(url)) reason = 'verification';
  else if ((await page.locator('form[action*="validateCaptcha"], #captchacharacters').count()) > 0)
    reason = 'captcha';
  return reason ? { status: 'needs_human', reason, message: BLOCKER_MESSAGES[reason] } : null;
}

const norm = (s: string) =>
  s
    .toLowerCase()
    .replace(/colour/g, 'color')
    .replace(/[^a-z0-9]/g, '');

function findVariant(variants: Variant[], wanted: string): Variant | undefined {
  const w = norm(wanted);
  const sizeLike = ['size', 'storage', 'capacity', 'memory'].includes(w);
  return (
    variants.find((v) => norm(v.dimension) === w || norm(v.key) === w) ??
    variants.find((v) => norm(v.dimension).includes(w) || norm(v.key).includes(w)) ??
    (sizeLike ? variants.find((v) => /size|storage|capacity/.test(norm(v.key + v.dimension))) : undefined)
  );
}

function findOption(options: Option[], wanted: string): Option | undefined {
  const w = norm(wanted);
  return (
    options.find((o) => norm(o.label) === w) ??
    options.find((o) => norm(o.label).includes(w) || w.includes(norm(o.label)))
  );
}

/** What Vox sees: no internal element refs. */
function publicProduct(p: Product) {
  return {
    asin: p.asin,
    title: p.title.slice(0, 150),
    price: p.price,
    availability: p.availability,
    variants: p.variants.map((v) => ({
      dimension: v.dimension,
      options: v.options.map(({ label, selected, available }) => ({ label, selected, available })),
    })),
  };
}

// ------------------------------------------------------------ page readers

/**
 * Reads the product page and tags each variant option element with
 * `data-vox-option` so it can be clicked reliably afterwards.
 */
function readProduct(page: Page): Promise<Product> {
  return page.evaluate(() => {
    const clean = (s?: string | null) => (s ?? '').replace(/\s+/g, ' ').trim();
    const first = (selectors: string[]) => {
      for (const s of selectors) {
        const t = clean(document.querySelector(s)?.textContent);
        if (t) return t;
      }
      return '';
    };
    const humanize = (key: string) =>
      key
        .replace(/_name$/, '')
        .replace(/_/g, ' ')
        .replace(/^\w/, (c) => c.toUpperCase());

    const title = first(['#productTitle']);
    const price = first([
      '#corePriceDisplay_desktop_feature_div .priceToPay .a-offscreen',
      '#corePriceDisplay_desktop_feature_div .a-price .a-offscreen',
      '#corePrice_feature_div .a-offscreen',
      '.priceToPay .a-offscreen',
      '#tp_price_block_total_price_ww .a-offscreen',
    ]);
    const availability = first(['#availability span', '#availability']);
    const asin =
      (document.querySelector('input#ASIN') as HTMLInputElement | null)?.value ||
      location.pathname.match(/\/dp\/([A-Z0-9]{10})/)?.[1] ||
      '';

    document.querySelectorAll('[data-vox-option]').forEach((el) => el.removeAttribute('data-vox-option'));
    const inline = [...document.querySelectorAll('[id^="inline-twister-row-"]')];
    const rows = inline.length ? inline : [...document.querySelectorAll('#twister [id^="variation_"]')];
    const variants = rows
      .map((row, d) => {
        const key = row.id.replace(/^inline-twister-row-|^variation_/, '');
        const heading = row.querySelector(
          '[id^="inline-twister-dim-title-"] .a-color-secondary, [id^="inline-twister-dim-title-"] span, .a-form-label',
        );
        const dimension = clean(heading?.textContent).split(':')[0].trim() || humanize(key);
        const options = [...row.querySelectorAll('li')]
          .map((li, o) => {
            const label =
              clean(li.querySelector('img')?.getAttribute('alt')) ||
              clean(li.getAttribute('title')?.replace(/^Click to select\s*/i, '')) ||
              clean(
                li.querySelector('.swatch-title-text-display, .swatch-title-text, .a-button-text')?.textContent,
              ).split(' ₹')[0] ||
              clean(li.textContent).split(' ₹')[0];
            const classes = `${li.className} ${li.querySelector('.a-button')?.className ?? ''}`;
            const ref = `${d}-${o}`;
            li.setAttribute('data-vox-option', ref);
            return {
              label,
              selected:
                /selected|swatchSelect/i.test(classes) ||
                li.querySelector('[aria-checked="true"], [aria-pressed="true"]') !== null,
              available: !/unavailable/i.test(classes),
              ref,
            };
          })
          .filter((o) => o.label);
        return { dimension, key, options };
      })
      .filter((v) => v.options.length > 1);

    // Dimensions with a single value (e.g. this listing only comes in 128 GB).
    for (const header of document.querySelectorAll('[id^="inline-twister-singleton-header-"]')) {
      const key = header.id.replace(/^inline-twister-singleton-header-/, '');
      const label = clean(document.getElementById(`inline-twister-expanded-dimension-text-${key}`)?.textContent);
      if (!label || variants.some((v) => v.key === key)) continue;
      const dimension = clean(header.querySelector('.a-color-secondary')?.textContent).split(':')[0].trim() || humanize(key);
      variants.push({ dimension, key, options: [{ label, selected: true, available: true, ref: '' }] });
    }

    return { asin, title, price: price || null, availability: availability || null, variants };
  });
}

function readCheckout(page: Page): Promise<Checkout> {
  return page.evaluate(() => {
    const body = document.body.innerText;
    const money = /₹\s?[\d,]+(?:\.\d{1,2})?/;
    const clean = (s?: string | null) => (s ?? '').replace(/\s+/g, ' ').trim();

    let total =
      clean(
        document.querySelector(
          '#subtotals-marketplace-table .grand-total-price, .order-summary-grand-total .a-offscreen, #subtotals-marketplace-spp-bottom .grand-total-price',
        )?.textContent,
      ).match(money)?.[0] ?? '';
    if (!total) total = body.match(/Order Total:?\s*(₹\s?[\d,]+(?:\.\d{1,2})?)/i)?.[1] ?? '';

    const address = clean(
      body.match(/Delivering to\s+([^\n]+(?:\n[^\n]+)?)/i)?.[1] ??
        body.match(/Delivery address\s*\n\s*([^\n]+(?:\n[^\n]+)?)/i)?.[1] ??
        '',
    ).replace(/\bChange\b.*$/i, '');
    const delivery = clean(
      body.match(/(Arriving [^\n]+|Delivery date:?[^\n]+|Get it by [^\n]+|Guaranteed delivery:?[^\n]+)/i)?.[1],
    );
    const paymentBlock = body.match(/(Paying with|Payment method)[\s\S]{0,200}/i)?.[0] ?? '';
    const payment = /(cash|pay) on delivery/i.test(paymentBlock)
      ? 'Pay on Delivery'
      : clean(body.match(/Paying with\s+([^\n]+)/i)?.[1]);

    return {
      total: total || null,
      address: address.slice(0, 120) || null,
      delivery: delivery || null,
      payment: payment || null,
    };
  });
}

const placeButton = (page: Page) =>
  page
    .locator('input[name="placeYourOrder1"], #submitOrderButtonId input, #placeYourOrder input')
    .or(page.getByRole('button', { name: /place your order/i }))
    .first();

const codRadio = (page: Page) =>
  page
    .getByRole('radio', { name: /(cash|pay) on delivery/i })
    .or(page.locator('label:has-text("Pay on Delivery") input[type=radio], label:has-text("Cash on Delivery") input[type=radio]'))
    .first();

// Exact decline wording only, so a "Start your free trial" button can never match.
const NO_THANKS = /^\s*(no,?\s*thanks|skip)\s*$/i;

/**
 * Declines upsells such as the Prime "30 days FREE" popup, which Amazon can
 * render in a separate frame. Falls back to the popup's Close button.
 */
async function declineUpsell(page: Page): Promise<boolean> {
  for (const frame of page.frames()) {
    const decline = frame
      .getByRole('button', { name: NO_THANKS })
      .or(frame.getByRole('link', { name: NO_THANKS }))
      .or(frame.getByText(NO_THANKS))
      .first();
    if (await visible(decline)) {
      await decline.click().catch(() => {});
      return true;
    }
  }
  for (const frame of page.frames()) {
    const primePopup = frame.getByText(/free trial|days of prime/i).first();
    const close = frame.getByRole('button', { name: /^close$/i }).first();
    if ((await visible(primePopup)) && (await visible(close))) {
      await close.click().catch(() => {});
      return true;
    }
  }
  return false;
}

const usePaymentMethod = (page: Page) =>
  page
    .getByRole('button', { name: /use this payment method/i })
    .or(page.locator('input[name*="SetPaymentPlanSelectionEvent"]'))
    .first();

const changePayment = (page: Page) =>
  page
    .locator('#payChangeButtonId, a[data-testid="payment-change-link"]')
    .or(page.getByRole('link', { name: /change payment/i }))
    .first();

// ------------------------------------------------------------------ steps

function readResults(page: Page) {
  return page.$$eval(RESULT_CARD, (cards) =>
    cards
      .map((card) => {
        const text = (selector: string) => card.querySelector(selector)?.textContent?.trim() ?? '';
        const sponsored =
          card.querySelector('[aria-label^="Sponsored"], .puis-sponsored-label-text, .s-sponsored-label-text') !==
            null || /\bSponsored\b/.test(text('.puis-label-popover, .s-label-popover-default'));
        // amazon.in puts the brand in its own short h2 above the product name.
        const headings = [...card.querySelectorAll('h2')]
          .map((h) => (h.textContent ?? '').replace(/\s+/g, ' ').trim())
          .filter(Boolean)
          .sort((a, b) => b.length - a.length);
        const [name = '', brand = ''] = headings;
        const title = brand && !name.toLowerCase().startsWith(brand.toLowerCase()) ? `${brand} ${name}` : name;
        return {
          asin: card.getAttribute('data-asin') ?? '',
          title: title.slice(0, 150),
          price: text('.a-price:not(.a-text-price) .a-offscreen') || null,
          rating: text('.a-icon-alt').match(/[\d.]+/)?.[0] ?? null,
          sponsored,
        };
      })
      .filter((r) => r.asin && r.title && !r.sponsored)
      .slice(0, 5)
      .map(({ sponsored: _sponsored, ...r }) => r),
  );
}

export async function search(query: string): Promise<Result> {
  if (!query.trim()) return { status: 'error', message: 'query is required' };
  const page = await getPage();
  await page.goto(`${config.baseUrl}/s?k=${encodeURIComponent(query.trim())}`, { waitUntil: 'commit' });
  // Right after `commit` the old page can still be unloading, which makes the
  // first wait fail at once; wait again on the new document.
  for (let attempt = 0; attempt < 2; attempt++) {
    const found = await page
      .locator(RESULT_CARD)
      .first()
      .waitFor({ timeout: 8_000 })
      .then(() => true)
      .catch(() => false);
    if (found) break;
  }
  const blocked = await blocker(page);
  if (blocked) return blocked;

  // Results stream in: sponsored slots render first and organic ones a moment
  // later, so keep reading until organic results appear.
  let results: Awaited<ReturnType<typeof readResults>> = [];
  const until = Date.now() + 8_000;
  do {
    results = await readResults(page).catch(() => []);
    if (results.length > 0) break;
    await page.waitForTimeout(400);
  } while (Date.now() < until);

  state.step = 'idle';
  state.total = null;
  if (results.length === 0) return { status: 'not_found', message: `No results for "${query}".` };
  return { status: 'ok', query, results };
}

export async function openProduct(asin: string): Promise<Result> {
  if (!/^[A-Z0-9]{10}$/i.test(asin.trim()))
    return { status: 'error', message: 'asin must be a 10-character Amazon ASIN' };
  const page = await getPage();
  await page.goto(`${config.baseUrl}/dp/${asin.trim().toUpperCase()}`, { waitUntil: 'commit' });
  await page.locator('#productTitle').waitFor().catch(() => {});
  const blocked = await blocker(page);
  if (blocked) return blocked;
  // The buy box and variant rows render after the title. The twister
  // container itself appears early and empty, so wait for its rows.
  await page
    .locator('#buy-now-button, #add-to-cart-button')
    .first()
    .waitFor({ timeout: 4_000 })
    .catch(() => {});
  if ((await page.locator('#twister_feature_div').count()) > 0)
    await page
      .locator('[id^="inline-twister-row-"], [id^="inline-twister-singleton-header-"], #twister [id^="variation_"]')
      .first()
      .waitFor({ timeout: 3_000 })
      .catch(() => {});

  const product = await readProduct(page);
  if (!product.title) return { status: 'not_found', message: 'Could not read that product page.' };
  state.step = 'product_open';
  state.total = null;
  return { status: 'ok', ...publicProduct(product) };
}

async function waitForSelected(page: Page, dimension: string, label: string): Promise<void> {
  const deadline = Date.now() + 8_000;
  while (Date.now() < deadline) {
    await settle(page, 400);
    const product = await readProduct(page).catch(() => null);
    const variant = product && findVariant(product.variants, dimension);
    if (variant?.options.some((o) => o.label === label && o.selected)) return;
  }
}

export async function selectOptions(wanted: Record<string, string>): Promise<Result> {
  if (state.step !== 'product_open')
    return { status: 'invalid_state', message: 'Open a product first with amazon_open_product.' };
  const page = await getPage();
  const applied: string[] = [];
  const unmatched: string[] = [];

  for (const [dimension, label] of Object.entries(wanted)) {
    const variant = findVariant((await readProduct(page)).variants, dimension);
    const option = variant && findOption(variant.options, String(label));
    if (!variant || !option) {
      unmatched.push(`${dimension}: ${label}`);
      continue;
    }
    if (!option.available) {
      unmatched.push(`${variant.dimension}: ${option.label} (unavailable)`);
      continue;
    }
    if (!option.selected) {
      const li = page.locator(`[data-vox-option="${option.ref}"]`);
      const target = li.locator('button, input, .a-button-text, img, a').first();
      await ((await target.count()) > 0 ? target : li).click();
      await waitForSelected(page, variant.dimension, option.label);
    }
    applied.push(`${variant.dimension}: ${option.label}`);
  }

  const blocked = await blocker(page);
  if (blocked) return blocked;
  const product = await readProduct(page);
  state.total = null;
  return {
    status: unmatched.length ? 'partial' : 'ok',
    applied,
    unmatched,
    ...publicProduct(product),
  };
}

async function codUnavailable(page: Page): Promise<Result> {
  const { total } = await readCheckout(page).catch(() => ({ total: null }));
  state.step = 'idle';
  return {
    status: 'cod_unavailable',
    total,
    message: 'Amazon does not offer Cash/Pay on Delivery for this order (it is usually limited to orders under ₹30,000).',
  };
}

const onCheckoutPage = (page: Page) => /\/(checkout|gp\/buy|buy)\//i.test(new URL(page.url()).pathname);
const log = (message: string) => console.log(`  ${message}`);

/**
 * Resumable: if an earlier call ran out of time, Chrome is already on a
 * checkout page, so continue from there instead of clicking Buy Now again.
 */
export async function checkout(quantity = 1): Promise<Result> {
  const page = await getPage();
  if (state.step === 'checkout_ready' && onCheckoutPage(page) && (await visible(placeButton(page)))) {
    return { status: 'ok', ...(await readCheckout(page)), payment: 'Pay on Delivery' };
  }

  if (onCheckoutPage(page) && state.step !== 'placed') {
    log(`checkout: resuming on ${new URL(page.url()).pathname}`);
  } else {
    if (state.step !== 'product_open')
      return { status: 'invalid_state', message: 'Open a product (and pick its options) before checkout.' };
    if (quantity > 1) await page.selectOption('#quantity', String(quantity)).catch(() => {});
    // Phones offer an exchange accordion; the plain purchase is "Without Exchange".
    await clickIfVisible(page.getByText(/^\s*without exchange\s*$/i));

    const buyNow = page.locator('#buy-now-button').or(page.getByRole('button', { name: /buy now/i })).first();
    if (!(await visible(buyNow)))
      return {
        status: 'error',
        message: 'This product has no Buy Now button. It may be unavailable, or need its options chosen first.',
      };
    await buyNow.click({ noWaitAfter: true });
    log('checkout: clicked Buy Now');
  }

  let codSelected = false;
  let codAttempts = 0;
  let openedPaymentPicker = false;
  let lastPath = '';
  const deadline = Date.now() + CHECKOUT_BUDGET_MS;
  while (Date.now() < deadline) {
    await settle(page, 500);
    const path = new URL(page.url()).pathname;
    if (path !== lastPath) log(`checkout: on ${(lastPath = path)}`);
    const blocked = await blocker(page);
    if (blocked) return blocked;

    if (await visible(page.locator('#turbo-checkout-iframe')))
      return {
        status: 'error',
        message:
          "Amazon opened its one-click Buy Now popup, which can't switch to Pay on Delivery. Turn off 1-click in the Amazon account and try again.",
      };

    // Protection-plan and Prime upsells.
    if (await declineUpsell(page)) {
      log('checkout: declined an upsell (No Thanks)');
      continue;
    }
    // Older flow: confirm the (default) delivery address.
    if (
      await clickIfVisible(
        page
          .locator('#shipToThisAddressButton, input[data-testid="Address_selectShipToThisAddress"]')
          .or(page.getByRole('button', { name: /deliver to this address|use this address/i })),
      )
    ) {
      log('checkout: confirmed the default address');
      continue;
    }

    // Payment page: judge by what the page shows, not by what we did before,
    // because Amazon can bring the payment page back with nothing selected.
    const cod = codRadio(page);
    if (await visible(cod)) {
      if (await cod.isDisabled().catch(() => false)) return codUnavailable(page);
      let acted = false;
      if (!(await cod.isChecked().catch(() => false))) {
        if (++codAttempts > 3)
          return { status: 'error', message: "Couldn't select Pay on Delivery. Check the browser window." };
        await cod.check({ force: true }).catch(() => cod.click({ force: true }));
        log('checkout: selected Pay on Delivery');
        acted = true;
      }
      codSelected = true;
      if (await clickIfVisible(usePaymentMethod(page), acted ? 3_000 : 0)) {
        log('checkout: clicked "Use this payment method"');
        acted = true;
      }
      if (acted) continue;
    }

    if (await visible(placeButton(page))) {
      const summary = await readCheckout(page);
      const payingOnDelivery = codSelected || summary.payment === 'Pay on Delivery';
      if (!payingOnDelivery) {
        // Another default payment is selected: open the payment picker once.
        if (!codSelected && !openedPaymentPicker && (await clickIfVisible(changePayment(page)))) {
          openedPaymentPicker = true;
          log('checkout: opened the payment picker (another method was selected)');
          continue;
        }
        log(`checkout: payment shows "${summary.payment}", not Pay on Delivery`);
        return codUnavailable(page);
      }
      log(`checkout: ready, total ${summary.total}`);
      state.step = 'checkout_ready';
      state.total = summary.total;
      return { status: 'ok', ...summary, payment: 'Pay on Delivery' };
    }

    // Payment collapsed behind a "Change" link, with no radios or Place button yet.
    // Only before Pay on Delivery is chosen: later pages (offers, Prime) have
    // their own "Change" links that lead back to the payment page.
    if (!codSelected && !openedPaymentPicker && (await clickIfVisible(changePayment(page)))) {
      openedPaymentPicker = true;
      log('checkout: opened the payment picker');
      continue;
    }
    if (openedPaymentPicker && !codSelected && !(await visible(cod))) {
      const radios = await page.getByRole('radio').count();
      if (radios > 0) return codUnavailable(page);
    }
  }
  log('checkout: gave up; call GET /debug to see what the page shows');
  return { status: 'error', message: 'Checkout took too long. Check the browser window.' };
}

/** What the browser currently shows, for diagnosing a stuck step. Read-only. */
export async function debug(): Promise<Result> {
  const page = await getPage();
  const dom = await page
    .evaluate(() => {
      const shown = (el: Element) => {
        const r = (el as HTMLElement).getBoundingClientRect();
        return r.width > 0 && r.height > 0;
      };
      const label = (el: Element) =>
        ((el as HTMLInputElement).value || el.getAttribute('aria-label') || el.textContent || '')
          .replace(/\s+/g, ' ')
          .trim()
          .slice(0, 80);
      return {
        title: document.title,
        buttons: [...document.querySelectorAll('button, input[type=submit], input[type=button], [role=button]')]
          .filter(shown)
          .map(label)
          .filter(Boolean)
          .slice(0, 40),
        radios: [...document.querySelectorAll('input[type=radio]')].slice(0, 30).map((r) => ({
          label: (r.closest('label')?.textContent ?? r.getAttribute('aria-label') ?? r.getAttribute('name') ?? '')
            .replace(/\s+/g, ' ')
            .trim()
            .slice(0, 80),
          checked: (r as HTMLInputElement).checked,
          disabled: (r as HTMLInputElement).disabled,
          shown: shown(r),
        })),
      };
    })
    .catch((err: unknown) => ({ error: String(err) }));
  // Amazon's action buttons are inputs labelled by another element; the
  // accessibility snapshot shows them with the names a person would see.
  const aria = await page
    .locator('body')
    .ariaSnapshot({ timeout: 3_000 })
    .catch(() => '');
  return {
    status: 'ok',
    url: page.url(),
    step: state.step,
    blocker: await blocker(page),
    place_button_visible: await visible(placeButton(page)),
    cod_radio_visible: await visible(codRadio(page)),
    ...dom,
    aria: aria.slice(0, 6_000),
  };
}

export async function placeOrder(): Promise<Result> {
  if (state.step !== 'checkout_ready')
    return { status: 'invalid_state', message: 'No checkout is ready. Run checkout first.' };
  const page = await getPage();
  const blocked = await blocker(page);
  if (blocked) return blocked;

  const button = placeButton(page);
  if (!(await visible(button))) {
    state.step = 'idle';
    return { status: 'invalid_state', message: 'The checkout page is no longer open. Run checkout again.' };
  }
  const { total } = await readCheckout(page);
  if (state.total && total && total !== state.total) {
    state.step = 'idle';
    return {
      status: 'price_changed',
      previous_total: state.total,
      total,
      message: 'The total changed since checkout. Confirm the new total with the user and run checkout again.',
    };
  }

  if (config.dryRun) {
    await button.scrollIntoViewIfNeeded().catch(() => {});
    await button
      .evaluate((el) => {
        (el as HTMLElement).style.outline = '4px solid #e11d48';
        (el as HTMLElement).style.outlineOffset = '4px';
      })
      .catch(() => {});
    state.step = 'idle';
    return { status: 'dry_run', total, message: 'Dry run: stopped before clicking Place your order.' };
  }

  // Never click twice, even if confirming the result fails below.
  state.step = 'placed';
  await button.click();

  // Stays under the server's 17 s answer budget for placing.
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline && !/thankyou|thank-you|order-confirmation/i.test(page.url())) {
    await settle(page, 500);
    if (await declineUpsell(page)) log('place: declined an upsell (No Thanks)');
    const blockedAfter = await blocker(page);
    if (blockedAfter)
      return {
        ...blockedAfter,
        message: `${blockedAfter.message} The order may not be placed yet; check Your Orders before retrying.`,
      };
  }

  const confirmed = /thankyou|thank-you|order-confirmation/i.test(page.url());
  const orderId =
    (await page
      .evaluate(() => document.body.innerText.match(/\b\d{3}-\d{7}-\d{7}\b/)?.[0] ?? null)
      .catch(() => null)) ?? new URL(page.url()).searchParams.get('purchaseId');
  if (!confirmed && !orderId)
    return {
      status: 'unknown',
      total,
      message: "Clicked Place your order but couldn't confirm it. Check Your Orders on Amazon before retrying.",
    };
  return { status: 'placed', order_id: orderId, total };
}

export async function health(): Promise<Result> {
  const page = await getPage();
  if (state.step === 'idle' && !page.url().startsWith(config.baseUrl))
    await page.goto(config.baseUrl, { waitUntil: 'commit' });
  const greeting = await page
    .locator('#nav-link-accountList-nav-line-1')
    .first()
    .textContent({ timeout: 3_000 })
    .catch(() => null);
  const name = greeting?.replace(/^Hello,?\s*/i, '').trim() || null;
  const loggedIn = !!name && !/sign in/i.test(name);
  return { status: 'ok', logged_in: loggedIn, name: loggedIn ? name : null, step: state.step, dry_run: config.dryRun };
}
