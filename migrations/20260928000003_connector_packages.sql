-- Immutable deployment-reviewed connector packages. Installations carry no authority.
CREATE TABLE connector_packages (
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id),
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    digest TEXT NOT NULL,
    manifest JSONB NOT NULL,
    review JSONB NOT NULL CHECK (jsonb_typeof(review)='object' AND review <> '{}'::jsonb),
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (deployment_id, external_key, version)
);
CREATE INDEX connector_packages_catalog ON connector_packages(deployment_id, external_key, version DESC) WHERE enabled;
CREATE TABLE connector_package_installations (
    extension_id UUID PRIMARY KEY REFERENCES remote_extensions(id),
    deployment_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (deployment_id,external_key,version) REFERENCES connector_packages(deployment_id,external_key,version)
);
CREATE INDEX connector_package_installations_version ON connector_package_installations(deployment_id,external_key,version);
