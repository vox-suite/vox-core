use super::{
    execution,
    measurements::{definition_hash, measurement_catalog, validate_definition},
};
use crate::{
    agents::chart_suggester::SuggestingCharts,
    domain::{identity::Actor, pulse::*},
    storage::pulse::{Inventory, PulseMetadata, PulseRepository},
};
use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum PulseError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("Pulse is busy; try again shortly")]
    Busy,
    #[error("Pulse data could not be loaded")]
    Database(#[from] sqlx::Error),
}
#[derive(Clone)]
pub struct PulseService {
    pub repo: PulseRepository,
    suggester: Option<Arc<dyn SuggestingCharts>>,
    locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    gate: Arc<tokio::sync::Semaphore>,
}
impl PulseService {
    pub fn new(repo: PulseRepository) -> Self {
        Self {
            repo,
            suggester: None,
            locks: Arc::default(),
            gate: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
    pub fn with_suggester(mut self, suggester: Arc<dyn SuggestingCharts>) -> Self {
        self.suggester = Some(suggester);
        self
    }
    fn key(actor: &Actor, meta: &PulseMetadata, kind: &str) -> String {
        let mut grants = actor.grants.clone();
        grants.sort();
        format!(
            "{kind}:{}:{:x}",
            meta.revision,
            Sha256::digest(format!(
                "{}:{:?}:{grants:?}",
                actor.principal_id, actor.principal_kind
            ))
        )
    }
    async fn lock(&self, key: &str) -> Result<Arc<Mutex<()>>, PulseError> {
        let mut locks = self.locks.lock().await;
        locks.retain(|_, v| v.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        if locks.len() >= 128 {
            return Err(PulseError::Busy);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.into(), Arc::downgrade(&lock));
        Ok(lock)
    }
    async fn inventory(
        &self,
        actor: &Actor,
        meta: &PulseMetadata,
        refresh: bool,
    ) -> Result<Inventory, PulseError> {
        let key = Self::key(actor, meta, "inventory");
        if !refresh && let Some(value) = meta.caches.get(&key) {
            return serde_json::from_value(value.clone())
                .map_err(|_| PulseError::Invalid("Invalid cached inventory".into()));
        }
        let inventory = self.repo.inventory(actor.user_id).await?;
        self.repo
            .put_cache(actor.user_id, &key, json!(inventory), 900)
            .await?;
        Ok(inventory)
    }
    async fn results(
        &self,
        actor: &Actor,
        meta: &PulseMetadata,
        inputs: &[(PulseDefinition, Measurement)],
        refresh: bool,
        arrived: chrono::DateTime<Utc>,
    ) -> Result<Vec<PulseResult>, PulseError> {
        let key = Self::key(
            actor,
            meta,
            &format!(
                "results:{}:{:x}",
                inputs
                    .iter()
                    .map(|(d, _)| Utc::now()
                        .with_timezone(&d.timezone.parse::<chrono_tz::Tz>().unwrap())
                        .date_naive()
                        .to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                Sha256::digest(
                    serde_json::to_vec(&inputs.iter().map(|(d, _)| d).collect::<Vec<_>>()).unwrap()
                )
            ),
        );
        if !refresh && let Some(value) = meta.caches.get(&key) {
            return serde_json::from_value(value.clone())
                .map_err(|_| PulseError::Invalid("Invalid cached result".into()));
        }
        let _permit = self.gate.acquire().await.map_err(|_| PulseError::Busy)?;
        let results = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            execution::execute_cached(
                &self.repo.pool,
                actor.user_id,
                inputs,
                &key,
                refresh,
                arrived,
            ),
        )
        .await
        .map_err(|_| PulseError::Busy)??;
        self.repo
            .put_cache(actor.user_id, &key, json!(results), 60)
            .await?;
        Ok(results)
    }
    pub async fn measurements(
        &self,
        actor: &Actor,
        timezone: &str,
    ) -> Result<Vec<Measurement>, PulseError> {
        Self::timezone(timezone)?;
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let inventory = self.inventory(actor, &meta, false).await?;
        Ok(measurement_catalog(&inventory.profiles))
    }
    fn timezone(timezone: &str) -> Result<(), PulseError> {
        timezone
            .parse::<chrono_tz::Tz>()
            .map(|_| ())
            .map_err(|_| PulseError::Invalid("Choose a valid timezone".into()))
    }
    pub async fn discover(
        &self,
        actor: &Actor,
        input: DiscoveryInput,
    ) -> Result<DiscoveryResponse, PulseError> {
        let arrived = Utc::now();
        Self::timezone(&input.timezone)?;
        let lock = self
            .lock(&format!("{}:discover:{}", actor.user_id, input.timezone))
            .await?;
        let _guard = lock.lock().await;
        let mut discovery_lease = self.repo.pool.begin().await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(25),
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("{}:discover:{}", actor.user_id, input.timezone))
                .execute(&mut *discovery_lease),
        )
        .await
        .map_err(|_| PulseError::Busy)??;
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let mut stable = meta.clone();
        stable.revision = "stable".into();
        let key = Self::key(actor, &stable, &format!("discovery:{}", input.timezone));
        let prompt = input
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| p.chars().take(500).collect::<String>());
        let mut existing: Option<DiscoveryResponse> = None;
        if prompt.is_none()
            && let Some(value) = meta.caches.get(&key)
        {
            let mut cached: DiscoveryResponse = serde_json::from_value(value.clone())
                .map_err(|_| PulseError::Invalid("Invalid cached suggestions".into()))?;
            cached.suggestions.retain(|s| {
                let hash = definition_hash(&s.definition);
                !meta.dismissed.contains(&hash) && !meta.saved_hashes.contains(&hash)
            });
            let fresh =
                serde_json::from_value::<chrono::DateTime<Utc>>(value["computed_at"].clone())
                    .is_ok_and(|at| at >= arrived);
            if input.more || (input.refresh && !fresh) {
                existing = Some(cached);
            } else {
                return Ok(cached);
            }
        }
        let limit: usize = if prompt.is_some() { 4 } else { 8 };
        let kept: Vec<PulseSuggestion> = if input.refresh {
            vec![]
        } else {
            existing.take().map(|e| e.suggestions).unwrap_or_default()
        };
        let inventory = self.inventory(actor, &meta, input.refresh).await?;
        let catalog = measurement_catalog(&inventory.profiles);
        let mut candidates = vec![];
        let mut llm_ok = false;
        if !catalog.is_empty()
            && let Some(suggester) = &self.suggester
        {
            let ranked = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                suggester.suggest_pulse(
                    catalog.iter().take(40).cloned().collect(),
                    input.timezone.clone(),
                    crate::agents::chart_suggester::PulseAsk {
                        instruction: prompt.clone(),
                        exclude_titles: kept.iter().map(|s| s.title.clone()).collect(),
                        limit,
                    },
                ),
            )
            .await;
            if let Ok(Ok(ai)) = ranked
                && !ai.is_empty()
            {
                llm_ok = true;
                candidates = ai;
            }
        }
        let mut valid = vec![];
        let mut hashes: std::collections::HashSet<String> = kept
            .iter()
            .map(|s| definition_hash(&s.definition))
            .collect();
        for mut candidate in candidates {
            candidate.title = candidate.title.chars().take(120).collect();
            candidate.reason = candidate.reason.chars().take(400).collect();
            if candidate.title.trim().is_empty() || candidate.definition.timezone != input.timezone
            {
                continue;
            }
            if let Some(m) = catalog
                .iter()
                .find(|m| m.id == candidate.definition.measurement_id)
            {
                let historical = m.profile.last_at.is_none_or(|last| {
                    last < Utc::now()
                        - chrono::Duration::days(i64::from(candidate.definition.period_days))
                });
                if historical
                    && !matches!(
                        m.kind,
                        MeasurementKind::RecurringCostProjection
                            | MeasurementKind::KnownIntervalDuration
                    )
                    && m.profile.source != "playstation"
                {
                    continue;
                }
            }
            let hash = definition_hash(&candidate.definition);
            if (prompt.is_none()
                && (meta.dismissed.contains(&hash) || meta.saved_hashes.contains(&hash)))
                || !hashes.insert(hash)
            {
                continue;
            }
            if let Ok(measurement) = validate_definition(&candidate.definition, &catalog) {
                valid.push((candidate, measurement));
                if valid.len() == limit * 2 {
                    break;
                }
            }
        }
        let inputs = valid
            .iter()
            .map(|(c, m)| (c.definition.clone(), m.clone()))
            .collect::<Vec<_>>();
        let results = self
            .results(actor, &meta, &inputs, input.refresh, arrived)
            .await?;
        let mut suggestions = vec![];
        let mut sources = std::collections::HashSet::new();
        for ((candidate, measurement), preview) in valid.into_iter().zip(results) {
            if !preview.points.iter().any(|p| p.value.is_some()) {
                continue;
            }
            let source = measurement.profile.key.clone();
            if prompt.is_none() && sources.contains(&source) && suggestions.len() < 3 {
                continue;
            }
            sources.insert(source);
            suggestions.push(PulseSuggestion {
                title: candidate.title,
                reason: candidate.reason,
                definition: candidate.definition,
                measurement,
                preview,
            });
            if suggestions.len() == limit {
                break;
            }
        }
        let mut all = kept;
        all.extend(suggestions);
        let suggestions = all;
        let response = DiscoveryResponse {
            suggestions,
            connections: meta.connections,
            source_count: inventory.source_count,
            record_count: inventory.record_count,
            profiled_days: 90,
            computed_at: Utc::now(),
        };
        if prompt.is_none() {
            self.repo
                .put_cache(
                    actor.user_id,
                    &key,
                    json!(response),
                    if llm_ok { 31_536_000 } else { 60 },
                )
                .await?;
        }
        Ok(response)
    }
    pub async fn preview(
        &self,
        actor: &Actor,
        definition: PulseDefinition,
    ) -> Result<PulseResult, PulseError> {
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let inventory = self.inventory(actor, &meta, false).await?;
        let measurement =
            validate_definition(&definition, &measurement_catalog(&inventory.profiles))
                .map_err(PulseError::Invalid)?;
        Ok(self
            .results(
                actor,
                &meta,
                &[(definition, measurement)],
                false,
                Utc::now(),
            )
            .await?
            .remove(0))
    }
    pub async fn save(
        &self,
        actor: &Actor,
        input: SavePulseInput,
    ) -> Result<SavedPulseChart, PulseError> {
        if input.title.trim().is_empty() || input.title.chars().count() > 120 {
            return Err(PulseError::Invalid(
                "Chart title must contain 1 to 120 characters".into(),
            ));
        }
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let inventory = self.inventory(actor, &meta, false).await?;
        validate_definition(&input.definition, &measurement_catalog(&inventory.profiles))
            .map_err(PulseError::Invalid)?;
        self.repo
            .save(
                actor.user_id,
                &input,
                &meta.revision,
                &definition_hash(&input.definition),
            )
            .await
            .map_err(|error| match error {
                sqlx::Error::Protocol(message) => PulseError::Conflict(message),
                other => PulseError::Database(other),
            })
    }
    pub async fn dismiss(
        &self,
        actor: &Actor,
        definition: PulseDefinition,
    ) -> Result<(), PulseError> {
        let meta = self.repo.metadata(actor.user_id, None).await?;
        let inventory = self.inventory(actor, &meta, false).await?;
        validate_definition(&definition, &measurement_catalog(&inventory.profiles))
            .map_err(PulseError::Invalid)?;
        self.repo
            .dismiss(actor.user_id, &definition_hash(&definition))
            .await?;
        Ok(())
    }
    pub async fn canvas(
        &self,
        actor: &Actor,
        timezone: &str,
        refresh: bool,
        cursor: Option<Uuid>,
    ) -> Result<CanvasResponse, PulseError> {
        let arrived = Utc::now();
        Self::timezone(timezone)?;
        let lock = self
            .lock(&format!("{}:canvas:{timezone}:{cursor:?}", actor.user_id))
            .await?;
        let _guard = lock.lock().await;
        let mut meta = self.repo.metadata(actor.user_id, cursor).await?;
        if meta.charts.is_empty() {
            return Ok(CanvasResponse {
                charts: vec![],
                legacy_boards: meta.legacy_boards,
                next_cursor: None,
            });
        }
        let inventory = self.inventory(actor, &meta, refresh).await?;
        let catalog = measurement_catalog(&inventory.profiles);
        let mut inputs = vec![];
        let mut valid_indexes = vec![];
        for (index, chart) in meta.charts.iter_mut().enumerate() {
            match validate_definition(&chart.definition, &catalog) {
                Ok(m) => {
                    valid_indexes.push(index);
                    inputs.push((chart.definition.clone(), m))
                }
                Err(message) => {
                    chart.result = Some(PulseResult {
                        source: "Source unavailable".into(),
                        points: vec![],
                        unit: String::new(),
                        quality: "unavailable".into(),
                        description: message.clone(),
                        record_count: 0,
                        undated_count: 0,
                        computed_at: Utc::now(),
                        data_as_of: None,
                        error: Some(message),
                    })
                }
            }
        }
        let results = self
            .results(actor, &meta, &inputs, refresh, arrived)
            .await?;
        for (index, result) in valid_indexes.into_iter().zip(results) {
            meta.charts[index].result = Some(result)
        }
        Ok(CanvasResponse {
            charts: meta.charts,
            legacy_boards: meta.legacy_boards,
            next_cursor: meta.next_cursor,
        })
    }
}
