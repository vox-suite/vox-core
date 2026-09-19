use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    actions::{ActionId, handler::ActionHandler},
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
    },
    bridge_client::{BridgeClientError, BridgeDispatch, OutboundCallRequest, OutboundCallResponse},
    db::Db,
    http::{AppState, router},
    identity::{IdentityService, UserId},
};

struct FakeBridge {
    calls: AtomicUsize,
}

#[async_trait]
impl BridgeDispatch for FakeBridge {
    async fn initiate_outbound_call(
        &self,
        _: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(OutboundCallResponse {
            provider_call_id: "CA-provider".into(),
        })
    }
}

struct Responder;

#[async_trait]
impl ConversationResponder for Responder {
    async fn respond(&self, _: ConversationPrompt) -> Result<String, AgentError> {
        Ok("ok".into())
    }
}

async fn setup() -> (Db, Uuid) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let user_id: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, 'phone', '+919999999996')")
        .bind(user_id).execute(db.pool()).await.unwrap();
    let owner = IdentityService::new(db.clone())
        .owner_for_user(UserId(user_id))
        .await
        .unwrap();
    let action_id: Uuid = sqlx::query_scalar(
        "INSERT INTO actions (user_context_id, user_id, kind, payload, idempotency_key) \
         VALUES ($1, $2, 'outbound_call', $3, $4) RETURNING id",
    )
    .bind(owner.user_context_id.0)
    .bind(user_id)
    .bind(serde_json::json!({"reason":"Warning", "opening_instruction":"Explain the warning"}))
    .bind(Uuid::new_v4().to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    (db, action_id)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_dispatch_creates_one_provider_call_and_persists_acceptance() {
    let (db, action_id) = setup().await;
    let bridge = Arc::new(FakeBridge {
        calls: AtomicUsize::new(0),
    });
    let handler = ActionHandler::new(db.clone(), bridge.clone());
    let (first, second) = tokio::join!(
        handler.handle(ActionId(action_id)),
        handler.handle(ActionId(action_id))
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(bridge.calls.load(Ordering::SeqCst), 1);
    let state: String = sqlx::query_scalar("SELECT state FROM actions WHERE id = $1")
        .bind(action_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let attempt: String =
        sqlx::query_scalar("SELECT state FROM action_attempts WHERE action_id = $1")
            .bind(action_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "in_progress");
    assert_eq!(attempt, "accepted");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn successful_callback_is_idempotent_and_accepted_by_the_schema() {
    let (db, action_id) = setup().await;
    sqlx::query(
        "UPDATE actions SET state = 'in_progress', provider_call_id = 'CA-provider' WHERE id = $1",
    )
    .bind(action_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO action_attempts (action_id, attempt_number, state) VALUES ($1, 1, 'accepted')",
    )
    .bind(action_id)
    .execute(db.pool())
    .await
    .unwrap();
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(Responder),
        "token".into(),
    ));
    let request = || {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/actions/{action_id}/result"))
            .header("authorization", "Bearer token")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"status":"succeeded","provider_call_id":"CA-provider","error_code":null}"#,
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    let state: String = sqlx::query_scalar("SELECT state FROM actions WHERE id = $1")
        .bind(action_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(state, "succeeded");
}
