-- An OAuth callback must complete against the exact remote extension version
-- and endpoint that began the browser flow. Existing short-lived sessions
-- cannot be safely bound retroactively, so require a fresh authorization.
ALTER TABLE mcp_authorization_sessions
    ADD COLUMN endpoint_url TEXT,
    ADD COLUMN extension_version INTEGER;

DELETE FROM mcp_authorization_sessions;

ALTER TABLE mcp_authorization_sessions
    ALTER COLUMN endpoint_url SET NOT NULL,
    ALTER COLUMN extension_version SET NOT NULL,
    ADD CONSTRAINT mcp_authorization_sessions_version_positive
        CHECK (extension_version > 0);
