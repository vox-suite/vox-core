-- Retain specialist delegation and preference consent without retired connector grants.
UPDATE agent_delegation_permissions SET state='revoked',revoked_at=now()
WHERE state='enabled' AND jsonb_array_length(scope->'capabilities')>0;
ALTER TABLE agent_delegation_permissions DROP CONSTRAINT agent_delegation_permissions_scope_check1;
ALTER TABLE agent_delegation_permissions ADD CONSTRAINT agent_delegation_permissions_scope_check1
CHECK(jsonb_typeof(scope->'capabilities')='array' AND jsonb_array_length(scope->'capabilities') BETWEEN 0 AND 32);
