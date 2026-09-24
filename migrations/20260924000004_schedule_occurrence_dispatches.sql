-- An occurrence may request an external effect only once. Unknown outcomes need
-- reconciliation; blindly retrying could place a second phone call.
CREATE TABLE schedule_occurrence_dispatches (
    schedule_id UUID NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    occurrence_at TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('claimed','dispatched','unknown')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (schedule_id, occurrence_at)
);
