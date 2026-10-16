-- Match both list sort keys and omit dropped Plans from list indexes.
CREATE INDEX spaces_active_created_cursor_idx
    ON spaces (user_id, created_at DESC, id DESC)
    WHERE state != 'dropped';

CREATE INDEX spaces_active_updated_cursor_idx
    ON spaces (user_id, updated_at DESC, id DESC)
    WHERE state != 'dropped';
