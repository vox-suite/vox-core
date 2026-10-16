use chrono::{TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::{domain::projects::ProjectData, storage::projects::ProjectRepository};

async fn database() -> PgPool {
    let pool = PgPool::connect(&std::env::var("VOX_PROJECTS_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    pool
}

#[tokio::test]
#[ignore = "requires isolated VOX_PROJECTS_TEST_DATABASE_URL"]
async fn project_list_has_lookahead_and_preserves_summary_and_cursor_scope() {
    let pool = database().await;
    let repo = ProjectRepository(pool.clone());
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1),($2)")
        .bind(user)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let at = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let mut ids = Vec::new();
    for _ in 0..52 {
        let id = Uuid::new_v4();
        // Heavy fields are intentionally opaque: the list must exclude them before decoding.
        sqlx::query("INSERT INTO projects(id,user_id,name,updated_at,data) VALUES($1,$2,'Fixture',$3,'{\"template\":\"engineering\",\"notes\":\"Summary\",\"sources\":[],\"nodes\":[{\"large\":true}],\"messages\":[{\"large\":true}],\"activity\":[{\"large\":true}]}')")
            .bind(id).bind(user).bind(at).execute(&pool).await.unwrap();
        ids.push(id);
    }
    ids.sort_by(|a, b| b.cmp(a));
    let foreign = repo
        .create(other, "Other", &ProjectData::default())
        .await
        .unwrap();
    let first = repo.list(user, None).await.unwrap();
    assert_eq!(
        first.len(),
        51,
        "the HTTP handler needs one lookahead row to distinguish exactly 50 from more than 50"
    );
    assert_eq!(first.iter().map(|p| p.id).collect::<Vec<_>>(), ids[..51]);
    assert!(first.iter().all(|p| p.data.nodes.is_empty()
        && p.data.messages.is_empty()
        && p.data.activity.is_empty()));
    assert_eq!(first[0].data.notes, "Summary");
    assert_eq!(first[0].data.template, "engineering");
    let second = repo.list(user, Some(ids[49])).await.unwrap();
    assert_eq!(second.iter().map(|p| p.id).collect::<Vec<_>>(), ids[50..]);
    assert!(repo.list(user, Some(foreign.id)).await.unwrap().is_empty());
    assert!(
        repo.list(user, Some(Uuid::new_v4()))
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE projects SET archived=true WHERE id=ANY($1)")
        .bind(&ids[..2])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(repo.list(user, None).await.unwrap().len(), 50);
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(&[user, other][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated VOX_PROJECTS_TEST_DATABASE_URL"]
async fn project_updates_are_tenant_scoped_and_reject_stale_revisions() {
    let pool = database().await;
    let repo = ProjectRepository(pool.clone());
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1),($2)")
        .bind(user)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let data = ProjectData {
        notes: "Original".into(),
        ..Default::default()
    };
    let project = repo.create(user, "Fixture", &data).await.unwrap();
    assert!(repo.exists(user, project.id).await.unwrap());
    assert!(!repo.exists(other, project.id).await.unwrap());
    assert!(!repo.exists(user, Uuid::new_v4()).await.unwrap());
    assert!(repo.get(other, project.id).await.unwrap().is_none());
    assert!(
        repo.update(other, project.id, project.revision, "Wrong user", &data)
            .await
            .unwrap()
            .is_none()
    );
    let next = repo
        .update(user, project.id, project.revision, "Updated", &data)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.revision, project.revision + 1);
    assert!(
        repo.update(user, project.id, project.revision, "Stale", &data)
            .await
            .unwrap()
            .is_none()
    );
    let (a, b) = tokio::join!(
        repo.update(user, project.id, next.revision, "Concurrent A", &data),
        repo.update(user, project.id, next.revision, "Concurrent B", &data),
    );
    assert_eq!(
        usize::from(a.unwrap().is_some()) + usize::from(b.unwrap().is_some()),
        1
    );
    sqlx::query("UPDATE projects SET archived=true WHERE id=$1")
        .bind(project.id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!repo.exists(user, project.id).await.unwrap());
    assert!(repo.get(user, project.id).await.unwrap().is_none());
    assert!(
        repo.update(user, project.id, next.revision + 1, "Archived", &data)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(&[user, other][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires VOX_PROJECTS_TEST_DATABASE_URL and scripts/projects-read-benchmark.sql fixtures"]
async fn benchmark_project_ownership_reads() {
    let pool = database().await;
    let repo = ProjectRepository(pool.clone());
    let user = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();
    let id: Uuid = sqlx::query_scalar("SELECT md5('projects-benchmark-100')::uuid")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut full = Vec::new();
    let mut ownership = Vec::new();
    for i in 0..35 {
        let start = std::time::Instant::now();
        let project = repo
            .get(user, id)
            .await
            .unwrap()
            .expect("seed the benchmark first");
        let full_time = start.elapsed().as_micros();
        let start = std::time::Instant::now();
        assert!(repo.exists(user, id).await.unwrap());
        let exists_time = start.elapsed().as_micros();
        assert_eq!(project.data.nodes.len(), 32);
        if i >= 5 {
            full.push(full_time);
            ownership.push(exists_time);
        }
    }
    full.sort_unstable();
    ownership.sort_unstable();
    println!(
        "local debug repository reads, 30 warmed samples: full get median={}us p95={}us; exists median={}us p95={}us",
        full[15], full[28], ownership[15], ownership[28]
    );
}
