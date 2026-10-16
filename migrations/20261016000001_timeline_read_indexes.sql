-- Match both columns in the desktop's keyset cursor, including date-only ties.
CREATE INDEX timeline_events_active_cursor_idx
    ON timeline_events (user_id, occurred_at DESC, id DESC)
    WHERE record_state = 'active';

CREATE INDEX timeline_events_active_group_cursor_idx
    ON timeline_events (user_id, group_id, occurred_at DESC, id DESC)
    WHERE record_state = 'active';

-- The complete cursor index also serves the existing active time-range reads.
DROP INDEX timeline_events_user_active_occurred_idx;
