// Drive the apps' HTML front end in Chromium through accessibility roles and names only (no
// ids), the way a computer-use agent sees them. Used by the ignored `ui_is_operable_by_role`
// test; also runnable by hand against a server started by that test.
//
//   node bench/cua/env/ui-smoke.mjs http://127.0.0.1:PORT
//
// Needs the `playwright` package and a Chromium it can launch (NUGGET_CUA_CHROMIUM overrides).
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright');

const base = process.argv[2];
if (!base) {
  console.error('usage: ui-smoke.mjs BASE_URL');
  process.exit(2);
}
const opts = process.env.NUGGET_CUA_CHROMIUM ? { executablePath: process.env.NUGGET_CUA_CHROMIUM } : {};
const browser = await chromium.launch(opts);
const page = await browser.newPage();

// Task `mail-reply-lunch`: reply to Priya.
await page.goto(`${base}/mail/`);
await page.getByRole('button', { name: 'Priya Shah: Lunch Thursday?' }).click();
await page.getByRole('button', { name: 'Reply' }).waitFor();
await page.getByRole('button', { name: 'Reply' }).click();
await page.getByRole('textbox', { name: 'Message' }).waitFor();
const body = page.getByRole('textbox', { name: 'Message' });
await body.fill('Hi Priya, Thursday at 12:30 works for me.');
// The page posts the text on `change` and reloads with the server's state.
await Promise.all([page.waitForEvent('load'), body.blur()]);
await page.getByRole('button', { name: 'Send' }).click();
await page.getByRole('heading', { name: /Inbox/ }).waitFor();

console.log(JSON.stringify({ ok: true, title: await page.title() }));
await browser.close();
