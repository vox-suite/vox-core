/**
 * Integration and clock-controlled tests for reminders (E40).
 *
 * Verifies:
 * - One-time, interval, and calendar recurrence preserve timezone intent across
 *   restarts and daylight-saving boundaries.
 * - Delivery status truthfully distinguishes: scheduled, delivered_to_channel, failed, unknown, and missed.
 * - Missed reminders beyond the grace period window are never silently delivered late.
 * - Reminders never authorize another action or execute consequential writes.
 * - HTTP endpoints require signed host assertions.
 */
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest, RegisteredHostApp},
    http::{AppState, router},
    identity::ResolvedUserContext,
    reminders::{
        CreateReminderRequest, MockReminderChannelAdapter, ReminderDeliveryOutcome,
        ReminderDeliveryStatus, ReminderError, ReminderScheduleKind, ReminderScheduler,
        ReminderService, ReminderStatus, compute_next_calendar,
    },
};

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn create_context(
    db: &Db,
    user_id: &str,
) -> (
    String,
    ResolvedUserContext,
    RegisteredHostApp,
    HostContextRequest,
) {
    let trust = HostTrustService::new(db.clone());
    let deployment = format!("dep-{}", Uuid::new_v4());
    let host_app = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment.clone(),
            host_app_external_key: "vox-web".into(),
            allowed_origins: vec!["https://voxagent.in".into()],
        })
        .await
        .unwrap();

    let host_req = HostContextRequest {
        host_user_id: user_id.into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host_app
        .credential
        .sign_context_request(&host_req, now, Uuid::new_v4())
        .unwrap();
    let context = trust
        .resolve_authenticated_context(&assertion, &host_req, Some("https://voxagent.in"), now)
        .await
        .unwrap();

    (deployment, context, host_app, host_req)
}

#[tokio::test]
async fn calendar_recurrence_preserves_wall_clock_across_daylight_saving_transitions() {
    let tz_ny: Tz = "America/New_York".parse().unwrap();
    let expr = "0 0 9 * * * *"; // Daily at 9:00:00 AM

    // 1. Summer (EDT, UTC-4): 2026-07-15 00:00:00 UTC
    let summer = Utc.with_ymd_and_hms(2026, 7, 15, 0, 0, 0).unwrap();
    let next_summer = compute_next_calendar(expr, tz_ny, summer).unwrap();
    // 9:00 AM EDT is 13:00 UTC
    assert_eq!(
        next_summer,
        Utc.with_ymd_and_hms(2026, 7, 15, 13, 0, 0).unwrap()
    );
    let local_summer = next_summer.with_timezone(&tz_ny);
    assert_eq!(local_summer.format("%H:%M:%S").to_string(), "09:00:00");

    // 2. Across Fall DST Transition (EDT -> EST):
    // In 2026, US Fall DST occurs on Sunday, Nov 1, 2026.
    // Saturday Oct 31, 2026: EDT (UTC-4)
    let oct_31 = Utc.with_ymd_and_hms(2026, 10, 31, 14, 0, 0).unwrap();
    let nov_1 = compute_next_calendar(expr, tz_ny, oct_31).unwrap();
    // Nov 1 2026 9:00 AM EST is 14:00 UTC (clock fell back 1 hour)
    assert_eq!(nov_1, Utc.with_ymd_and_hms(2026, 11, 1, 14, 0, 0).unwrap());
    let local_nov_1 = nov_1.with_timezone(&tz_ny);
    assert_eq!(local_nov_1.format("%H:%M:%S").to_string(), "09:00:00");

    // 3. Winter (EST, UTC-5): 2026-12-15 00:00:00 UTC
    let winter = Utc.with_ymd_and_hms(2026, 12, 15, 0, 0, 0).unwrap();
    let next_winter = compute_next_calendar(expr, tz_ny, winter).unwrap();
    // 9:00 AM EST is 14:00 UTC
    assert_eq!(
        next_winter,
        Utc.with_ymd_and_hms(2026, 12, 15, 14, 0, 0).unwrap()
    );
    let local_winter = next_winter.with_timezone(&tz_ny);
    assert_eq!(local_winter.format("%H:%M:%S").to_string(), "09:00:00");

    // 4. Across Spring DST Transition (EST -> EDT):
    // In 2027, US Spring DST occurs on Sunday, March 14, 2027.
    // Saturday March 13, 2027: EST (UTC-5)
    let march_13 = Utc.with_ymd_and_hms(2027, 3, 13, 15, 0, 0).unwrap();
    let march_14 = compute_next_calendar(expr, tz_ny, march_13).unwrap();
    // March 14 2027 9:00 AM EDT is 13:00 UTC (clock sprang forward 1 hour)
    assert_eq!(
        march_14,
        Utc.with_ymd_and_hms(2027, 3, 14, 13, 0, 0).unwrap()
    );
    let local_march_14 = march_14.with_timezone(&tz_ny);
    assert_eq!(local_march_14.format("%H:%M:%S").to_string(), "09:00:00");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn one_time_and_interval_reminders_preserve_timing_and_record_delivery() {
    let db = setup().await;
    let (_, context, _, _) = create_context(&db, "user-reminder-1").await;
    let service = ReminderService::new(db.clone());
    let adapter = Arc::new(MockReminderChannelAdapter::new());
    let scheduler = ReminderScheduler::new(db.clone(), adapter.clone());

    let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();

    // 1. One-Time Reminder
    let one_time = service
        .create(
            &context,
            CreateReminderRequest {
                title: "Doctor Appointment".into(),
                message: "Remember appointment with Dr. Rao at 11am".into(),
                channel: "phone".into(),
                destination: "+15551234567".into(),
                timezone: "America/New_York".into(),
                schedule_kind: ReminderScheduleKind::OneTime,
                run_at: Some(t0 + Duration::minutes(30)),
                interval_seconds: None,
                recurrence_expression: None,
                max_retries: Some(3),
                metadata: None,
            },
            t0,
        )
        .await
        .unwrap();

    assert_eq!(one_time.status, ReminderStatus::Scheduled.as_str());
    assert_eq!(one_time.next_trigger_at, Some(t0 + Duration::minutes(30)));

    // 2. Interval Reminder (every 2 hours)
    let interval = service
        .create(
            &context,
            CreateReminderRequest {
                title: "Drink Water".into(),
                message: "Stay hydrated".into(),
                channel: "in_app".into(),
                destination: "vox-user-inbox".into(),
                timezone: "UTC".into(),
                schedule_kind: ReminderScheduleKind::Interval,
                run_at: None,
                interval_seconds: Some(7200),
                recurrence_expression: None,
                max_retries: Some(3),
                metadata: None,
            },
            t0,
        )
        .await
        .unwrap();

    assert_eq!(interval.next_trigger_at, Some(t0 + Duration::seconds(7200)));

    // Advance clock to t0 + 30m: One-time reminder is due!
    let t1 = t0 + Duration::minutes(30);
    let processed = scheduler
        .process_due_reminders_for_context(context.id.0, t1)
        .await
        .unwrap();
    assert_eq!(processed, 1);

    // Verify adapter received dispatch
    let dispatched = adapter.get_dispatched();
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].0, one_time.id);

    // Verify one-time reminder status transitioned to delivered_to_channel
    let updated_one_time = service.get(&context, one_time.id).await.unwrap();
    assert_eq!(
        updated_one_time.status,
        ReminderStatus::DeliveredToChannel.as_str()
    );
    assert_eq!(updated_one_time.delivered_at, Some(t1));

    // Verify delivery history was written
    let deliveries = service.get_deliveries(&context, one_time.id).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].status,
        ReminderDeliveryStatus::DeliveredToChannel.as_str()
    );
    assert!(deliveries[0].provider_receipt_id.is_some());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn missed_reminder_policy_never_silently_delivers_materially_late_occurrences() {
    let db = setup().await;
    let (_, context, _, _) = create_context(&db, "user-reminder-missed").await;
    let service = ReminderService::new(db.clone());
    let adapter = Arc::new(MockReminderChannelAdapter::new());
    // 15-minute grace period
    let scheduler = ReminderScheduler::new(db.clone(), adapter.clone())
        .with_grace_period(Duration::minutes(15));

    let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();

    // Scheduled for 10:30 AM
    let reminder = service
        .create(
            &context,
            CreateReminderRequest {
                title: "Take Medicine".into(),
                message: "Antibiotics dose 2".into(),
                channel: "phone".into(),
                destination: "+15559876543".into(),
                timezone: "America/New_York".into(),
                schedule_kind: ReminderScheduleKind::OneTime,
                run_at: Some(t0 + Duration::minutes(30)),
                interval_seconds: None,
                recurrence_expression: None,
                max_retries: Some(3),
                metadata: None,
            },
            t0,
        )
        .await
        .unwrap();

    // System downtime! Clock jumps to 12:00 PM (delay = 90m > 15m grace window)
    let t_recovery = t0 + Duration::hours(2);
    let processed = scheduler
        .process_due_reminders_for_context(context.id.0, t_recovery)
        .await
        .unwrap();
    assert_eq!(processed, 1);

    // CRITICAL INVARIANT: The channel adapter was NOT called! Late reminder was NOT delivered!
    let dispatched = adapter.get_dispatched();
    assert_eq!(dispatched.len(), 0);

    // Reminder status is truthfully marked "missed"
    let updated = service.get(&context, reminder.id).await.unwrap();
    assert_eq!(updated.status, ReminderStatus::Missed.as_str());
    assert!(
        updated
            .failure_reason
            .unwrap()
            .contains("Delivery window expired")
    );

    // Delivery record is marked "missed"
    let deliveries = service.get_deliveries(&context, reminder.id).await.unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].status,
        ReminderDeliveryStatus::Missed.as_str()
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn bounded_retries_and_retry_window_exhaustion() {
    let db = setup().await;
    let (_, context, _, _) = create_context(&db, "user-reminder-retries").await;
    let service = ReminderService::new(db.clone());
    let adapter = Arc::new(MockReminderChannelAdapter::with_forced_outcome(
        ReminderDeliveryOutcome::Failed {
            reason: "Carrier network busy".into(),
            retryable: true,
        },
    ));
    let scheduler = ReminderScheduler::new(db.clone(), adapter.clone());

    let t0 = Utc.with_ymd_and_hms(2026, 9, 24, 10, 0, 0).unwrap();

    let reminder = service
        .create(
            &context,
            CreateReminderRequest {
                title: "Call Mom".into(),
                message: "Wish birthday".into(),
                channel: "phone".into(),
                destination: "+15553334444".into(),
                timezone: "UTC".into(),
                schedule_kind: ReminderScheduleKind::OneTime,
                run_at: Some(t0 + Duration::minutes(10)),
                interval_seconds: None,
                recurrence_expression: None,
                max_retries: Some(3),
                metadata: None,
            },
            t0,
        )
        .await
        .unwrap();

    // 1st attempt at 10:10 AM
    let t1 = t0 + Duration::minutes(10);
    scheduler
        .process_due_reminders_for_context(context.id.0, t1)
        .await
        .unwrap();
    let r1 = service.get(&context, reminder.id).await.unwrap();
    assert_eq!(r1.status, ReminderStatus::Scheduled.as_str());
    assert_eq!(r1.retry_count, 1);
    // Backoff 1 min -> 10:11 AM
    assert_eq!(r1.next_trigger_at, Some(t1 + Duration::seconds(60)));

    // 2nd attempt at 10:11 AM
    let t2 = t1 + Duration::seconds(60);
    scheduler
        .process_due_reminders_for_context(context.id.0, t2)
        .await
        .unwrap();
    let r2 = service.get(&context, reminder.id).await.unwrap();
    assert_eq!(r2.status, ReminderStatus::Scheduled.as_str());
    assert_eq!(r2.retry_count, 2);
    // Backoff 5 mins -> 10:16 AM
    assert_eq!(r2.next_trigger_at, Some(t2 + Duration::seconds(300)));

    // 3rd attempt at 10:16 AM -> max_retries (3) reached!
    let t3 = t2 + Duration::seconds(300);
    scheduler
        .process_due_reminders_for_context(context.id.0, t3)
        .await
        .unwrap();
    let r3 = service.get(&context, reminder.id).await.unwrap();
    assert_eq!(r3.status, ReminderStatus::Failed.as_str());

    let deliveries = service.get_deliveries(&context, reminder.id).await.unwrap();
    assert_eq!(deliveries.len(), 3);
    assert_eq!(
        deliveries[0].status,
        ReminderDeliveryStatus::Failed.as_str()
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn reminders_strictly_prohibit_action_authority() {
    let db = setup().await;
    let (_, context, _, _) = create_context(&db, "user-reminder-auth-guard").await;
    let service = ReminderService::new(db.clone());

    let now = Utc::now();

    // Attempting to attach action_id or consequential execution
    let malformed_attempt = service
        .create(
            &context,
            CreateReminderRequest {
                title: "Malicious Reminder".into(),
                message: "Attempting to trigger write".into(),
                channel: "in_app".into(),
                destination: "box".into(),
                timezone: "UTC".into(),
                schedule_kind: ReminderScheduleKind::OneTime,
                run_at: Some(now + Duration::hours(1)),
                interval_seconds: None,
                recurrence_expression: None,
                max_retries: Some(3),
                metadata: Some(json!({
                    "action_id": "act_malicious_123",
                    "execute_consequential": true,
                })),
            },
            now,
        )
        .await;

    assert!(matches!(
        malformed_attempt,
        Err(ReminderError::ActionAuthorityProhibited)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn http_reminder_endpoints_require_signed_host_assertions() {
    let db = setup().await;
    let (_, _context, host_app, host_req) = create_context(&db, "user-reminder-http").await;
    let app = router(AppState::with_host_trust(db.clone(), "".into()));

    let now = Utc::now();
    let assertion = host_app
        .credential
        .sign_context_request(&host_req, now, Uuid::new_v4())
        .unwrap();

    let create_payload = json!({
        "host_context": host_req,
        "title": "Board Meeting",
        "message": "Prepare Q3 deck",
        "channel": "phone",
        "destination": "+14155552671",
        "timezone": "America/New_York",
        "schedule_kind": "one_time",
        "run_at": (now + Duration::hours(3)).to_rfc3339(),
        "max_retries": 3
    });

    let req = Request::builder()
        .method("POST")
        .uri("/v1/reminders")
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", assertion.secret())
        .header("x-vox-host-audience", assertion.audience())
        .header(
            "x-vox-host-timestamp",
            assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", assertion.nonce().to_string())
        .header("x-vox-host-signature", assertion.signature())
        .body(Body::from(create_payload.to_string()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["title"], "Board Meeting");
    assert_eq!(val["status"], "scheduled");

    let reminder_id = val["id"].as_str().unwrap();

    // Cancel reminder via HTTP
    let cancel_assertion = host_app
        .credential
        .sign_context_request(&host_req, Utc::now(), Uuid::new_v4())
        .unwrap();

    let cancel_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/reminders/{reminder_id}/cancel"))
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            cancel_assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", cancel_assertion.secret())
        .header("x-vox-host-audience", cancel_assertion.audience())
        .header(
            "x-vox-host-timestamp",
            cancel_assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", cancel_assertion.nonce().to_string())
        .header("x-vox-host-signature", cancel_assertion.signature())
        .body(Body::from(json!({ "host_context": host_req }).to_string()))
        .unwrap();

    let cancel_resp = app.clone().oneshot(cancel_req).await.unwrap();
    assert_eq!(cancel_resp.status(), StatusCode::OK);
    let cancel_bytes = axum::body::to_bytes(cancel_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let cancel_val: serde_json::Value = serde_json::from_slice(&cancel_bytes).unwrap();
    assert_eq!(cancel_val["status"], "cancelled");

    // Report delivery callback via HTTP
    let callback_assertion = host_app
        .credential
        .sign_context_request(&host_req, Utc::now(), Uuid::new_v4())
        .unwrap();

    let callback_req = Request::builder()
        .method("POST")
        .uri(format!("/v1/reminders/{reminder_id}/delivery-callback"))
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            callback_assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", callback_assertion.secret())
        .header("x-vox-host-audience", callback_assertion.audience())
        .header(
            "x-vox-host-timestamp",
            callback_assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", callback_assertion.nonce().to_string())
        .header("x-vox-host-signature", callback_assertion.signature())
        .body(Body::from(
            json!({
                "host_context": host_req,
                "status": "delivered_to_channel",
                "channel": "phone",
                "destination": "+14155552671",
                "provider_receipt_id": "CA1234567890abcdef",
                "failure_reason": null,
                "attempted_at": Utc::now().to_rfc3339()
            })
            .to_string(),
        ))
        .unwrap();

    let callback_resp = app.oneshot(callback_req).await.unwrap();
    assert_eq!(callback_resp.status(), StatusCode::CREATED);
    let cb_bytes = axum::body::to_bytes(callback_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let cb_val: serde_json::Value = serde_json::from_slice(&cb_bytes).unwrap();
    assert_eq!(cb_val["status"], "delivered_to_channel");
    assert_eq!(cb_val["provider_receipt_id"], "CA1234567890abcdef");
}
