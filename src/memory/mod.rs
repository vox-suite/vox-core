pub mod cache;
pub mod greetings;
pub mod projection;

use crate::{
    db::Db,
    identity::UserId,
    voiceprint::{VoiceSignature, VoiceprintService},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct MemoryService {
    db: Db,
    cache: Option<Arc<dyn cache::ContextCache>>,
    voiceprints: VoiceprintService,
}

impl MemoryService {
    pub fn new(db: Db, cache: Option<Arc<dyn cache::ContextCache>>) -> Self {
        let voiceprints = VoiceprintService::new(db.clone());
        Self {
            db,
            cache,
            voiceprints,
        }
    }

    pub fn cache(&self) -> Option<&Arc<dyn cache::ContextCache>> {
        self.cache.as_ref()
    }

    pub async fn load(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(value)) = cache.get(user_id).await
        {
            return Ok(value);
        }
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
        }
        Ok(value)
    }

    pub async fn get_user_name(&self, user_id: UserId) -> Result<Option<String>, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(name)) = cache.get_user_name(user_id).await
        {
            return Ok(Some(name));
        }

        let name: Option<String> =
            sqlx::query_scalar("SELECT facts->>'name' FROM user_profiles WHERE user_id = $1")
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await?
                .flatten();

        if let Some(ref n) = name
            && let Some(cache) = &self.cache
        {
            let _ = cache.set_user_name(user_id, n).await;
        }

        Ok(name)
    }

    pub async fn set_user_name(&self, user_id: UserId, name: &str) -> Result<(), sqlx::Error> {
        let trimmed = name.trim();
        sqlx::query(
            "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
             VALUES ($1, jsonb_build_object('name', $2::text), 1, now()) \
             ON CONFLICT (user_id) DO UPDATE SET \
             facts = jsonb_set(user_profiles.facts, '{name}', to_jsonb($2::text), true), \
             updated_at = now()",
        )
        .bind(user_id.0)
        .bind(trimmed)
        .execute(self.db.pool())
        .await?;

        if let Some(cache) = &self.cache {
            let _ = cache.set_user_name(user_id, trimmed).await;
            if let Ok(identities) = sqlx::query_as::<_, (String, String)>(
                "SELECT channel, external_id FROM user_contact_points WHERE user_id = $1",
            )
            .bind(user_id.0)
            .fetch_all(self.db.pool())
            .await
            {
                for (channel, external_id) in identities {
                    let _ = cache
                        .set_greeting_name(&channel, &external_id, trimmed)
                        .await;
                }
            }
        }

        Ok(())
    }

    pub async fn get_voice_signature(
        &self,
        user_id: UserId,
    ) -> Result<Option<VoiceSignature>, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(raw)) = cache.get_voice_signature(user_id).await
            && let Some(sig) = VoiceSignature::from_raw(&raw)
        {
            return Ok(Some(sig));
        }

        let sig = self.voiceprints.get_voiceprint(user_id).await?;
        if let Some(ref s) = sig
            && let Some(cache) = &self.cache
        {
            let _ = cache.set_voice_signature(user_id, &s.to_json()).await;
        }
        Ok(sig)
    }

    pub async fn set_voice_signature(
        &self,
        user_id: UserId,
        signature: &VoiceSignature,
    ) -> Result<(), sqlx::Error> {
        if !signature.usable() {
            return Ok(());
        }
        self.voiceprints
            .save_voiceprint(
                user_id,
                signature,
                signature.sample_duration_ms.min(i32::MAX as u64) as i32,
            )
            .await?;

        if let Some(cache) = &self.cache {
            let _ = cache
                .set_voice_signature(user_id, &signature.to_json())
                .await;
        }

        Ok(())
    }

    pub async fn refresh(&self, user_id: UserId) -> Result<(), sqlx::Error> {
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
            if let Ok(Some(Some(name))) = sqlx::query_scalar::<_, Option<String>>(
                "SELECT facts->>'name' FROM user_profiles WHERE user_id = $1",
            )
            .bind(user_id.0)
            .fetch_optional(self.db.pool())
            .await
            {
                let trimmed = name.trim();
                let _ = cache.set_user_name(user_id, trimmed).await;
                if let Ok(identities) = sqlx::query_as::<_, (String, String)>(
                    "SELECT channel, external_id FROM user_contact_points WHERE user_id = $1",
                )
                .bind(user_id.0)
                .fetch_all(self.db.pool())
                .await
                {
                    for (channel, external_id) in identities {
                        let _ = cache
                            .set_greeting_name(&channel, &external_id, trimmed)
                            .await;
                    }
                }
            }
        }
        Ok(())
    }
}
