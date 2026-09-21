use async_trait::async_trait;
use chrono::{Duration, Utc};
use rig::{prelude::ToolContext, tool::Tool};
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
        tools::tasks::{
            CreateTask, CreateTaskArgs, GetTask, GetTaskArgs, ListTasks, ListTasksArgs,
            TaskToolError, UpdateTask, UpdateTaskArgs,
        },
    },
    conversations::{CompleteConversationRequest, RespondRequest, service::ConversationService},
    db::Db,
    identity::{ChannelIdentity, IdentityService},
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
            voice_signature: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
        })
        .await
        .unwrap();
    let bob_conversation = conversations
        .respond(RespondRequest {
            identity: bob.clone(),
            external_conversation_id: "shared-conversation-id".into(),
            text: "hello".into(),
            initiation_context: None,
            voice_signature: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
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
    let alice_status: String = sqlx::query_scalar("SELECT status FROM conversations WHERE id = $1")
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
        "INSERT INTO conversations (user_id, channel, external_id) \
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
            voice_signature: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
        })
        .await
        .unwrap();
    assert_eq!(resumed.conversation_id.0, legacy_conversation_id);
    let restored_context: Uuid =
        sqlx::query_scalar("SELECT user_context_id FROM conversations WHERE id = $1")
            .bind(legacy_conversation_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(restored_context, charlie_owner.user_context_id.0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn task_tools_scope_every_read_and_write_to_the_resource_owner() {
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

    let bob_project_id: Uuid = sqlx::query_scalar(
        "INSERT INTO projects (user_id, name) VALUES ($1, 'Bob private project') RETURNING id",
    )
    .bind(bob.user_id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let cross_owner_project = CreateTask::new(Some(db.clone()), alice)
        .call(
            &mut context,
            CreateTaskArgs {
                title: "Must not join Bob's project".into(),
                instruction: None,
                project_name: None,
                project_id: Some(bob_project_id.to_string()),
                execution_type: None,
                due_at: None,
            },
        )
        .await;
    assert!(matches!(
        cross_owner_project,
        Err(TaskToolError::NotFound(_))
    ));

    let created = CreateTask::new(Some(db.clone()), alice)
        .call(
            &mut context,
            CreateTaskArgs {
                title: "Alice private task".into(),
                instruction: None,
                project_name: None,
                project_id: None,
                execution_type: None,
                due_at: None,
            },
        )
        .await
        .unwrap();
    let task_id = created["task_id"].as_str().unwrap().to_owned();

    let bob_list = ListTasks::new(Some(db.clone()), bob)
        .call(
            &mut context,
            ListTasksArgs {
                status: Some("all".into()),
                project_id: None,
                limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(bob_list["tasks"].as_array().unwrap().len(), 0);

    let bob_get = GetTask::new(Some(db.clone()), bob)
        .call(
            &mut context,
            GetTaskArgs {
                task_id: Some(task_id.clone()),
                title_query: None,
            },
        )
        .await;
    assert!(matches!(bob_get, Err(TaskToolError::NotFound(_))));

    let bob_update = UpdateTask::new(Some(db.clone()), bob)
        .call(
            &mut context,
            UpdateTaskArgs {
                task_id: task_id.clone(),
                status: Some("cancelled".into()),
                feasibility_reasoning: None,
                execution_result: None,
            },
        )
        .await;
    assert!(matches!(bob_update, Err(TaskToolError::NotFound(_))));

    UpdateTask::new(Some(db.clone()), alice)
        .call(
            &mut context,
            UpdateTaskArgs {
                task_id: task_id.clone(),
                status: Some("completed".into()),
                feasibility_reasoning: None,
                execution_result: None,
            },
        )
        .await
        .unwrap();

    let stored_owner: (Uuid, Uuid) =
        sqlx::query_as("SELECT user_context_id, user_id FROM tasks WHERE id = $1")
            .bind(Uuid::parse_str(&task_id).unwrap())
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored_owner, (alice.user_context_id.0, alice.user_id.0));
}
