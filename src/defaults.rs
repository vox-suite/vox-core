//! Deployment-curated defaults. Publication is explicit and idempotent; no user installation is implied.
use crate::skills::{PublishSkillRequest, SkillError, SkillService};
pub fn skills() -> Result<Vec<PublishSkillRequest>, SkillError> {
    [
        include_str!("../defaults/skills/summarize-actions/SKILL.md"),
        include_str!("../defaults/skills/writing-assistant/SKILL.md"),
        include_str!("../defaults/skills/plan-my-day/SKILL.md"),
        include_str!("../defaults/skills/meeting-prep/SKILL.md"),
        include_str!("../defaults/skills/compare-options/SKILL.md"),
        include_str!("../defaults/skills/create-a-skill/SKILL.md"),
    ]
    .into_iter()
    .map(|source| {
        crate::skill_format::import(&std::collections::BTreeMap::from([(
            "SKILL.md".into(),
            source.into(),
        )]))
    })
    .collect()
}
pub async fn publish(pool: sqlx::PgPool, deployment: &str) -> Result<usize, SkillError> {
    let service = SkillService::new(pool);
    let defaults = skills()?;
    let count = defaults.len();
    for skill in defaults {
        service.publish_curated(deployment, skill).await?;
    }
    Ok(count)
}
#[cfg(test)]
mod tests {
    #[test]
    fn defaults_are_valid_and_have_no_capability_authority() {
        let skills = super::skills().unwrap();
        assert_eq!(skills.len(), 6);
        assert!(skills.iter().all(|s| s.requested_capabilities.is_empty()));
    }
}
