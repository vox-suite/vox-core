-- Model-inferred conversation confirmations are retired. Consequential
-- connector actions must use the authenticated proposal/approval path.
DROP TABLE IF EXISTS connected_app_pending_actions;
-- The generic connection initiate/callback endpoints never had a provider
-- exchange; remove their unused state store with those endpoints.
DROP TABLE IF EXISTS connection_authorization_sessions;
ALTER TABLE remote_extension_credentials DROP COLUMN IF EXISTS last_used_at;
