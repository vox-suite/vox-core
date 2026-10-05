use crate::{
    domain::spans::{ExecutionType, NewSpan, SpanStatus},
    realtime::UserEventHub,
    storage::spans::SpanRepository,
};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgPool, Row};
use std::collections::BTreeMap;
use uuid::Uuid;
use vox_connections::{
    identity::RequestContext,
    providers::{
        playstation::PlayStationError,
        playstation_account::{GameSnapshot, PlayStationAccounts, observed_activity},
    },
};

#[derive(Clone)]
pub struct PlayStationCapture {
    pool: PgPool,
    pub accounts: PlayStationAccounts,
    hub: Option<UserEventHub>,
}
#[derive(Serialize)]
pub struct SyncResult {
    pub connection_id: Uuid,
    pub games_count: usize,
    pub spans_created: usize,
    pub baseline: bool,
}

impl PlayStationCapture {
    pub fn new(
        pool: PgPool,
        key: &str,
        hub: Option<UserEventHub>,
    ) -> Result<Self, PlayStationError> {
        Ok(Self {
            accounts: PlayStationAccounts::new(pool.clone(), key)?,
            pool,
            hub,
        })
    }
    pub async fn set_capture(
        &self,
        context: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<(), PlayStationError> {
        let mut tx = self.pool.begin().await?;
        let found=sqlx::query("SELECT id FROM external_connections WHERE id=$1 AND user_context_id=$2 AND authorization_state='authorized' FOR UPDATE").bind(id).bind(context.id.0).fetch_optional(&mut *tx).await?;
        if found.is_none() {
            return Err(PlayStationError::ConnectionNotFound);
        }
        let result=sqlx::query("UPDATE playstation_accounts SET generation=CASE WHEN capture_enabled<>$2 THEN gen_random_uuid() ELSE generation END,capture_enabled=$2,snapshots=CASE WHEN capture_enabled<>$2 THEN '{}'::jsonb ELSE snapshots END,last_synced_at=CASE WHEN capture_enabled<>$2 THEN NULL ELSE last_synced_at END,next_sync_at=now(),failure_code=NULL,updated_at=now() WHERE connection_id=$1").bind(id).bind(enabled).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            return Err(PlayStationError::ConnectionNotFound);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn sync_for_context(
        &self,
        context: &RequestContext,
        id: Uuid,
    ) -> Result<SyncResult, PlayStationError> {
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM external_connections WHERE id=$1 AND user_context_id=$2)",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_one(&self.pool)
        .await?;
        if !found {
            return Err(PlayStationError::ConnectionNotFound);
        }
        self.sync(id).await
    }
    pub async fn sync(&self, id: Uuid) -> Result<SyncResult, PlayStationError> {
        self.sync_mode(id, true).await
    }
    async fn sync_mode(&self, id: Uuid, force: bool) -> Result<SyncResult, PlayStationError> {
        let generation: Option<Uuid> = sqlx::query_scalar(
            "SELECT generation FROM playstation_accounts WHERE connection_id=$1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let generation = generation.ok_or(PlayStationError::ConnectionNotFound)?;
        let result = self.sync_inner(id, force, generation).await;
        if let Err(error) = &result {
            self.failure(id, error, generation).await?;
        }
        result
    }
    async fn sync_inner(
        &self,
        id: Uuid,
        force: bool,
        generation: Uuid,
    ) -> Result<SyncResult, PlayStationError> {
        let token = self.accounts.token_for_connection(id, true).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query("SELECT c.user_context_id,uc.user_id FROM external_connections c JOIN user_contexts uc ON uc.id=c.user_context_id JOIN integration_definitions i ON i.id=c.integration_id WHERE c.id=$1 AND c.authorization_state='authorized' AND (c.expires_at IS NULL OR c.expires_at>now()) AND 'playstation.game_activity'=ANY(c.authorized_capabilities) AND i.external_key='playstation' AND i.state='enabled' FOR UPDATE OF c").bind(id).fetch_optional(&mut *tx).await?.ok_or(PlayStationError::ReconnectRequired)?;
        let user_id: Uuid = row.get("user_id");
        let capture=sqlx::query("SELECT account_id,snapshots,capture_enabled,last_synced_at,next_sync_at,failure_code,generation FROM playstation_accounts WHERE connection_id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or(PlayStationError::ReconnectRequired)?;
        if capture.get::<Uuid, _>("generation") != generation {
            return Err(PlayStationError::ConnectionNotFound);
        }
        if !capture.get::<bool, _>("capture_enabled") {
            return Err(PlayStationError::Invalid);
        }
        let account_id: String = capture.get("account_id");
        let previous: BTreeMap<String, GameSnapshot> =
            serde_json::from_value(capture.get("snapshots"))
                .map_err(|_| PlayStationError::Invalid)?;
        let not_due = capture.get::<chrono::DateTime<Utc>, _>("next_sync_at") > Utc::now();
        if not_due && (!force || capture.get::<Option<String>, _>("failure_code").is_some()) {
            return Ok(SyncResult {
                connection_id: id,
                games_count: previous.len(),
                spans_created: 0,
                baseline: false,
            });
        }
        let games = match tokio::time::timeout(
            std::time::Duration::from_secs(45),
            self.accounts.games(&token),
        )
        .await
        {
            Ok(Ok(games)) => games,
            failed => {
                let error = match failed {
                    Ok(Err(e)) => e,
                    _ => PlayStationError::ProviderError("PSN sync timed out".into()),
                };
                tx.rollback().await?;
                return Err(error);
            }
        };
        let now = Utc::now();
        let baseline_at: Option<chrono::DateTime<Utc>> = capture.get("last_synced_at");
        let activity = observed_activity(&account_id, &previous, &games, baseline_at, now);
        let schema_id: Uuid = sqlx::query_scalar("INSERT INTO data_schemas(user_id,namespace,name,description,json_schema,color_token,icon_token) VALUES($1,'playstation','gaming_activity','Observed PlayStation playtime changes; exact session boundaries unknown',$2,8,15) ON CONFLICT(user_id,namespace,name,version) DO UPDATE SET name=EXCLUDED.name RETURNING id")
            .bind(user_id).bind(json!({"type":"object","required":["platform","title_id","duration_seconds","timing"],"properties":{"platform":{"type":"string"},"title_id":{"type":"string"},"duration_seconds":{"type":"integer","minimum":0},"timing":{"const":"provider_last_played"}}})).fetch_one(&mut *tx).await?;
        let mut spans = Vec::new();
        for observed in &activity {
            let source_ref = format!(
                "{}:{}",
                observed.source_ref,
                observed.observation_start.timestamp_micros()
            );
            let data = json!({"platform":observed.game.platform,"title_id":observed.game.title_id,"account_id":account_id,"connection_id":id,"game_name":observed.game.name,"image_url":observed.game.image_url,"duration_seconds":observed.duration_seconds,"total_playtime_seconds":observed.game.play_duration_seconds,"last_played_at":observed.game.last_played_at,"observation_start":observed.observation_start,"observation_end":observed.observation_end,"timing":"provider_last_played","session_start":null,"session_end":null});
            let event_id:Uuid=sqlx::query_scalar("INSERT INTO inbound_events(user_id,source_kind,source_id,external_event_id,payload_hash,event_type,occurred_at,payload,processed_at) VALUES($1,'playstation',$2,$3,$4,'gaming.playtime_observed',$5,$6,now()) ON CONFLICT(source_kind,source_id,external_event_id) DO UPDATE SET external_event_id=EXCLUDED.external_event_id RETURNING id")
                .bind(user_id).bind(format!("{user_id}:{account_id}")).bind(&source_ref).bind({use sha2::{Digest,Sha256};hex::encode(Sha256::digest(serde_json::to_vec(&data).map_err(|_|PlayStationError::Invalid)?))}).bind(now).bind(&data).fetch_one(&mut *tx).await?;
            let span = NewSpan {
                title: format!("Played {}", observed.game.name),
                notes: format!(
                    "{} minutes of additional playtime reported between {} and {}. Exact session times are unknown.",
                    observed.duration_seconds / 60,
                    observed.observation_start,
                    observed.observation_end
                ),
                category: Some("gaming".into()),
                source: Some("playstation".into()),
                source_ref: Some(source_ref),
                status: Some(SpanStatus::Done),
                start_at: observed.game.last_played_at.map(|last| last.min(now)),
                end_at: None,
                execution_type: Some(ExecutionType::ManualHuman),
                source_event_id: Some(event_id),
                schema_id: Some(schema_id),
                data: Some(data),
                ..Default::default()
            };
            spans.push(SpanRepository::record_in_transaction(&mut tx, user_id, span).await?);
        }
        let mut snapshots = previous.clone();
        for game in &games {
            let Some(last_played_at) = game.last_played_at else {
                continue;
            };
            snapshots.insert(
                game.title_id.clone(),
                GameSnapshot {
                    total_seconds: previous
                        .get(&game.title_id)
                        .map_or(game.play_duration_seconds, |old| {
                            old.total_seconds.max(game.play_duration_seconds)
                        }),
                    observed_at: now,
                    last_played_at,
                    name: game.name.clone(),
                    platform: game.platform.clone(),
                },
            );
        }
        sqlx::query("UPDATE playstation_accounts SET snapshots=$2,last_synced_at=$3,next_sync_at=$3+interval '1 day',failure_code=NULL,failure_count=0,updated_at=now() WHERE connection_id=$1").bind(id).bind(json!(snapshots)).bind(now).execute(&mut *tx).await?;
        if self.hub.is_none() {
            for span_id in &spans {
                sqlx::query("SELECT pg_notify('vox_playstation_spans',$1)")
                    .bind(json!({"user_id":user_id,"span_id":span_id}).to_string())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        if let Some(hub) = &self.hub {
            for span_id in &spans {
                hub.notify(user_id, json!({"type":"span_created","span_id":span_id}));
            }
        }
        Ok(SyncResult {
            connection_id: id,
            games_count: games.len(),
            spans_created: spans.len(),
            baseline: baseline_at.is_none(),
        })
    }
    async fn failure(
        &self,
        id: Uuid,
        error: &PlayStationError,
        generation: Uuid,
    ) -> Result<(), PlayStationError> {
        let mut tx = self.pool.begin().await?;
        let active=sqlx::query("SELECT c.id FROM external_connections c JOIN playstation_accounts p ON p.connection_id=c.id WHERE c.id=$1 AND c.authorization_state='authorized' AND p.capture_enabled AND p.generation=$2 FOR UPDATE OF c").bind(id).bind(generation).fetch_optional(&mut *tx).await?;
        if active.is_none() {
            return Ok(());
        }
        let code = match error {
            PlayStationError::ReconnectRequired => "reconnect_required",
            PlayStationError::RateLimited(_) => "rate_limited",
            _ => "sync_failed",
        };
        let delay = match error {
            PlayStationError::RateLimited(s) => (*s).clamp(60, 86400) as i64,
            _ => 300,
        };
        sqlx::query("UPDATE playstation_accounts SET failure_code=$2,failure_count=LEAST(failure_count+1,10),next_sync_at=now()+make_interval(secs => LEAST($3::double precision*power(2,LEAST(failure_count,5)),86400)),capture_enabled=CASE WHEN $2='reconnect_required' THEN false ELSE capture_enabled END,updated_at=now() WHERE connection_id=$1").bind(id).bind(code).bind(delay).execute(&mut *tx).await?;
        if code == "reconnect_required" {
            sqlx::query("UPDATE external_connections SET authorization_state='expired',expires_at=NULL,updated_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn run_due(&self) -> Result<(), PlayStationError> {
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT p.connection_id FROM playstation_accounts p JOIN external_connections c ON c.id=p.connection_id WHERE p.capture_enabled AND p.next_sync_at<=now() AND c.authorization_state='authorized' ORDER BY p.next_sync_at LIMIT 20").fetch_all(&self.pool).await?;
        for id in ids {
            if self.sync_mode(id, false).await.is_err() {
                tracing::warn!(connection_id=%id,"PlayStation capture failed");
            }
        }
        Ok(())
    }
}
