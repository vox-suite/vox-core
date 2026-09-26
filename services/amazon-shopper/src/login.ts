/**
 * One-time manual login: opens the shopper's Chrome profile on amazon.in so
 * you can sign in yourself. The session cookies stay in the profile; no
 * password or OTP is ever stored by Vox. Stop the server first.
 */
import { openContext } from './browser.js';
import { config } from './config.js';

const context = await openContext();
const page = context.pages()[0] ?? (await context.newPage());
await page.goto(config.baseUrl, { waitUntil: 'commit' });

console.log('Sign in to amazon.in in the Chrome window and tick "Keep me signed in".');
console.log('While signed in, check that Your Addresses has the right default delivery address.');
console.log('Close the Chrome window when you are done.');

let announced = false;
const poll = setInterval(async () => {
  const greeting = await page
    .locator('#nav-link-accountList-nav-line-1')
    .first()
    .textContent({ timeout: 1_000 })
    .catch(() => null);
  const name = greeting?.replace(/^Hello,?\s*/i, '').trim();
  if (!announced && name && !/sign in/i.test(name)) {
    announced = true;
    console.log(`Signed in as ${name}. You can close the window.`);
  }
}, 2_000);

await new Promise((resolve) => context.on('close', resolve));
clearInterval(poll);
console.log(`Saved session in ${config.profileDir}`);
