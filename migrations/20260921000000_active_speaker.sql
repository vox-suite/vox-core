ALTER TABLE conversations ADD COLUMN active_user_id uuid REFERENCES users(id);
ALTER TABLE conversations ADD COLUMN verification_state jsonb;
