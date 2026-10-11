# Timeline

## What it is

Timeline is a day-by-day log of what happened in your life, built automatically from the apps you connect. A purchase email, a place you visited, a game you played, a photo you shared: each becomes one **event** with a time, a title and some details. You scroll through your days instead of through ten different apps.

## Why we have it

- One place to answer "what did I do on Tuesday?" or "what did I spend this week?".
- It is the shared source for everything else. **Pulse** builds its charts from these events, and the assistant reads them when you ask questions.
- Every event keeps a link to where it came from (the evidence), so you can always check why it is there.

## How it works, in short

1. A source (Gmail, Google Takeout, attachments, a voice command, and so on) produces an event.
2. Core checks it, drops duplicates, and saves it to Postgres together with its evidence.
3. The desktop app asks Core for the days it is showing, keeps the answers in memory and on disk, and only re-asks for days that changed.

## How events are stored (Postgres)

| Table | What it holds |
|---|---|
| `timeline_groups` | The six broad categories: finance, activity, entertainment, work, health, personal. |
| `timeline_event_types` | The kinds of event ("transaction", "visit", ...). Each type belongs to a group and has a schema for its details. Published types cannot be edited. |
| `timeline_events` | The events. One row each. |
| `timeline_evidence` | Where an event came from (email id, attachment, raw reference). Many per event. |
| `timeline_revisions` | One counter per user. It goes up whenever any of that user's events change. |

Main fields on an event:

- `occurred_at` and optional `ended_at`: when it happened.
- `time_precision`: how exact the time is (year, month, day, hour, minute, second). A receipt that only has a date is stored as `day`.
- `content`: the type-specific details (amount, merchant, place, ...), validated against the type's schema.
- `record_state`: `active`, `superseded` or `retracted`. Only `active` events are shown.
- `dedupe_key`: unique per user. Importing the same thing twice does not create two events.
- `revision`: goes up each time the event is updated.

Events are not stored in time order. Postgres keeps them sorted through the index on `(user_id, occurred_at DESC)`, so reads come back newest first without sorting work.

## How the app asks for data

All calls are made by the desktop app through Core's HTTP API.

| Call | Purpose |
|---|---|
| `GET /v1/timeline/groups`, `GET /v1/timeline/event-types` | Static lists. Loaded once. |
| `POST /v1/timeline/events/days` | **Day summaries** for a date range (max 366 days): per day a count, per-category counts and a **revision**. No events inside. Fast. |
| `POST /v1/timeline/events/query` | The events for a range. **30 per page**, newest first. Paging uses a cursor made of the last event's time and id, so new events never shift later pages. |
| `POST /v1/timeline/events/counts` | Older per-day-per-category counts. Still available. |

The day revision is a short hash of the ids and revisions of that day's active events. If a day's revision is the same as last time, nothing on that day changed.

## How the app caches (desktop)

The unit of everything is **one day**.

1. **Summaries first.** For the days on screen, one `days` call.
2. **Skip empty days.** Count 0 means no events call at all.
3. **Reuse unchanged days.** If a day's revision matches the cached one, show the cache and make no call. Otherwise fetch that day's first 30 events (up to 6 days in parallel).
4. **Cache layers.**
   - In memory: `day-store.ts` holds counts and events per day and scope (all, or one group).
   - On disk: the store is saved to `~/.vox/cache/<user id>/timeline.json` (about 1.5 s after a change, newest 90 days of events). On the next start the cached days show immediately, then the summaries call decides what to refresh.
   - The folder is private to the user (permissions 700) and is deleted on sign-out.
5. **Live updates.** When Core says "timeline updated", the app waits 350 ms, fetches the summaries again, compares revisions, and reloads only the days that changed. It also does this when the window becomes visible again.

What each view does:

- **Day:** an hourly axis that spans several days. It starts with 2 days on each side of the chosen day and adds 3 more when you scroll to either edge (up to 30). Each day pages 30 events at a time as you scroll.
- **Week:** summaries for 7 days, then only the non-empty days, in parallel.
- **Month:** summaries only for the 6-week grid, which is enough to draw counts and category dots. Events are prefetched in the background, 7 days at a time, so opening a day is instant.
- **Voice or filter results** (for example "my spending last week") skip the day cache and query directly, 30 at a time.

## When a new event arrives

The server stores it and the index keeps it in order. The app does not insert it locally. The day's revision changes, the next summaries call shows that, and the day is refetched. This is always correct and cheap enough because a day is at most one 30-event page.

## Limits and things to know

- Day boundaries use the device time zone. The same time zone is sent to the server, so daylight-saving days work, but the cache is only valid for that zone.
- The disk cache is plain JSON in the user's home folder. It holds event titles and details, so it is wiped at sign-out; encrypting it is a possible next step.
- Revisions are per user, not per source. Any change to an event bumps `timeline_revisions`, but the day revisions stay precise, so only changed days reload.
- The new endpoints are not yet in `contracts/openapi.json` or the desktop's `api.gen.ts`.

## Where the code lives

- Core: `services/api/routes/timeline.rs`, `src/storage/timeline.rs`, `src/domain/timeline.rs`, migration `20261009000004_timeline.sql`.
- Desktop: `src/features/timeline/day-store.ts`, `src/hooks/use-timeline-days.ts`, `src/components/timeline-view.tsx`, `src/components/planning-timeline.tsx`, `src/features/persist.ts`, `src-tauri/src/device_cache.rs`.
