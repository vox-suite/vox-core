CREATE TABLE platform_deployments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT platform_deployments_external_key_key UNIQUE (external_key),
    CONSTRAINT platform_deployments_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_apps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_apps_deployment_id_id_key UNIQUE (deployment_id, id),
    CONSTRAINT host_apps_deployment_external_key_key UNIQUE (deployment_id, external_key),
    CONSTRAINT host_apps_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_organizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_organizations_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_organizations_scope_id_key
        UNIQUE (deployment_id, host_app_id, id),
    CONSTRAINT host_organizations_scope_external_key_key
        UNIQUE (deployment_id, host_app_id, external_key),
    CONSTRAINT host_organizations_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE user_contexts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    organization_id UUID,
    host_user_id TEXT NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_contexts_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_organization_fkey
        FOREIGN KEY (deployment_id, host_app_id, organization_id)
        REFERENCES host_organizations(deployment_id, host_app_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_user_id_key UNIQUE (user_id),
    CONSTRAINT user_contexts_host_user_id_not_empty
        CHECK (
            length(btrim(host_user_id)) > 0
            AND octet_length(host_user_id) <= 512
        )
);

CREATE UNIQUE INDEX user_contexts_unorganized_subject_key
    ON user_contexts (deployment_id, host_app_id, host_user_id)
    WHERE organization_id IS NULL;

CREATE UNIQUE INDEX user_contexts_organized_subject_key
    ON user_contexts (deployment_id, host_app_id, organization_id, host_user_id)
    WHERE organization_id IS NOT NULL;

CREATE INDEX user_contexts_scope_idx
    ON user_contexts (deployment_id, host_app_id, organization_id);
