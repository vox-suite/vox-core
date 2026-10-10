use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::desktop::actions::{ResourceActions, ResourceRequest};
#[tokio::test]
#[ignore = "requires isolated VOX_DESKTOP_TEST_DATABASE_URL"]
async fn resource_writes_are_owned_versioned_and_idempotent() {
    let pool = PgPool::connect(&std::env::var("VOX_DESKTOP_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [user, other] {
        sqlx::query("INSERT INTO users(id) VALUES($1)")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let service = ResourceActions::new(pool.clone());
    let key = Uuid::new_v4();
    let create = ResourceRequest {
        command_id: key,
        action: "create_space".into(),
        space_id: None,
        arguments: json!({"intent":"Plan Japan","title":"Japan"}),
    };
    let a = service.execute(user, create.clone()).await.unwrap();
    let b = service.execute(user, create).await.unwrap();
    assert_eq!(a, b);
    let space = Uuid::parse_str(a["space_id"].as_str().unwrap()).unwrap();
    let add = ResourceRequest {
        command_id: Uuid::new_v4(),
        action: "add_node".into(),
        space_id: Some(space),
        arguments: json!({"title":"Budget","kind":"budget","body":"1000"}),
    };
    assert!(service.execute(other, add.clone()).await.is_err());
    let node = service.execute(user, add).await.unwrap();
    let node_id = node["node_id"].as_str().unwrap();
    let update = ResourceRequest {
        command_id: Uuid::new_v4(),
        action: "update_node".into(),
        space_id: Some(space),
        arguments: json!({"nodeId":node_id,"expectedVersion":0,"title":"Wrong"}),
    };
    assert!(service.execute(user, update).await.is_err());
    let chat = ResourceRequest {
        command_id: Uuid::new_v4(),
        action: "space_chat".into(),
        space_id: Some(space),
        arguments: json!({"message":"Research flights","nodeId":node_id}),
    };
    let first = service.execute(user, chat.clone()).await.unwrap();
    let again = service.execute(user, chat).await.unwrap();
    assert_eq!(first, again);
    let requests: i64 =
        sqlx::query_scalar("SELECT count(*) FROM space_workflow_requests WHERE space_id=$1")
            .bind(space)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(requests, 1);
    sqlx::query("UPDATE spaces SET state='committed' WHERE id=$1")
        .bind(space)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        service
            .execute(
                user,
                ResourceRequest {
                    command_id: Uuid::new_v4(),
                    action: "add_node".into(),
                    space_id: Some(space),
                    arguments: json!({"title":"Blocked","kind":"idea"})
                }
            )
            .await
            .is_err()
    );
}
