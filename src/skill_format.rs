//! Portable, declarative Agent Skills. Imported files are data, never executable code.
use crate::skills::{PublishSkillRequest, SkillError, validate};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    name: String,
    description: String,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: String,
}

pub fn import(files: &BTreeMap<String, String>) -> Result<PublishSkillRequest, SkillError> {
    if files.len() > 32 || files.values().map(String::len).sum::<usize>() > 48_000 {
        return Err(SkillError::Invalid);
    }
    let source = files.get("SKILL.md").ok_or(SkillError::Invalid)?;
    let source = source.replace("\r\n", "\n");
    let rest = source.strip_prefix("---\n").ok_or(SkillError::Invalid)?;
    let (header, body) = rest.split_once("\n---\n").ok_or(SkillError::Invalid)?;
    if header.len() > 8192 {
        return Err(SkillError::Invalid);
    }
    let fields: Frontmatter = yaml_serde::from_str(header).map_err(|_| SkillError::Invalid)?;
    if fields.name.len() > 64
        || fields.name.starts_with('-')
        || fields.name.ends_with('-')
        || fields.name.contains("--")
    {
        return Err(SkillError::Invalid);
    }
    let mut resources = serde_json::Map::new();
    for (path, content) in files {
        if path == "SKILL.md" {
            continue;
        }
        let safe = path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..");
        if !safe
            || path.starts_with('/')
            || path.contains(['\\', ':', '\0'])
            || !(path.starts_with("references/") || path.starts_with("assets/"))
            || ![".md", ".txt", ".json"]
                .iter()
                .any(|ext| path.ends_with(ext))
            || content.len() > 16_384
        {
            return Err(SkillError::Invalid);
        }
        resources.insert(path.clone(), json!(content));
    }
    let title = fields
        .metadata
        .get("vox.title")
        .cloned()
        .unwrap_or_else(|| fields.name.replace('-', " "));
    resources.insert("vox.metadata.json".into(),json!(json!({"license":fields.license,"compatibility":fields.compatibility,"metadata":fields.metadata}).to_string()));
    let skill = PublishSkillRequest {
        external_key: fields.name,
        title,
        summary: fields.description,
        instructions: body.trim().to_owned(),
        requested_capabilities: fields
            .allowed_tools
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        resources: Value::Object(resources),
    };
    validate(&skill)?;
    // Markdown resource links must resolve within the imported bundle. Web
    // URLs in prose are guidance, but fetched/executable resources are not.
    for part in skill.instructions.split("](").skip(1) {
        let target = part
            .split(')')
            .next()
            .ok_or(SkillError::Invalid)?
            .split_whitespace()
            .next()
            .ok_or(SkillError::Invalid)?;
        if !target.starts_with('#') && !files.contains_key(target) {
            return Err(SkillError::Invalid);
        }
    }
    Ok(skill)
}

pub fn export(skill: &PublishSkillRequest) -> Result<BTreeMap<String, String>, SkillError> {
    validate(skill)?;
    let mut files = BTreeMap::new();
    let name = serde_json::to_string(&skill.external_key).map_err(|_| SkillError::Invalid)?;
    let description = serde_json::to_string(&skill.summary).map_err(|_| SkillError::Invalid)?;
    let tools = serde_json::to_string(&skill.requested_capabilities.join(" "))
        .map_err(|_| SkillError::Invalid)?;
    let provenance: Value = skill
        .resources
        .get("vox.metadata.json")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(json!({}));
    let mut header =
        format!("---\nname: {name}\ndescription: {description}\nallowed-tools: {tools}\n");
    for field in ["license", "compatibility"] {
        if let Some(value) = provenance.get(field).and_then(Value::as_str) {
            header.push_str(&format!(
                "{field}: {}\n",
                serde_json::to_string(value).map_err(|_| SkillError::Invalid)?
            ));
        }
    }
    let mut metadata: BTreeMap<String, String> = provenance
        .get("metadata")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    metadata.insert("vox.title".into(), skill.title.clone());
    header.push_str("metadata:\n");
    for (key, value) in metadata {
        header.push_str(&format!(
            "  {}: {}\n",
            serde_json::to_string(&key).map_err(|_| SkillError::Invalid)?,
            serde_json::to_string(&value).map_err(|_| SkillError::Invalid)?
        ));
    }
    files.insert(
        "SKILL.md".into(),
        format!("{header}---\n\n{}\n", skill.instructions),
    );
    for (path, value) in skill.resources.as_object().ok_or(SkillError::Invalid)? {
        if (path.starts_with("references/") || path.starts_with("assets/"))
            && let Some(text) = value.as_str()
        {
            files.insert(path.clone(), text.to_owned());
        }
    }
    // Use the same path/content checks for export; never emit an unsafe archive.
    import(&files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_multiline_guidance_without_granting_tools_and_rejects_executables() {
        let mut files=BTreeMap::from([("SKILL.md".into(),"---\nname: meeting-prep\ndescription: >\n  Prepare an agenda\n  from supplied notes.\nallowed-tools: calendar.read\n---\n\nReview the supplied notes.\n".into())]);
        let skill = import(&files).unwrap();
        assert_eq!(skill.requested_capabilities, vec!["calendar.read"]);
        assert_eq!(skill.instructions, "Review the supplied notes.");
        files.insert("scripts/run.py".into(), "print('run')".into());
        assert!(import(&files).is_err());
        files.remove("scripts/run.py");
        files.insert("references/../../secret.txt".into(), "secret".into());
        assert!(import(&files).is_err());
    }
}
