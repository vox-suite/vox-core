# amazon-shopper

Local helper that lets Vox shop on **amazon.in** during a phone call. It drives a
visible Chrome window with Playwright, using a Chrome profile you have already
logged into. Vox Core's shopping tools call its small HTTP API. See
[docs/tools.md](../../docs/tools.md#8-shopping-tools).

```
Caller → Twilio → vox-bridge → vox-core (Gemini + amazon_* tools) → this helper → Chrome → amazon.in
```

This is a demo path. It is off unless `AMAZON_SHOPPER_URL` is set in vox-core.

## Setup (once)

Requires Node 22+ and Google Chrome.

```sh
cd services/amazon-shopper
npm install
cp .env.example .env        # set SHOPPER_TOKEN; keep SHOPPER_DRY_RUN=true for rehearsals
npm run login               # sign in to amazon.in in the window, tick "Keep me signed in", close it
```

Your password and OTP go only into Amazon's own page. The helper keeps the
session in the profile folder (`~/.vox-shopper/profile` by default) and never
stores credentials.

In vox-core's `.env`:

```sh
AMAZON_SHOPPER_URL=http://host.docker.internal:4100   # Core in Docker; use http://127.0.0.1:4100 with cargo run
AMAZON_SHOPPER_TOKEN=<same value as SHOPPER_TOKEN>
```

Restart Core API after changing these.

## Run

```sh
npm run dev
```

Chrome opens on amazon.in. The log says `DRY RUN` or `LIVE`. Stop it with Ctrl+C.
Stop the helper before running `npm run login`, because only one process can use
the profile at a time.

## What happens on a call

| Caller says | Tool | Browser |
|---|---|---|
| "I want to buy an iPhone 16" | `amazon_search`, `amazon_open_product` | searches, opens the matching listing |
| "Teal" | `amazon_select_options` | clicks the colour |
| "Yes, buy it" | `amazon_checkout` | Buy Now → default address → Pay on Delivery → review page |
| "Yes, place it" | `amazon_place_order` | clicks **Place your order** (or stops, in dry run) |

Vox Core only places the order from a turn *after* the one that prepared
checkout, so the caller must hear the total and answer first.

## Things to know before the demo

- **Pay on Delivery is limited to about ₹30,000.** For anything above that
  (an iPhone 16 is ₹79,900 or more) checkout returns `cod_unavailable`. To show the
  whole iPhone flow, keep `SHOPPER_DRY_RUN=true`: it stops on a highlighted
  "Place your order" button. For a real order, pick something cheaper.
- **Real orders:** set `SHOPPER_DRY_RUN=false`, restart, place the order, then
  cancel it in Your Orders.
- **Each call turn has 30 seconds in total.** That is vox-bridge's Core timeout
  (`vox-bridge/src/core/client.rs`). Search plus opening a product took about 4 s
  on a good connection and about 21 s on a slow one. Use a fast network. If turns
  still time out, raise that timeout to 60 s.
- **Verification pages:** if Amazon asks for a password, an OTP or a CAPTCHA,
  Vox tells the caller to check the screen. Finish it in the Chrome window, then
  ask Vox to try again.
- **Log in again on the venue network** before presenting, and run one dry-run
  checkout to catch any re-check early.
- There is one Amazon account and one browser, so it serves one caller at a time.

## Status of the selectors (checked 2026-09-26 against live amazon.in)

- **Verified:** search (sponsored results filtered), product page (title, price,
  colour and fixed-size variants), and colour selection.
- **Not yet verified:** checkout and place. Those need a logged-in account.
  Rehearse them in dry run first. The code tries role and text locators first,
  then Amazon's known element ids.

## API

All routes need `Authorization: Bearer $SHOPPER_TOKEN`. Every response has a
`status`.

| Route | Body | Status values |
|---|---|---|
| `GET /health` | | `ok` (with `logged_in`, `name`, `step`, `dry_run`) |
| `POST /search` | `{ "query": "iphone 16" }` | `ok`, `not_found`, `needs_human` |
| `POST /product` | `{ "asin": "B0DGJHBX5Y" }` | `ok`, `not_found`, `needs_human` |
| `POST /select` | `{ "options": { "Colour": "Teal" } }` | `ok`, `partial`, `invalid_state` |
| `POST /checkout` | `{ "quantity": 1 }` | `ok`, `cod_unavailable`, `needs_human`, `invalid_state`, `error` |
| `POST /place` | `{}` | `placed`, `dry_run`, `price_changed`, `unknown`, `needs_human`, `invalid_state` |

Unexpected failures return HTTP 500 with `{ "status": "error", "message": ... }`.

## Settings

| Variable | Default | |
|---|---|---|
| `SHOPPER_TOKEN` | required | Shared with vox-core's `AMAZON_SHOPPER_TOKEN` |
| `SHOPPER_DRY_RUN` | `true` | `false` really clicks Place your order |
| `SHOPPER_HOST` | `127.0.0.1` | Use `0.0.0.0` if Core in Docker cannot reach the helper |
| `SHOPPER_PORT` | `4100` | |
| `SHOPPER_PROFILE_DIR` | `~/.vox-shopper/profile` | Chrome profile holding the amazon.in session |
