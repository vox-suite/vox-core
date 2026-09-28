-- Earlier package declarations lack pinned metadata and cannot retain authority.
ALTER TABLE connector_packages ADD COLUMN metadata JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE connector_packages ALTER COLUMN metadata DROP DEFAULT;
UPDATE connector_packages SET enabled=false;
UPDATE remote_extensions e SET lifecycle_state='disabled',operator_enabled=false FROM connector_package_installations i WHERE i.extension_id=e.id;
DELETE FROM remote_extension_credentials c USING connector_package_installations i WHERE i.extension_id=c.extension_id;
DELETE FROM mcp_authorization_sessions o USING connector_package_installations i WHERE i.extension_id=o.extension_id;
UPDATE external_connections x SET authorization_state='revoked' FROM connector_package_installations i WHERE i.extension_id=x.remote_extension_id;
UPDATE agent_capability_grants g SET state='revoked',revoked_at=now() FROM external_connections x JOIN connector_package_installations i ON i.extension_id=x.remote_extension_id WHERE g.connection_id=x.id;
