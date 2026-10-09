use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::storage::{
    space_tasks::{TaskProposal, TaskRepository},
    spaces::SpaceRepository,
};
#[tokio::test]
#[ignore = "requires isolated VOX_SPACE_TEST_DATABASE_URL"]
async fn durable_forks_joins_leases_and_cancellation() {
    let pool = PgPool::connect(
        &std::env::var("VOX_SPACE_TEST_DATABASE_URL").expect("isolated database required"),
    )
    .await
    .unwrap();
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1)")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let spaces = SpaceRepository::new(pool.clone());
    let tasks = TaskRepository::new(pool.clone());
    let space = spaces
        .create_workflow_space(user, "Trip", "Plan a trip")
        .await
        .unwrap();
    let graph = spaces.get_graph(user, space.id).await.unwrap().unwrap();
    assert_eq!(graph.nodes.len(), 1);
    let root = graph.nodes[0].id;
    let proposal = |role: &str, deps: Vec<Uuid>| TaskProposal {
        role: role.into(),
        title: role.into(),
        brief: format!("Do {role}"),
        dependencies: deps,
    };
    let web = tasks
        .spawn(space.id, &proposal("web_search", vec![root]), 24, 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        Some(web),
        tasks
            .spawn(space.id, &proposal("web_search", vec![root]), 24, 12)
            .await
            .unwrap()
    );
    let data = tasks
        .spawn(space.id, &proposal("user_data", vec![root]), 24, 12)
        .await
        .unwrap()
        .unwrap();
    let join = tasks
        .spawn(space.id, &proposal("synthesis", vec![web, data]), 24, 12)
        .await
        .unwrap()
        .unwrap();
    let (a, b) = tokio::join!(tasks.claim(space.id), tasks.claim(space.id));
    let a = a.unwrap().unwrap();
    let b = b.unwrap().unwrap();
    assert_ne!(a.node_id, b.node_id);
    assert!(tasks.claim(space.id).await.unwrap().is_none());
    assert!(
        tasks
            .finish(&a, &json!({"summary":"Result","evidence":[]}), true)
            .await
            .unwrap()
    );
    assert!(tasks.claim(space.id).await.unwrap().is_none());
    assert!(
        tasks
            .finish(&b, &json!({"summary":"Other result","evidence":[]}), true)
            .await
            .unwrap()
    );
    let c = tasks.claim(space.id).await.unwrap().unwrap();
    assert_eq!(c.node_id, join);
    assert!(spaces.add_edge(space.id, join, root).await.is_err());
    let foreign = spaces
        .create_workflow_space(user, "Other", "Other")
        .await
        .unwrap();
    assert!(
        tasks
            .spawn(foreign.id, &proposal("plan", vec![root]), 24, 12)
            .await
            .is_err()
    );
    tasks.stop(space.id).await.unwrap();
    assert!(
        tasks
            .spawn_generation(space.id, &proposal("web_search", vec![root]), 24, 12, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!tasks.heartbeat(&c).await.unwrap());
    assert!(
        !tasks
            .finish(&c, &json!({"summary":"Late"}), true)
            .await
            .unwrap()
    );
    assert!(
        tasks
            .spawn(space.id, &proposal("plan", vec![join]), 24, 12)
            .await
            .unwrap()
            .is_none()
    );
    tasks.retry(space.id, join).await.unwrap();
    let retry = tasks.claim(space.id).await.unwrap().unwrap();
    assert_eq!(retry.node_id, join);
    sqlx::query("UPDATE space_nodes SET version=version+1 WHERE id=$1")
        .bind(web)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !tasks
            .finish(&retry, &json!({"summary":"Stale"}), true)
            .await
            .unwrap()
    );
    tasks.retry(space.id, join).await.unwrap();
    let lease = tasks.claim(space.id).await.unwrap().unwrap();
    sqlx::query(
        "UPDATE space_tasks SET lease_expires_at=now()-interval '1 second' WHERE node_id=$1",
    )
    .bind(lease.node_id)
    .execute(&pool)
    .await
    .unwrap();
    let recovered = tasks.claim(space.id).await.unwrap().unwrap();
    assert_ne!(lease.token, recovered.token);
    assert!(!tasks.heartbeat(&lease).await.unwrap());
    tasks
        .finish(&recovered, &json!({"error":"Failed"}), false)
        .await
        .unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM space_tasks WHERE node_id=$1")
        .bind(join)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}
