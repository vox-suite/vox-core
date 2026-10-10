use serde_json::json;
use std::time::Duration;
use uuid::Uuid;
use vox_core::realtime::DeviceHub;
#[tokio::test]
async fn old_socket_cannot_acknowledge_a_reconnected_device() {
    let hub = DeviceHub::new();
    let device = Uuid::new_v4();
    let (old, _) = hub.register(device);
    let (new, mut outgoing) = hub.register(device);
    let link = hub.get(device).unwrap();
    let request = tokio::spawn(async move {
        link.request("app_command", json!({}), Duration::from_millis(100))
            .await
    });
    let frame: serde_json::Value = serde_json::from_str(&outgoing.recv().await.unwrap()).unwrap();
    hub.resolve_from_generation(device, old, &json!({"id":frame["id"],"ok":true}));
    assert!(!request.is_finished());
    hub.resolve_from_generation(device, new, &json!({"id":frame["id"],"ok":true}));
    assert_eq!(request.await.unwrap().unwrap()["ok"], true);
    hub.unregister(device, old);
    assert!(hub.get(device).is_some());
}
