-- Full reviewed skill content is immutable and addressed by its digest.
ALTER TABLE skill_package_versions ADD COLUMN title TEXT;
ALTER TABLE skill_package_versions ADD COLUMN summary TEXT;
ALTER TABLE skill_package_versions ADD COLUMN digest TEXT;
UPDATE skill_package_versions v SET title=s.title,summary=s.summary FROM skill_packages s WHERE s.id=v.skill_id;
ALTER TABLE skill_package_versions ALTER COLUMN title SET NOT NULL;
ALTER TABLE skill_package_versions ALTER COLUMN summary SET NOT NULL;
-- Historical versions remain evidence; they need new publication and review before loading.
CREATE TABLE connector_skill_installations (
    extension_id UUID NOT NULL REFERENCES remote_extensions(id),
    skill_id UUID NOT NULL REFERENCES skill_packages(id),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id),
    version INTEGER NOT NULL,
    PRIMARY KEY(extension_id,skill_id),
    FOREIGN KEY(skill_id,version) REFERENCES skill_package_versions(skill_id,version)
);
ALTER TABLE skill_installations ADD COLUMN independently_installed BOOLEAN NOT NULL DEFAULT true;
