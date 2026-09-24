use super::*;
use crate::identity::{UserContextId, UserId};

#[test]
fn schedule_outbound_call_tool_metadata() {
    let owner = ResourceOwner {
        user_context_id: UserContextId(Uuid::new_v4()),
        user_id: UserId(Uuid::new_v4()),
    };
    let tool = ScheduleOutboundCall::new(None, None, owner);
    assert_eq!(ScheduleOutboundCall::NAME, "schedule_outbound_call");
    assert!(
        tool.description()
            .contains("Schedule an outbound phone call")
    );
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["delay_minutes"].is_object());
    assert!(params["properties"]["reason"].is_object());
    assert!(params["properties"]["opening_instruction"].is_object());
}

#[test]
fn trigger_outbound_call_tool_metadata() {
    let owner = ResourceOwner {
        user_context_id: UserContextId(Uuid::new_v4()),
        user_id: UserId(Uuid::new_v4()),
    };
    let tool = TriggerOutboundCall::new(None, None, owner);
    assert_eq!(TriggerOutboundCall::NAME, "trigger_outbound_call");
    assert!(tool.description().contains("outbound phone call"));
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert!(params["properties"]["reason"].is_object());
    assert!(params["properties"]["opening_instruction"].is_object());
}
