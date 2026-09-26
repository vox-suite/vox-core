CREATE TABLE skill_packages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE CASCADE,
    owner_user_context_id UUID REFERENCES user_contexts(id) ON DELETE CASCADE,
    external_key TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    latest_version INTEGER NOT NULL DEFAULT 1 CHECK (latest_version > 0),
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX skill_packages_curated_key
    ON skill_packages (deployment_id, external_key)
    WHERE owner_user_context_id IS NULL;
CREATE UNIQUE INDEX skill_packages_private_key
    ON skill_packages (owner_user_context_id, external_key)
    WHERE owner_user_context_id IS NOT NULL;

CREATE TABLE skill_package_versions (
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    instructions TEXT NOT NULL,
    requested_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    resources JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (skill_id, version)
);

CREATE TABLE skill_installations (
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    installed_version INTEGER NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_context_id, skill_id),
    FOREIGN KEY (skill_id, installed_version)
        REFERENCES skill_package_versions(skill_id, version)
);
