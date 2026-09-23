/**
* Integration tests for user memory and context isolation.
*/
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostTrustService, RegisterHostAppRequest},
    identity::{
        ChannelIdentity, DeploymentId, HostAppId, HostOrganizationId, IdentityError,
        IdentityService, UserContextSubject,
    },
};

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn auth_identities_isolate_deployment_host_pairs_and_host_users() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    let trust = HostTrustService::new(db.clone());
    let deployment_key_a = format!("identity-a-{}", Uuid::new_v4());
    let deployment_key_b = format!("identity-b-{}", Uuid::new_v4());
    let registered_a = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment_key_a.clone(),
            host_app_external_key: "host-a".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let registered_b = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment_key_a,
            host_app_external_key: "host-b".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let registered_c = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment_key_b,
            host_app_external_key: "host-c".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let deployment_a = registered_a.deployment_id;
    let deployment_b = registered_c.deployment_id;
    let host_a = registered_a.host_app_id;
    let host_b = registered_b.host_app_id;
    let host_c = registered_c.host_app_id;
    let organization_a = HostOrganizationId(sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO host_organizations (deployment_id,host_app_id,external_key) VALUES ($1,$2,'org-a') RETURNING id"
    ).bind(deployment_a.0).bind(host_a.0).fetch_one(db.pool()).await.unwrap());
    let organization_b = HostOrganizationId(sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO host_organizations (deployment_id,host_app_id,external_key) VALUES ($1,$2,'org-b') RETURNING id"
    ).bind(deployment_a.0).bind(host_a.0).fetch_one(db.pool()).await.unwrap());
    let identities = IdentityService::new(db.clone());

    let concurrent_subject = UserContextSubject {
        deployment_id: deployment_a,
        host_app_id: host_a,
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
            deployment_id: deployment_a,
            host_app_id: host_a,
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: deployment_a,
            host_app_id: host_b,
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: deployment_b,
            host_app_id: host_c,
            organization_id: None,
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: deployment_a,
            host_app_id: host_a,
            organization_id: Some(organization_a),
            host_user_id: "same-user".into(),
        },
        UserContextSubject {
            deployment_id: deployment_a,
            host_app_id: host_a,
            organization_id: Some(organization_b),
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
async fn context_resolution_rejects_invalid_host_user_ids() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    let deployment = DeploymentId(Uuid::new_v4());
    let host_a = HostAppId(Uuid::new_v4());
    let identities = IdentityService::new(db.clone());

    let blank_user = UserContextSubject {
        deployment_id: deployment,
        host_app_id: host_a,
        organization_id: None,
        host_user_id: "   ".into(),
    };
    assert!(matches!(
        identities.resolve_context(&blank_user).await,
        Err(IdentityError::InvalidContext)
    ));

    let oversized_user = UserContextSubject {
        host_user_id: "x".repeat(513),
        ..blank_user.clone()
    };
    assert!(matches!(
        identities.resolve_context(&oversized_user).await,
        Err(IdentityError::InvalidContext)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn legacy_ownership_uses_persisted_context_but_cannot_be_host_asserted() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    let identities = IdentityService::new(db.clone());
    let owner = identities
        .resolve_legacy_owner(&ChannelIdentity {
            channel: "test-channel".into(),
            external_id: format!("legacy-{}", Uuid::new_v4()),
        })
        .await
        .expect("create transitional legacy owner");
    assert_ne!(owner.user_context_id.0, owner.user_id.0);
    assert_eq!(
        identities.owner_for_user(owner.user_id).await.unwrap(),
        owner
    );

    let (deployment_id, host_app_id): (Uuid, Uuid) = sqlx::query_as(
        "SELECT d.id, h.id FROM platform_deployments d
         JOIN host_apps h ON h.deployment_id = d.id
         WHERE d.external_key = 'vox.legacy.deployment'
           AND h.external_key = 'vox.legacy.channel-host'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let legacy_subject = UserContextSubject {
        deployment_id: DeploymentId(deployment_id),
        host_app_id: HostAppId(host_app_id),
        organization_id: None,
        host_user_id: owner.user_id.0.to_string(),
    };
    assert!(matches!(
        identities.resolve_context(&legacy_subject).await,
        Err(IdentityError::AccessDenied)
    ));

    let (deployment_id, host_app_id): (Uuid, Uuid) = sqlx::query_as(
        "SELECT d.id, h.id FROM platform_deployments d
         JOIN host_apps h ON h.deployment_id = d.id
         WHERE d.external_key = 'vox.standalone.deployment'
           AND h.external_key = 'vox.standalone.web'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let standalone_subject = UserContextSubject {
        deployment_id: DeploymentId(deployment_id),
        host_app_id: HostAppId(host_app_id),
        organization_id: None,
        host_user_id: "arbitrary-user".into(),
    };
    assert!(matches!(
        identities.resolve_context(&standalone_subject).await,
        Err(IdentityError::AccessDenied)
    ));
}
