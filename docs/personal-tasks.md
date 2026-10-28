# Personal task API

`POST /v1/personal-tasks` creates an ordinary Timeline span for a human to-do. The existing authenticated Actor supplies ownership; clients cannot supply user IDs or request autonomous execution. No agent, scheduled reminder, cloud model, transcript upload, or audio processing is invoked by this service.

```json
{
  "request_id": "request-6d5e10f9",
  "title": "Send the proposal",
  "notes": "Include the revised estimate.",
  "due_at": null,
  "source_links": []
}
```

Keep the same request ID and original body across retries. New intentional requests use new IDs, even when their titles are identical. IDs accept 1–200 ASCII letters/digits plus `-`, `_`, and `:`. Titles are trimmed and limited to 200 characters; notes to 16,000 bytes. Unknown execution/ownership fields are rejected. Dates are optional RFC3339 timestamps with an explicit offset; no date or start time is fabricated for an untimed task.

A successful response is `{ "task": <Span>, "created": true|false }`: HTTP 201 for a new task, 200 for an unchanged retry, 409 if the same owner/request ID is reused for changed input, and 410 if the original task was deleted. Returned tasks always begin as `planned`, category `task`, execution type `manual_human`, and source `personal_task`. Subsequent edits/status changes are preserved on retries. Existing authenticated `/v1/spans/list`, `/{id}/update`, and `/{id}/delete` APIs own listing, editing, completing/cancelling, and deleting these same canonical tasks.

Optional `source_links` is bounded metadata: up to eight `{ "kind": "meeting"|"event", "id": "UUID", "evidence_ids": ["opaque segment reference"] }` objects, each with up to 32 evidence references of 256 bytes. Local meeting links are opaque; they do not establish assignment, authorize access to another object, or trigger retrieval. The future meeting-extraction layer must validate local evidence/assignee ownership and preserve stable request IDs before it calls this service. No separate meeting task table is introduced.

The receipt table stores owner/request ID, a normalized-payload hash, and a nullable span reference. Unique insertion serializes concurrent requests within the creation transaction. Span deletion clears its reference while retaining the receipt, so extraction/outbox retries cannot resurrect it. User/account deletion cascades the receipt. Receipt writes are service-only with RLS enabled; the API never accepts a raw receipt write. Task creation and receipt update commit together; failed transactions leave neither.

## Verification and delivery boundary

Five focused tests pass, including the explicitly enabled integration test against a disposable local PostgreSQL 17 + pgvector container. That integration runs all repository migrations and tests eight concurrent retries, owner isolation, untimed tasks, shared span listing, edits/completion surviving replay, changed-payload conflict, deletion tombstones, and a new intentional request after deletion. Validation tests cover offsets, size bounds, control characters, and rejection of execution fields. API compile and generated OpenAPI checks pass. Authentication middleware registration was source-reviewed; a production HTTP session was not exercised.

This is the reusable backend dependency. Desktop summary/extraction, candidate review/promotion, durable local outbox/sync, and adoption by existing unrelated agent tools are not implemented in this slice. The source change does not establish that the migration or endpoint has been deployed.

To reproduce on a dedicated test database only:

```sh
INTEGRATION_TEST_DATABASE_URL=postgres://... cargo test --lib application::personal_tasks --locked -- --include-ignored
cargo check --bin vox-core-api --locked
```

Bootstrap Supabase-compatible local roles/functions with `scripts/ci-supabase-bootstrap.sql` before running the database test. Never point this suite at production.
