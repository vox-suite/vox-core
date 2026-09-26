/**
* Integration tests for outbound calling workflow.
*/
use async_trait::async_trait;
use chrono::Utc;
use rig::tool::Tool;
use sqlx::Row;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use vox_core::{
    agents::{
        event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
        tools::calls::{
            ScheduleCallArgs, ScheduleOutboundCall, TriggerCallArgs, TriggerOutboundCall,
        },
    },
    bridge_client::{BridgeError, OutboundBridge, OutboundCallRequest, OutboundCallResponse},
    db::Db,
    identity::{ChannelIdentity, IdentityService, ResourceOwner},
    outbound::OutboundCallService,
    schedules::{handler::ScheduleHandler, ticker::ScheduleTicker},
};

#[derive(Clone, Default)]
struct MockBridge {
    calls: Arc<Mutex<Vec<OutboundCallRequest>>>,
}

#[async_trait]
impl OutboundBridge for MockBridge {
    async fn initiate_outbound_call(
        &self,
        request: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeError> {
        let call_id = format!("CA_{}", Uuid::new_v4().simple());
        self.calls.lock().unwrap().push(request);
        Ok(OutboundCallResponse {
            provider_call_id: call_id,
        })
    }
}

struct DummyPlanner;

#[async_trait]
impl EventPlanning for DummyPlanner {
    async fn plan(
        &self,
        _: EventPlanningPrompt,
    ) -> Result<Vec<PlannedAction>, vox_core::agents::AgentError> {
        Ok(vec![])
    }
}

async fn setup() -> (Db, ResourceOwner, String) {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();

    let phone = format!("+1555{:08}", Uuid::new_v4().as_u128() % 100_000_000);
    let owner = IdentityService::new(db.clone())
        .resolve_legacy_owner(&ChannelIdentity {
            channel: "phone".into(),
            external_id: phone.clone(),
        })
        .await
        .unwrap();

    (db, owner, phone)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn outbound_service_creates_records_and_dispatches_bridge_call() {
    let (db, owner, phone) = setup().await;
    let mock_bridge = Arc::new(MockBridge::default());
    let service = OutboundCallService::new(db.clone(), Some(mock_bridge.clone()));

    let record = service
        .initiate_call_for_user(
            owner,
            "Clean room reminder",
            "Remind the user to clean their room as requested 5 minutes ago",
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(record.phone_number, phone);
    assert_eq!(record.reason, "Clean room reminder");
    assert_eq!(record.state, "in_progress");
    assert!(record.provider_call_id.is_some());

    let row = sqlx::query(
        "SELECT checkpoint->>'phone_number' AS phone_number, \
         checkpoint->>'reason' AS reason, \
         checkpoint->>'state' AS state, \
         checkpoint->>'provider_call_id' AS provider_call_id \
         FROM jobs WHERE kind = 'dispatch_action' AND checkpoint->>'call_id' = $1",
    )
    .bind(record.id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();

    let db_phone: String = row.get("phone_number");
    let db_reason: String = row.get("reason");
    let db_state: String = row.get("state");

    assert_eq!(db_phone, phone);
    assert_eq!(db_reason, "Clean room reminder");
    assert_eq!(db_state, "in_progress");

    let calls = mock_bridge.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].identity.external_id, phone);
    assert_eq!(calls[0].reason, "Clean room reminder");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn schedule_outbound_call_tool_schedules_and_executes_reminder_call() {
    let (db, owner, phone) = setup().await;
    let mock_bridge = Arc::new(MockBridge::default());
    let service = Arc::new(OutboundCallService::new(
        db.clone(),
        Some(mock_bridge.clone()),
    ));

    let tool = ScheduleOutboundCall::new(Some(db.clone()), Some(service.clone()), owner);

    let output = tool
        .call(
            &mut rig::prelude::ToolContext::default(),
            ScheduleCallArgs {
                delay_seconds: Some(-1),
                delay_minutes: None,
                run_at: None,
                reason: "Clean bedroom".into(),
                opening_instruction: "Remind user to clean bedroom".into(),
                phone_number: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(output["status"], "scheduled");
    let schedule_id: Uuid = output["schedule_id"].as_str().unwrap().parse().unwrap();
    let span_id: Uuid = output["span_id"].as_str().unwrap().parse().unwrap();

    let st = sqlx::query("SELECT state, instruction FROM scheduled_tasks WHERE id = $1")
        .bind(schedule_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let st_state: String = st.get("state");
    let st_instruction: String = st.get("instruction");
    assert_eq!(st_state, "active");
    assert_eq!(st_instruction, "Remind user to clean bedroom");

    let t = sqlx::query("SELECT status, title, execution_type FROM spans WHERE id = $1")
        .bind(span_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let t_status: String = t.get("status");
    let t_title: String = t.get("title");
    let t_exec: String = t.get("execution_type");
    assert_eq!(t_status, "planned");
    assert_eq!(t_title, "Clean bedroom");
    assert_eq!(t_exec, "autonomous");

    let ticker = ScheduleTicker::new(db.clone());
    let count = ticker.tick(Utc::now()).await.unwrap();
    assert!(count >= 1);

    let handler =
        ScheduleHandler::new(db.clone(), Arc::new(DummyPlanner)).with_outbound(service.clone());

    handler
        .handle(vox_core::schedules::ScheduleId(schedule_id), Utc::now())
        .await
        .unwrap();

    {
        let calls = mock_bridge.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].identity.external_id, phone);
        assert!(
            calls[0]
                .opening_instruction
                .contains("Remind user to clean bedroom")
        );
    }

    let completed_task = sqlx::query("SELECT status FROM spans WHERE id = $1")
        .bind(span_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let comp_status: String = completed_task.get("status");
    assert_eq!(comp_status, "done");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn trigger_outbound_call_tool_places_immediate_call() {
    let (db, owner, phone) = setup().await;
    let mock_bridge = Arc::new(MockBridge::default());
    let service = Arc::new(OutboundCallService::new(
        db.clone(),
        Some(mock_bridge.clone()),
    ));

    let tool = TriggerOutboundCall::new(Some(db.clone()), Some(service.clone()), owner);

    let output = tool
        .call(
            &mut rig::prelude::ToolContext::default(),
            TriggerCallArgs {
                reason: "Emergency water leak".into(),
                opening_instruction: "Alert user about water leak".into(),
                phone_number: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(output["status"], "call_initiated");
    assert_eq!(output["phone_number"], phone);

    let calls = mock_bridge.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].identity.external_id, phone);
    assert_eq!(calls[0].reason, "Emergency water leak");
    assert_eq!(calls[0].opening_instruction, "Alert user about water leak");
}
