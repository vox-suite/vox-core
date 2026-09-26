/**
* Integration tests for resource ownership and context boundaries.
*/
use async_trait::async_trait;
use chrono::{Duration, Utc};
use rig::{prelude::ToolContext, tool::Tool};
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
        tools::spans::{
            CreateSpan, CreateSpanArgs, GetSpan, GetSpanArgs, ListSpans, ListSpansArgs,
            SpanToolError, UpdateSpan, UpdateSpanArgs,
        },
    },
    conversations::{CompleteConversationRequest, RespondRequest, service::ConversationService},
    db::Db,
    identity::{ChannelIdentity, IdentityService},
    realtime::UserEventHub,
    schedules::{
        CreateScheduleRequest, ScheduleKind, UpdateScheduleRequest, service::ScheduleError,
        service::ScheduleService,
    },
};

struct FixedAgent;

#[async_trait]
impl ConversationResponder for FixedAgent {
    async fn respond(&self, _: ConversationPrompt) -> Result<String, AgentError> {
        Ok("acknowledged".into())
    }
}

async fn setup() -> Db {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    db
}

fn identity(value: &str) -> ChannelIdentity {
    ChannelIdentity {
        channel: "test-channel".into(),
        external_id: value.into(),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn conversations_and_schedules_deny_cross_context_observation_and_mutation() {
    let db = setup().await;
    let conversations = ConversationService::new(db.clone(), Arc::new(FixedAgent));
    let schedules = ScheduleService::new(db.clone());
    let alice = identity("alice");
    let bob = identity("bob");

    let alice_conversation = conversations
        .respond(RespondRequest {
            identity: alice.clone(),
            external_conversation_id: "shared-conversation-id".into(),
            text: "hello".into(),
            initiation_context: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
            filler: None,
        })
        .await
        .unwrap();
    let bob_conversation = conversations
        .respond(RespondRequest {
            identity: bob.clone(),
            external_conversation_id: "shared-conversation-id".into(),
            text: "hello".into(),
            initiation_context: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
            filler: None,
        })
        .await
        .unwrap();
    assert_ne!(
        alice_conversation.conversation_id,
        bob_conversation.conversation_id
    );

    conversations
        .complete(CompleteConversationRequest {
            identity: bob.clone(),
            external_conversation_id: "shared-conversation-id".into(),
        })
        .await
        .unwrap();
    let alice_status: String = sqlx::query_scalar("SELECT state FROM conversations WHERE id = $1")
        .bind(alice_conversation.conversation_id.0)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(alice_status, "active");

    let now = Utc::now();
    let alice_schedule = schedules
        .create_at(
            CreateScheduleRequest {
                identity: alice.clone(),
                instruction: "Alice reminder".into(),
                schedule_kind: ScheduleKind::Once,
                run_at: Some(now + Duration::hours(1)),
                recurrence_expression: None,
                timezone: "UTC".into(),
            },
            now,
        )
        .await
        .unwrap();
    let cross_context_update = schedules
        .update_at(
            alice_schedule.id,
            UpdateScheduleRequest {
                identity: bob,
                state: Some("paused".into()),
                run_at: None,
                recurrence_expression: None,
            },
            now,
        )
        .await;
    assert!(matches!(cross_context_update, Err(ScheduleError::NotFound)));

    schedules
        .update_at(
            alice_schedule.id,
            UpdateScheduleRequest {
                identity: alice,
                state: Some("paused".into()),
                run_at: None,
                recurrence_expression: None,
            },
            now,
        )
        .await
        .unwrap();

    let charlie = identity("charlie");
    let charlie_owner = IdentityService::new(db.clone())
        .resolve_legacy_owner(&charlie)
        .await
        .unwrap();
    let legacy_conversation_id: Uuid = sqlx::query_scalar(
        "INSERT INTO conversations (user_id, channel, external_conversation_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(charlie_owner.user_id.0)
    .bind(&charlie.channel)
    .bind("rolling-legacy-conversation")
    .fetch_one(db.pool())
    .await
    .unwrap();
    let resumed = conversations
        .respond(RespondRequest {
            identity: charlie,
            external_conversation_id: "rolling-legacy-conversation".into(),
            text: "resume".into(),
            initiation_context: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
            filler: None,
        })
        .await
        .unwrap();
    assert_eq!(resumed.conversation_id.0, legacy_conversation_id);
    let restored_user: Uuid = sqlx::query_scalar("SELECT user_id FROM conversations WHERE id = $1")
        .bind(legacy_conversation_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(restored_user, charlie_owner.user_id.0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn span_tools_scope_every_read_and_write_to_the_resource_owner() {
    let db = setup().await;
    let identities = IdentityService::new(db.clone());
    let alice = identities
        .resolve_legacy_owner(&identity("task-alice"))
        .await
        .unwrap();
    let bob = identities
        .resolve_legacy_owner(&identity("task-bob"))
        .await
        .unwrap();
    let mut context = ToolContext::new();

    let bob_collection_id: Uuid = sqlx::query_scalar(
        "INSERT INTO collections (user_id, name) VALUES ($1, 'Bob private trip') RETURNING id",
    )
    .bind(bob.user_id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let span_args = |title: &str, collection_id: Option<String>| CreateSpanArgs {
        title: title.into(),
        notes: None,
        category: None,
        start_at: None,
        end_at: None,
        due_at: None,
        status: None,
        execution_type: None,
        collection_name: None,
        collection_id,
        amount: None,
        currency: None,
    };
    let cross_owner = CreateSpan::new(Some(db.clone()), alice, UserEventHub::default())
        .call(
            &mut context,
            span_args(
                "Must not join Bob's collection",
                Some(bob_collection_id.to_string()),
            ),
        )
        .await;
    assert!(matches!(cross_owner, Err(SpanToolError::NotFound(_))));

    let created = CreateSpan::new(Some(db.clone()), alice, UserEventHub::default())
        .call(&mut context, span_args("Alice private span", None))
        .await
        .unwrap();
    let span_id = created["span"]["id"].as_str().unwrap().to_owned();

    let bob_list = ListSpans::new(Some(db.clone()), bob)
        .call(
            &mut context,
            ListSpansArgs {
                from: None,
                to: None,
                status: None,
                collection_id: None,
                unscheduled: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(bob_list["spans"].as_array().unwrap().len(), 0);

    let bob_get = GetSpan::new(Some(db.clone()), bob)
        .call(
            &mut context,
            GetSpanArgs {
                span_id: span_id.clone(),
            },
        )
        .await;
    assert!(matches!(bob_get, Err(SpanToolError::NotFound(_))));

    let update = |status: &str| UpdateSpanArgs {
        span_id: span_id.clone(),
        title: None,
        notes: None,
        status: Some(status.into()),
        start_at: None,
        end_at: None,
        due_at: None,
        execution_result: None,
    };
    let bob_update = UpdateSpan::new(Some(db.clone()), bob, UserEventHub::default())
        .call(&mut context, update("cancelled"))
        .await;
    assert!(matches!(bob_update, Err(SpanToolError::NotFound(_))));

    UpdateSpan::new(Some(db.clone()), alice, UserEventHub::default())
        .call(&mut context, update("done"))
        .await
        .unwrap();

    let stored_user: Uuid = sqlx::query_scalar("SELECT user_id FROM spans WHERE id = $1")
        .bind(Uuid::parse_str(&span_id).unwrap())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored_user, alice.user_id.0);
}
