use crate::{
    application::{collections::CollectionService, spans::SpanService},
    domain::{
        identity::Actor,
        spans::{NewSpan, SpanQuery},
    },
    storage::pulse::PulseRepository,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;
pub mod http;
#[cfg(test)]
mod tests;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid authorization or request")]
    Invalid,
    #[error("integration permission denied")]
    Forbidden,
    #[error("integration is not configured")]
    Unavailable,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}
pub fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn verify_pkce(verifier: &str, challenge: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}
fn validate_window(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Result<(), Error> {
    match (from, to) {
        (None, None) => Ok(()),
        (Some(f), Some(t)) if t > f && t - f <= Duration::days(365) => Ok(()),
        _ => Err(Error::Invalid),
    }
}
#[derive(Clone)]
pub struct IntegrationService {
    pub pool: PgPool,
    spans: SpanService,
    collections: CollectionService,
    charts: PulseRepository,
    client_id: String,
    redirect_uri: Option<String>,
}
#[derive(Deserialize)]
pub struct Authorization {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
    pub collection_ids: Vec<Uuid>,
    pub chart_ids: Vec<Uuid>,
    pub span_from: Option<DateTime<Utc>>,
    pub span_to: Option<DateTime<Utc>>,
    pub allow_create_plans: bool,
}
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Grant {
    pub id: Uuid,
    pub user_id: Uuid,
    pub collection_ids: Vec<Uuid>,
    pub chart_ids: Vec<Uuid>,
    pub span_from: Option<DateTime<Utc>>,
    pub span_to: Option<DateTime<Utc>>,
    pub allow_create_plans: bool,
}
#[derive(Deserialize)]
pub struct TokenRequest {
    pub client_id: String,
    pub grant_type: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
}
#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub token_type: &'static str,
}
#[derive(Deserialize, Serialize)]
pub struct Plan {
    pub request_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub source_url: Option<String>,
    pub source_id: Uuid,
    pub collection_id: Option<Uuid>,
}
impl IntegrationService {
    pub fn new(
        pool: PgPool,
        spans: SpanService,
        collections: CollectionService,
        charts: PulseRepository,
    ) -> Self {
        Self {
            pool,
            spans,
            collections,
            charts,
            client_id: std::env::var("SHARE_TO_ACTION_CLIENT_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "share_to_action".into()),
            redirect_uri: std::env::var("SHARE_TO_ACTION_REDIRECT_URI")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
    pub fn validate_client(&self, client: &str, redirect: &str) -> Result<(), Error> {
        let expected = self.redirect_uri.as_ref().ok_or(Error::Unavailable)?;
        if client != self.client_id || redirect != expected {
            return Err(Error::Invalid);
        }
        let url = url::Url::parse(expected).map_err(|_| Error::Unavailable)?;
        if url.fragment().is_some()
            || !(url.scheme() == "https"
                || url.scheme() == "http"
                    && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
        {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub async fn authorize(&self, actor: &Actor, a: Authorization) -> Result<String, Error> {
        if actor.principal_kind != crate::domain::identity::PrincipalKind::User {
            return Err(Error::Forbidden);
        }
        self.validate_client(&a.client_id, &a.redirect_uri)?;
        validate_window(a.span_from, a.span_to)?;
        if a.state.is_empty()
            || a.state.len() > 512
            || a.code_challenge.len() != 43
            || URL_SAFE_NO_PAD
                .decode(&a.code_challenge)
                .map(|v| v.len())
                .ok()
                != Some(32)
            || a.collection_ids.len() > 30
            || a.chart_ids.len() > 10
        {
            return Err(Error::Invalid);
        }
        if (!a.collection_ids.is_empty() || !a.chart_ids.is_empty()) && a.span_from.is_none() {
            return Err(Error::Invalid);
        }
        for id in &a.collection_ids {
            if self.collections.get_collection(actor, *id).await?.is_none() {
                return Err(Error::Forbidden);
            }
        }
        for id in &a.chart_ids {
            if self.charts.get_chart(actor.user_id, *id).await?.is_none() {
                return Err(Error::Forbidden);
            }
        }
        let mut tx = self.pool.begin().await?;
        let id=sqlx::query_scalar::<_,Uuid>("INSERT INTO integration_grants(user_id,client_id,collection_ids,chart_ids,span_from,span_to,allow_create_plans) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id").bind(actor.user_id).bind(&self.client_id).bind(a.collection_ids).bind(a.chart_ids).bind(a.span_from).bind(a.span_to).bind(a.allow_create_plans).fetch_one(&mut *tx).await?;
        let code = secret();
        sqlx::query("INSERT INTO integration_codes(code_hash,grant_id,redirect_uri,code_challenge,expires_at) VALUES($1,$2,$3,$4,now()+interval '5 minutes')").bind(hash(&code)).bind(id).bind(&a.redirect_uri).bind(a.code_challenge).execute(&mut *tx).await?;
        tx.commit().await?;
        let mut url = url::Url::parse(&a.redirect_uri).map_err(|_| Error::Invalid)?;
        url.query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &a.state);
        Ok(url.into())
    }
    pub async fn token(&self, r: TokenRequest) -> Result<TokenResponse, Error> {
        self.validate_client(
            &self.client_id,
            self.redirect_uri.as_deref().ok_or(Error::Unavailable)?,
        )?;
        if r.client_id != self.client_id {
            return Err(Error::Invalid);
        }
        let mut tx = self.pool.begin().await?;
        let grant = if r.grant_type.as_deref() == Some("refresh_token") {
            let value = r.refresh_token.as_deref().ok_or(Error::Invalid)?;
            sqlx::query_scalar::<_,Uuid>("UPDATE integration_tokens t SET rotated_at=now() FROM integration_grants g WHERE t.refresh_hash=$1 AND t.grant_id=g.id AND g.client_id=$2 AND g.revoked_at IS NULL AND g.expires_at>now() AND t.refresh_expires_at>now() AND t.rotated_at IS NULL RETURNING t.grant_id").bind(hash(value)).bind(&self.client_id).fetch_optional(&mut *tx).await?.ok_or(Error::Invalid)?
        } else {
            if !matches!(r.grant_type.as_deref(), None | Some("authorization_code")) {
                return Err(Error::Invalid);
            }
            self.validate_client(
                &r.client_id,
                r.redirect_uri.as_deref().ok_or(Error::Invalid)?,
            )?;
            let row=sqlx::query("SELECT c.grant_id,c.code_challenge,c.redirect_uri FROM integration_codes c JOIN integration_grants g ON g.id=c.grant_id WHERE c.code_hash=$1 AND c.consumed_at IS NULL AND c.expires_at>now() AND g.client_id=$2 AND g.revoked_at IS NULL AND g.expires_at>now() FOR UPDATE OF c").bind(hash(r.code.as_deref().ok_or(Error::Invalid)?)).bind(&self.client_id).fetch_optional(&mut *tx).await?.ok_or(Error::Invalid)?;
            if row.get::<String, _>("redirect_uri") != r.redirect_uri.unwrap()
                || !verify_pkce(
                    r.code_verifier.as_deref().ok_or(Error::Invalid)?,
                    &row.get::<String, _>("code_challenge"),
                )
            {
                return Err(Error::Invalid);
            }
            sqlx::query("UPDATE integration_codes SET consumed_at=now() WHERE code_hash=$1")
                .bind(hash(r.code.as_deref().unwrap()))
                .execute(&mut *tx)
                .await?;
            row.get("grant_id")
        };
        let access = secret();
        let refresh = secret();
        sqlx::query("INSERT INTO integration_tokens(access_hash,refresh_hash,grant_id,access_expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '30 days')").bind(hash(&access)).bind(hash(&refresh)).bind(grant).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(TokenResponse {
            access_token: access,
            refresh_token: refresh,
            expires_in: 3600,
            token_type: "Bearer",
        })
    }
    pub async fn authenticate(&self, token: &str) -> Result<Grant, Error> {
        self.validate_client(
            &self.client_id,
            self.redirect_uri.as_deref().ok_or(Error::Unavailable)?,
        )?;
        sqlx::query_as::<_,Grant>("SELECT g.id,g.user_id,g.collection_ids,g.chart_ids,g.span_from,g.span_to,g.allow_create_plans FROM integration_grants g JOIN integration_tokens t ON g.id=t.grant_id WHERE t.access_hash=$1 AND t.access_expires_at>now() AND t.rotated_at IS NULL AND g.revoked_at IS NULL AND g.expires_at>now() AND g.client_id=$2").bind(hash(token)).bind(&self.client_id).fetch_optional(&self.pool).await?.ok_or(Error::Forbidden)
    }
    pub async fn revoke(&self, g: &Grant) -> Result<(), Error> {
        sqlx::query("UPDATE integration_grants SET revoked_at=now() WHERE id=$1 AND user_id=$2")
            .bind(g.id)
            .bind(g.user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn context(&self, g: &Grant) -> Result<serde_json::Value, Error> {
        let actor = Actor::user(g.user_id);
        let mut collections = Vec::new();
        let mut spans = Vec::new();
        let mut pulse = Vec::new();
        for id in &g.collection_ids {
            if let Some(c) = self.collections.get_collection(&actor, *id).await? {
                collections.push(c);
                if g.span_from.is_some() {
                    let q = SpanQuery {
                        collection_id: Some(*id),
                        from: g.span_from,
                        to: g.span_to,
                        limit: Some(100),
                        ..Default::default()
                    };
                    let rows = self
                        .spans
                        .list_spans(&actor, &q)
                        .await
                        .map_err(|_| Error::Invalid)?;
                    for mut s in rows {
                        s.collection_ids.retain(|id| g.collection_ids.contains(id));
                        if spans
                            .iter()
                            .all(|p: &crate::domain::spans::Span| p.id != s.id)
                            && spans.len() < 300
                        {
                            spans.push(s)
                        }
                    }
                }
            }
        }
        let visible_ids: Vec<Uuid> = spans.iter().map(|s| s.id).collect();
        for s in &mut spans {
            if s.parent_id.is_some_and(|id| !visible_ids.contains(&id)) {
                s.parent_id = None;
            }
        }
        for id in &g.chart_ids {
            if let Some(chart) = self.charts.get_chart(g.user_id,*id).await? {
                let data = self.chart_data(g,&chart).await?;
                pulse.push(serde_json::json!({"id":chart.id,"title":chart.title,"definition":chart.definition,"data":data}));
            }
        }
        Ok(serde_json::json!({"collections":collections,"spans":spans,"pulse":pulse}))
    }
    async fn chart_data(&self,g:&Grant,chart:&crate::domain::pulse::SavedPulseChart) -> Result<serde_json::Value,Error> {
        use chrono::TimeZone;
        let timezone=chart.definition.timezone.parse::<chrono_tz::Tz>().map_err(|_| Error::Invalid)?;
        let now=chrono::Utc::now();
        let last=now.with_timezone(&timezone).date_naive()-chrono::Duration::days(i64::from(chart.definition.offset_days));
        let first=last-chrono::Duration::days(i64::from(chart.definition.period_days)-1);
        let start=timezone.from_local_datetime(&first.and_hms_opt(0,0,0).ok_or(Error::Invalid)?).earliest().ok_or(Error::Invalid)?.with_timezone(&chrono::Utc);
        let end=if chart.definition.offset_days==0 {now} else {timezone.from_local_datetime(&(last+chrono::Duration::days(1)).and_hms_opt(0,0,0).ok_or(Error::Invalid)?).earliest().ok_or(Error::Invalid)?.with_timezone(&chrono::Utc)};
        if g.span_from.is_none_or(|from| from>start) || g.span_to.is_none_or(|to| to<end) {return Err(Error::Forbidden);}
        let result=crate::application::pulse::service::PulseService::new(self.charts.clone()).preview(&Actor::user(g.user_id),chart.definition.clone()).await.map_err(|_| Error::Invalid)?;
        serde_json::to_value(result).map_err(|_| Error::Invalid)
    }
    pub async fn plan(&self, g: &Grant, p: Plan) -> Result<Uuid, Error> {
        if !g.allow_create_plans
            || p.collection_id
                .is_some_and(|id| !g.collection_ids.contains(&id))
        {
            return Err(Error::Forbidden);
        }
        if p.title.trim().is_empty()
            || p.title.len() > 500
            || p.notes.as_ref().is_some_and(|s| s.len() > 16000)
            || p.starts_at.zip(p.ends_at).is_some_and(|(s, e)| e < s)
        {
            return Err(Error::Invalid);
        }
        if let Some(u) = &p.source_url {
            let parsed = url::Url::parse(u).map_err(|_| Error::Invalid)?;
            if !matches!(parsed.scheme(), "https" | "http") {
                return Err(Error::Invalid);
            }
        }
        let actor = Actor::user(g.user_id);
        if let Some(c) = p.collection_id
            && self.collections.get_collection(&actor, c).await?.is_none()
        {
            return Err(Error::Forbidden);
        }
        let payload_hash = hash(&serde_json::to_string(&p).map_err(|_| Error::Invalid)?);
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("{}:{}:{}", g.user_id, self.client_id, p.request_id))
            .execute(&mut *tx)
            .await?;
        let live=sqlx::query_scalar::<_,bool>("SELECT revoked_at IS NULL AND expires_at>now() FROM integration_grants WHERE id=$1 FOR SHARE").bind(g.id).fetch_optional(&mut *tx).await?.unwrap_or(false);
        if !live {
            return Err(Error::Forbidden);
        }
        if let Some((id,previous))=sqlx::query_as::<_,(Uuid,String)>("SELECT span_id,payload_hash FROM integration_plan_requests WHERE user_id=$1 AND client_id=$2 AND request_id=$3").bind(g.user_id).bind(&self.client_id).bind(p.request_id).fetch_optional(&mut *tx).await? {if previous!=payload_hash{return Err(Error::Invalid)}return Ok(id)}
        let input = NewSpan {
            title: p.title,
            notes: p.notes.unwrap_or_default(),
            start_at: p.starts_at,
            end_at: p.ends_at,
            due_at: p.due_at,
            source: Some("share_to_action".into()),
            source_ref: Some(p.request_id.to_string()),
            data: Some(serde_json::json!({"source_id":p.source_id,"source_url":p.source_url})),
            collection_ids: p.collection_id.into_iter().collect(),
            ..Default::default()
        };
        let id = self
            .spans
            .create_span_in_transaction(&actor, &mut tx, input)
            .await
            .map_err(|_| Error::Invalid)?;
        sqlx::query("INSERT INTO integration_plan_requests(user_id,client_id,request_id,payload_hash,span_id) VALUES($1,$2,$3,$4,$5)").bind(g.user_id).bind(&self.client_id).bind(p.request_id).bind(payload_hash).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        self.spans.notify_created(g.user_id, id);
        Ok(id)
    }
}
