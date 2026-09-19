use uuid::Uuid;
use vox_core::{
    db::Db,
    identity::{
        DeploymentId, HostAppId, HostOrganizationId, IdentityError, IdentityService,
        UserContextSubject,
    },
};

async fn register_deployment(db: &Db, external_key: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO platform_deployments (external_key) VALUES ($1) RETURNING id")
        .bind(external_key)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn register_host(db: &Db, deployment_id: Uuid, external_key: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO host_apps (deployment_id, external_key) VALUES ($1, $2) RETURNING id",
    )
    .bind(deployment_id)
    .bind(external_key)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

async fn register_organization(
    db: &Db,
    deployment_id: Uuid,
    host_app_id: Uuid,
    external_key: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO host_organizations (deployment_id, host_app_id, external_key) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(deployment_id)
    .bind(host_app_id)
    .bind(external_key)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn canonical_contexts_isolate_deployments_hosts_organizations_and_users() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE platform_deployments, users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();

    let deployment_a = register_deployment(&db, "deployment-a").await;
    let deployment_b = register_deployment(&db, "deployment-b").await;
    let host_a = register_host(&db, deployment_a, "host-a").await;
    let host_b = register_host(&db, deployment_a, "host-b").await;
    let host_c = register_host(&db, deployment_b, "host-a").await;
    let organization_a = register_organization(&db, deployment_a, host_a, "organization-a").await;
    let organization_b = register_organization(&db, deployment_a, host_a, "organization-b").await;
    let identities = IdentityService::new(db.clone());

    let concurrent_subject = UserContextSubject {
        deployment_id: DeploymentId(deployment_a),
        host_app_id: HostAppId(host_a),
        organization_id: None,
        host_user_id: "concurrent-user".into(),
    };
    let (concurrent_left, concurrent_right) = tokio::join!(
        identities.resolve_context(&concurrent_subject),
        identities.resolve_context(&concurrent_subject)
    );
    assert_eq!(concurrent_left.unwrap(), concurrent_right.unwrap());

    let subjects = [
        UserContextSubject {
            deployment_id: DeploymentId(deployment_a),
            host_app_id: HostAppId(host_a),
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: DeploymentId(deployment_a),
            host_app_id: HostAppId(host_b),
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: DeploymentId(deployment_b),
            host_app_id: HostAppId(host_c),
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: DeploymentId(deployment_a),
            host_app_id: HostAppId(host_a),
            organization_id: Some(HostOrganizationId(organization_a)),
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: DeploymentId(deployment_a),
            host_app_id: HostAppId(host_a),
            organization_id: Some(HostOrganizationId(organization_b)),
            host_user_id: "same-user".into(),
        },
    ];

    let mut contexts = Vec::new();
    for subject in &subjects {
        contexts.push(identities.resolve_context(subject).await.unwrap());
    }

    for left in 0..contexts.len() {
        assert_eq!(
            identities.resolve_context(&subjects[left]).await.unwrap(),
            contexts[left],
            "resolving the same subject must be stable"
        );
        identities
            .authorize_context(contexts[left].id, contexts[left].user_id)
            .await
            .unwrap();

        for right in 0..contexts.len() {
            if left == right {
                continue;
            }
            assert_ne!(contexts[left].id, contexts[right].id);
            assert_ne!(contexts[left].user_id, contexts[right].user_id);
            assert!(matches!(
                identities
                    .authorize_context(contexts[left].id, contexts[right].user_id)
                    .await,
                Err(IdentityError::AccessDenied)
            ));
        }
    }

    assert!(matches!(
        identities
            .authorize_context(
                vox_core::identity::UserContextId(Uuid::new_v4()),
                contexts[0].user_id,
            )
            .await,
        Err(IdentityError::AccessDenied)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn context_resolution_rejects_unregistered_or_mismatched_scopes() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE platform_deployments, users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();

    let deployment = register_deployment(&db, "scope-deployment").await;
    let host_a = register_host(&db, deployment, "scope-host-a").await;
    let host_b = register_host(&db, deployment, "scope-host-b").await;
    let organization = register_organization(&db, deployment, host_a, "scope-org").await;
    let identities = IdentityService::new(db.clone());

    let wrong_host = UserContextSubject {
        deployment_id: DeploymentId(deployment),
        host_app_id: HostAppId(host_b),
        organization_id: Some(HostOrganizationId(organization)),
        host_user_id: "user".into(),
    };
    assert!(matches!(
        identities.resolve_context(&wrong_host).await,
        Err(IdentityError::ScopeNotFound)
    ));

    let unrelated_user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let constraint_error = sqlx::query(
        "INSERT INTO user_contexts (\
            deployment_id, host_app_id, organization_id, host_user_id, user_id\
         ) VALUES ($1, $2, $3, 'constraint-user', $4)",
    )
    .bind(deployment)
    .bind(host_b)
    .bind(organization)
    .bind(unrelated_user)
    .execute(db.pool())
    .await
    .unwrap_err();
    assert_eq!(
        constraint_error
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("23503"),
        "database must reject an organization outside the host-app scope"
    );

    let blank_user = UserContextSubject {
        deployment_id: DeploymentId(deployment),
        host_app_id: HostAppId(host_a),
        organization_id: None,
        host_user_id: "   ".into(),
    };
    assert!(matches!(
        identities.resolve_context(&blank_user).await,
        Err(IdentityError::InvalidContext)
    ));

    let oversized_user = UserContextSubject {
        host_user_id: "x".repeat(513),
        ..blank_user
    };
    assert!(matches!(
        identities.resolve_context(&oversized_user).await,
        Err(IdentityError::InvalidContext)
    ));
}
