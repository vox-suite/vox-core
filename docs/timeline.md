# Timeline

Every source listed as **Built** is in code and has been tested by the team with real accounts. Facts below come from the code and docs in this repo as of 2026-10-12.

## 1. What it is

Timeline is a day-by-day record of what happened in your life, built automatically from the apps and files you connect. A purchase email, a place you visited, a game you played, a video you watched, a calendar meeting, a delivery: each becomes one **event** with a time, a title and some details. You scroll through your days instead of through ten different apps.

It is the base layer of Vox. **Pulse** (to be renamed Insights) draws its charts from these events, the voice assistant reads them when you ask questions, and Plans reason over them.

## 2. Why it matters (the pitch in plain words)

- One place to answer "what did I do on Tuesday?" or "what did I spend last week?".
- Nothing to type. Events arrive from the accounts and files you connect.
- Every event keeps a link to where it came from (the **evidence**), so you can always check why it is there.
- Duplicates are removed. The same bank debit seen in an email and in a statement PDF shows up once.
- You stay in control: each connected account has separate switches for "add to Timeline" and "let the assistant read it", and disconnecting deletes the stored credentials.
- Fast on any device: only the days that changed are downloaded again.
- Same Timeline on desktop and Android and you can drive it by voice.

## 3. What the user sees

### Views (desktop and Android)

| View | What it shows |
|---|---|
| **Day** | Hourly horizontal axis that spans several days. Starts with 2 days on each side of the chosen day and adds 3 more at either edge, up to 30. Current-time marker. Events sit at their real time, with a duration bar when the end time is known. |
| **Week** | Seven days, only non-empty days are loaded. On Android, a swipeable Sunday–Saturday strip with an agenda list for the selected day. |
| **Month** | A 6-week grid with the number of events per day and a coloured dot per category. Tapping a day opens it in Day view. Events are loaded in the background so the day opens instantly. |
| **Custom range** | Pick a start and end date in a two-month calendar (up to 31 days). |

Common controls: previous/next, date badge with a range picker, refresh, and a category filter row with the categories that have data (Finance, Activity, Entertainment, Work, Personal).

### Event detail

Tapping an event opens a sheet with the title, a short summary, the exact time (or only the date when the source only knew the date, labelled "day precision"), all the details for that kind of event (amount, merchant, place, game, artist, and so on), and the list of **evidence** (source type and source id).

### Voice and assistant

- Voice command "show my spending last week" opens Timeline filtered to spending, with an optional merchant or category (for example "at Cafe"). Ranges understood: today, yesterday, this week, last week, or a custom date range of up to one year. The filter shows a "Spending · dates · time zone" bar with "Clear filter" and pages 30 entries at a time.
- The assistant can read your timeline (`query_timeline_events`, at most 20 entries per page and a 366-day window), list the kinds of events (`list_timeline_types`), save a factual event with its evidence (`save_timeline_event`), and register a new kind of event (`create_timeline_event_type`). For totals and statistics it uses the Pulse measurements instead of adding things up itself. It also has `list_visits` and `show_on_map` for places.
- Android registers as a device with app control and supports the same voice flow as desktop (navigation and spending filters). Desktop terminal commands are not part of Timeline.

### Live updates

New data appears without a manual refresh. Core announces "timeline updated"; the app waits 350 ms, re-checks the day summaries and reloads only the days that changed. It also re-checks when the window comes back to the foreground or the live connection reconnects.

## 4. Where events come from

| Source | What becomes an event | Status |
|---|---|---|
| **Gmail** (live sync) | Receipts, bills, bank alerts, statements, orders, deliveries, appointments found in your mail. New mail is picked up through Gmail's change feed, with a reconciliation sweep and renewal of the watch. A Gmail account must be connected with "Timeline sync" on. | Built |
| **Gmail history import** | Choose a date range (up to 366 days), review candidate emails (3 per page) with the events found in each, then import only the ones you pick. | Built |
| **PDF attachments** | PDF attachments of useful emails (max 25 MiB) are parsed in the background (bank statements, invoices) and each transaction row becomes an event. The file is kept 7 days at most and then expires. | Built |
| **Google Calendar** | Meetings and appointments (`appointment`). Title, time and status are managed by Google, notes stay local. Deleted calendar events are retracted. | Built |
| **PlayStation** | Game sessions (`gaming`). PlayStation only exposes lifetime counters, so an increase creates an event with an observation window and unknown exact session times. The app says so. | Built |
| **Spotify** | Latest 50 plays, refreshed every 15 minutes (`music`). Limited window, not a full history. | Built |
| **YouTube (API)** | Likes and playlist additions (`video_like`, `video_playlist_addition`). They never claim a watch. | Built |
| **YouTube watch history** (Google Takeout upload) | Real watched videos with the actual timestamp (`video_watch`), up to 20,000 per request. Search, like, deleted and malformed rows are skipped. | Built |
| **Google Maps Timeline** (Takeout upload) | Place visits (`visit`) and travel segments (`travel`) with start, end, distance. Old and new export formats are understood. Zip files up to 100 MB. | Built |
| **Swiggy** | Food orders (`order`) with status. Read-only, no ordering. Only recognised statuses with a real timestamp become events. | Built |
| **Assistant / voice** | The assistant can save an event you tell it about, with evidence. | Built |

Not supported for now (do not promise): SMS capture, Health and fitness data, live GPS location, Steam, Valorant, Amazon shopper data, rider live tracking on a map for food delivery, a full Spotify history, a full YouTube watch history from the API.

### Import guarantees

- Honest times. A source that only knows a date stores a date (`day` precision). The email delivery time is never used as the event date.
- Honest facts. No amount, currency, date or duration is guessed. If the extractor is unsure it returns nothing and the email is flagged for review instead.
- Money is only counted as spending when it is a confirmed debit. Refunds, bills, statements, transfers, card repayments and income are not spending.
- Secrets are not stored. OTPs, passwords, PINs, CVVs and full card numbers are excluded from extraction and redacted from stored email subject and snippet.
- Re-importing is safe. Each event has a stable `dedupe_key` and each piece of evidence has a hash, so the same thing is never added twice.

## 5. What an event looks like

### Categories and kinds of events

The system defines six fixed categories (each with its own colour and icon) and 19 built-in kinds. Health (kind: workout) exists in the schema but is not supported: no source feeds it and it is not offered. The table lists what is supported:

| Category | Colour / icon | Kinds |
|---|---|---|
| **Finance** | emerald, wallet | transaction, refund, transfer, bill, statement, repayment |
| **Activity** | orange, map-pin | order, delivery, visit, travel |
| **Entertainment** | purple, tv | music (listen), gaming (game session), video watch, video like, playlist addition |
| **Work** | blue, briefcase | appointment, work task |
| **Personal** | amber, user | personal event (log / diary entry) |

Each kind has a published schema for its details (amount, total, currency, direction, spending flag, merchant, reference, account hint, duration, distance, calories, game, platform, artist, album, track, channel, restaurant, status, place, activity type, timing) and an **analytics definition** that tells Pulse what it can chart (for example transactions → spending sum by merchant, music → plays and listening time, gaming → sessions and play time, video → views and watch time, orders → count and spend).

Users and the assistant can add their own kinds. A published kind cannot be edited afterwards, and a new version is created instead, so old events always match the rules they were saved under.

### Fields on an event

- `title`, optional `summary`
- `occurred_at` and optional `ended_at` (end cannot be before start)
- `time_precision`: year, month, day, hour, minute, second or millisecond
- `source_timezone`
- `content`: the kind-specific details, checked against the kind's schema (up to 150 fields)
- `confidence` (0 to 1): sources that read free text, such as email, use 0.8; structured sources use 1.0
- `record_state`: `active`, `superseded` or `retracted`. Only `active` events are shown.
- `dedupe_key`: unique per user
- `revision`: goes up each time the event changes

### Evidence

Many per event: source type (gmail, google_takeout, source_attachment, google_calendar, playstation, ...), source id, optional raw reference (for example the Takeout file name), optional links to the stored source record or attachment, and metadata. One evidence row is unique per event by its content hash.

### Money handling (finance)

After an event is saved, the finance step:

1. Computes a fingerprint from the reference (or account hint plus merchant), amount, direction and the day (India time).
2. If the same fingerprint already exists, the new event is marked `superseded`, its evidence moves to the original, and the original keeps a `duplicate_count`.
3. Marks `is_spending`, `is_refund`, `is_statement`, `is_repayment`, `is_due`, `is_transfer` and a signed `net_amount` (refunds negative).
4. When a payment or repayment has the same reference and currency as an earlier bill or due notice, that bill is marked settled (`settled_by_event_id`, `settled_at`).

Currencies are never mixed. Ambiguous symbols such as "Rs" or "$" without a clear currency go to review instead of being guessed.

## 6. How events are stored (Postgres)

| Table | What it holds |
|---|---|
| `timeline_groups` | The six categories. |
| `timeline_event_types` | The kinds. Global (no owner) or per user. Each has a group, a JSON Schema for details, an analytics definition, a UI hint, a version and a state (draft, published, deprecated). Published schemas cannot be edited (database trigger). |
| `timeline_events` | The events. One row each. A trigger checks that a user only uses their own custom kinds. |
| `timeline_evidence` | Where an event came from. Many per event. |
| `timeline_revisions` | One counter per user. It goes up whenever any of that user's events change. |

Events are not stored in time order. Indexes on user + time, user + category + time and user + kind + time return the newest first without sorting work. Everything is deleted with the user (cascade).

## 7. How the apps ask for data

All calls go through Core's authenticated HTTP API.

| Call | Purpose |
|---|---|
| `GET /v1/timeline/groups`, `GET /v1/timeline/event-types` | Static lists. Loaded once. Kinds can be filtered by category. |
| `POST /v1/timeline/event-types` | Publish a new kind. |
| `POST /v1/timeline/events/days` | **Day summaries** for a range (max 366 days): per day a count, per-category counts and a **revision**. No events inside. Fast. |
| `POST /v1/timeline/events/query` | The events for a range. **30 per page**, newest first. Cursor paging, so new events never shift later pages. Filters: category, kind, date window, record state, spending only, merchant. Filters are applied before paging. |
| `POST /v1/timeline/events/counts` | Older per-day-per-category counts. Still available. |
| `POST /v1/timeline/events` | Save one event with evidence. |
| `POST /v1/connectors/google/takeout/upload` | Takeout upload (YouTube history, Maps Timeline). |
| `POST /v1/connectors/gmail/history/page` and `/import` | Review and import old mail. |

The day revision is a short hash of the ids and revisions of that day's active events. If a day's revision is the same as last time, nothing on that day changed. Invalid ranges, unknown time zones and oversized filters are rejected.

## 8. How the apps cache (desktop and Android)

The unit of everything is **one day**.

1. **Summaries first.** For the days on screen, one `days` call.
2. **Skip empty days.** Count 0 means no events call at all.
3. **Reuse unchanged days.** If a day's revision matches the cached one, show the cache and make no call. Otherwise fetch that day's first 30 events (up to 6 days in parallel).
4. **Cache layers.**
   - In memory: a day store holds counts and events per day and per category filter.
   - On disk (desktop): `~/.vox/cache/<user id>/timeline.json`, saved about 1.5 s after a change, newest 90 days of events. Private folder (permissions 700).
   - On disk (Android): `noBackupFilesDir/timeline/<hashed user id>.json`, saved after 1.5 s of inactivity, newest 90 day/filter entries and 800 summaries. Not included in device backups. Snapshots from a different time zone are rejected.
   - On the next start the cached days show immediately, then the summaries call decides what to refresh.
   - Sign-out deletes the cache and clears the in-memory stores. A session generation counter stops late responses from bringing cleared data back.
5. **Live updates.** See section 3.

What each view fetches:

- **Day:** pages 30 events per day as you scroll, and asks for the next page when you approach the oldest loaded event. Android also keeps explicit per-day paging buttons.
- **Week:** summaries for 7 days, then only the non-empty days, in parallel.
- **Month:** summaries only for the 6-week grid (counts and category dots), then events prefetched in the background, 7 days at a time (3 requests at a time on Android).
- **Voice or filter results** (for example "my spending last week") skip the day cache and query directly, 30 at a time.

## 9. When a new event arrives

The server stores it and the index keeps it in order. The app does not insert it locally. The day's revision changes, the next summaries call shows that, and the day is refetched. This is always correct and cheap, because a day is at most one 30-event page.

## 10. Privacy, safety and control

- **Consent first.** Linking an account needs explicit consent to Timeline sync and assistant reads. The two switches are independent and can be paused at any time. Resuming a paused PlayStation connection starts a new baseline and does not import what happened while paused.
- **Disconnect** deletes the stored credentials. Events already in Timeline are kept.
- **Credentials** are encrypted at rest and bound to the user and the connector. Provider tokens never reach the desktop or phone app.
- **Tenant isolation.** Every table is keyed by user. Foreign keys include the user id, so evidence can never point at another user's event.
- **Evidence for every event.** The product separates what was read from a source from what was inferred (confidence, precision, and `timing` markers such as "known interval" or "provider timestamp").
- **No made-up data.** No date, duration, currency or amount is invented (see "Import guarantees").
- **Personal files stay local.** Product principle for files on the device; only the excerpts a step needs go to a model. Email and PDF text is sent to the extraction model for parsing (the extractor instructions tell it to ignore any instructions inside the document).
- **Retention.** PDF attachments expire after 7 days.
- **Device cache.** Plain JSON in a private, non-backup location, wiped at sign-out. Encrypting it is a possible next step.

## 11. Limits and things to know

- Day boundaries use the device time zone, and that same time zone is sent to the server. Daylight-saving days work, but the cache is only valid for that zone.
- Revisions are per user, not per source, but day revisions stay precise, so only changed days reload.
- Timeline shows what connected sources report. It is **not complete history**: Spotify gives the last 50 plays, YouTube API gives likes and playlists, PlayStation gives counters, Takeout is a snapshot of when you exported. The product labels these partial.
- Gaming sessions from PlayStation have unknown start and end times.
- Delivery and order events appear only when the provider gives a real timestamp and a recognised status.
- Email extraction can decline: if the model is unsure, nothing is added and a low-priority notice asks you to review the email.
- The Android Timeline follows the same day-cache flow as desktop (`vox-android/docs/timeline.md`).
- The new endpoints (`days`, `query` with spending filters) are not yet in `contracts/openapi.json` or the desktop's `api.gen.ts`.

## 12. Safe things to say on the marketing site

Backed by the code:

- "Your life, one day at a time, built from the apps you connect."
- Day, week and month views; filter by Finance, Activity, Entertainment, Work, Personal.
- Every entry shows where it came from.
- Duplicate bank messages and emails are merged into one entry.
- Ask by voice: "show my spending last week".
- Works on desktop and Android and updates live.
- You decide per account whether it feeds Timeline or the assistant, and you can pause either.
- Sources today: Gmail (receipts, bills, statements, PDFs), Google Calendar, PlayStation, Spotify, YouTube, Google Maps Timeline and YouTube history via Google Takeout, Swiggy.

Do not claim:

- A complete history of anything, live GPS tracking, rider tracking, SMS capture, Health or fitness data, Steam, Amazon, or WhatsApp as a Timeline source.
- Specific numbers for accuracy or speed, user counts or testimonials. None are on file (see `vox-web/PRODUCT.md`: no invented social proof).

## 13. Where the code lives

- Core: `services/api/routes/timeline.rs`, `routes/takeout.rs`, `routes/gmail.rs`; `src/storage/timeline.rs`, `src/domain/timeline.rs`, `src/application/timeline.rs`; `src/agents/tools/timeline.rs`; `src/gmail_sync.rs`, `src/document_extraction.rs`, `src/attachments/processor.rs`, `src/finance_normalization/`, `src/fresh_connections.rs`; migration `20261009000004_timeline.sql`; Pulse reads in `src/storage/pulse.rs`.
- Connections: `vox-connections/src/providers/` (gmail, google_calendar, playstation, personal for Spotify and YouTube, swiggy); docs `vox-connections/docs/`.
- Desktop: `src/features/timeline/` (`day-store.ts`, `api.ts`, `filters.ts`), `src/hooks/use-timeline-days.ts`, `src/components/timeline-view.tsx`, `planning-timeline.tsx`, `span-month-counts.tsx`, `spans/timeline-header.tsx`, `src/features/persist.ts`, `src-tauri/src/device_cache.rs`.
- Android: `app/src/main/java/in/voxagent/mobile/timeline/` (`TimelineApi`, `TimelineDayStore`, `TimelinePersistence`, `TimelineViewModel`, `TimelineScreen`, `PlanningTimeline`, `MonthGrid`), `control/AppCommandExecutor.kt`; docs `vox-android/docs/timeline.md`.
- Related docs: `product-vision.md`, `connections.md`, `playstation.md`, `vox-connections/docs/personal-integrations.md`, `vox-connections/docs/food-connections.md`, `docs/superpowers/specs/2026-09-29-pulse-analytics-design.md`.
