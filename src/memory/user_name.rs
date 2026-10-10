use super::{MemoryService, active_actor};
use crate::identity::ResourceOwner;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use std::sync::OnceLock;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum UserNameError {
    #[error(
        "provide a name of at most 120 characters using letters, spaces, periods, apostrophes or hyphens"
    )]
    InvalidName,
    #[error("quote the user's own name statement from the current message, at most 512 characters")]
    InvalidEvidence,
    #[error("user name storage failed")]
    Storage(#[from] sqlx::Error),
}

pub struct UserNameStatement {
    name: String,
    evidence_quote: String,
}

impl UserNameStatement {
    pub fn new(name: &str, evidence_quote: &str, user_text: &str) -> Result<Self, UserNameError> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        static NAME_PATTERN: OnceLock<Regex> = OnceLock::new();
        let pattern =
            NAME_PATTERN.get_or_init(|| Regex::new(r"\A\p{L}[\p{L}\p{M} .'’‐\-]*\z").unwrap());
        if name.chars().count() > 120 || !pattern.is_match(&name) {
            return Err(UserNameError::InvalidName);
        }
        let quote = evidence_quote.trim();
        if quote.is_empty() || quote.chars().count() > 512 || !user_text.contains(quote) {
            return Err(UserNameError::InvalidEvidence);
        }
        let normalized_quote = quote.split_whitespace().collect::<Vec<_>>().join(" ");
        let lowered = normalized_quote.to_lowercase();
        let states = [
            "fine", "good", "okay", "ok", "well", "great", "tired", "busy", "happy", "sad",
        ];
        if states.contains(&name.to_lowercase().as_str())
            && !lowered.contains("my name")
            && !lowered.contains("call me")
            && !lowered.contains("name is")
        {
            return Err(UserNameError::InvalidEvidence);
        }
        let evidence_pattern = Regex::new(&format!(
            r"(?i)(?:^|[^\p{{L}}\p{{M}}\p{{N}}_]){}(?:$|[^\p{{L}}\p{{M}}\p{{N}}_])",
            regex::escape(&name)
        ))
        .map_err(|_| UserNameError::InvalidEvidence)?;
        if !evidence_pattern.is_match(&normalized_quote) {
            return Err(UserNameError::InvalidEvidence);
        }
        Ok(Self {
            name,
            evidence_quote: quote.to_owned(),
        })
    }
}

#[derive(Debug, Serialize)]
pub struct UserNameUpdate {
    pub name: String,
    pub saved: bool,
    pub cache_updated: bool,
}

impl MemoryService {
    pub async fn update_user_name(
        &self,
        owner: ResourceOwner,
        agent_key: &str,
        conversation_id: Uuid,
        statement: UserNameStatement,
    ) -> Result<UserNameUpdate, UserNameError> {
        let mut tx = self.db.pool().begin().await?;
        active_actor(&mut tx, owner, agent_key).await?;
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM conversations WHERE id=$1 AND user_id=$2 \
             AND user_context_id=$3 AND agent_external_key=$4 FOR SHARE",
        )
        .bind(conversation_id)
        .bind(owner.user_id.0)
        .bind(owner.user_context_id.0)
        .bind(agent_key)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
        let source = json!({
            "kind": "user_statement",
            "conversation_id": conversation_id,
            "user_context_id": owner.user_context_id.0,
            "agent_key": agent_key,
            "evidence_quote": statement.evidence_quote,
        });
        sqlx::query_scalar::<_, Uuid>(
            "UPDATE users SET display_name=$2, \
             profile_facts=profile_facts || jsonb_build_object(\
                 'name', $2::text, 'name_source', \
                 jsonb_set($3::jsonb, '{recorded_at}', to_jsonb(now()))), \
             profile_version=profile_version+1, updated_at=now() \
             WHERE id=$1 AND status <> 'disabled' RETURNING id",
        )
        .bind(owner.user_id.0)
        .bind(&statement.name)
        .bind(source)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
        tx.commit().await?;
        let cache_updated = if self.cache.is_some() {
            match self.refresh_minimal_user(owner.user_id).await {
                Ok(()) => true,
                Err(_) => {
                    tracing::warn!(user_id = %owner.user_id.0, "Saved user name; identity cache refresh failed");
                    false
                }
            }
        } else {
            false
        };
        Ok(UserNameUpdate {
            name: statement.name,
            saved: true,
            cache_updated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_user_supplied_names_and_corrections_in_multiple_scripts() {
        for (name, text) in [
            ("  Rahul   Biswakarma  ", "My name is Rahul Biswakarma."),
            ("Jay", "Actually, my name is Jay."),
            ("O’Connor", "My name is O’Connor."),
            ("Jean-Luc", "Call me Jean-Luc."),
            ("राहुल", "मेरा नाम राहुल है।"),
            ("李明", "我叫 李明。"),
            ("Jose\u{301}", "My name is Jose\u{301}."),
        ] {
            let statement = UserNameStatement::new(name, text, text).unwrap();
            assert_eq!(
                statement.name,
                name.split_whitespace().collect::<Vec<_>>().join(" ")
            );
        }
    }

    #[test]
    fn rejects_names_missing_from_current_user_evidence() {
        for (name, quote, text) in [
            ("Rahul", "My name is Rahul", "I'm fine."),
            ("Rahul", "I'm fine.", "I'm fine."),
            ("Ann", "My name is Anna.", "My name is Anna."),
            ("Rahul", "", "My name is Rahul."),
        ] {
            assert!(matches!(
                UserNameStatement::new(name, quote, text),
                Err(UserNameError::InvalidEvidence)
            ));
        }
        let quote = format!("Rahul {}", "x".repeat(512));
        assert!(UserNameStatement::new("Rahul", &quote, &quote).is_err());
    }

    #[test]
    fn rejects_blank_malformed_and_oversized_names() {
        for name in [
            "",
            " ",
            "123",
            "Rahul {}",
            "Rahul\0",
            "😀",
            "Rahul@example.com",
        ] {
            assert!(matches!(
                UserNameStatement::new(name, name, name),
                Err(UserNameError::InvalidName)
            ));
        }
        let name = "a".repeat(121);
        assert!(matches!(
            UserNameStatement::new(&name, &name, &name),
            Err(UserNameError::InvalidName)
        ));
    }
}
