/**
* Integration tests for user memory and context isolation.
*/
use uuid::Uuid;
use vox_core::{
    db::Db,
    identity::{
        DeploymentId, HostAppId, HostOrganizationId, IdentityError, IdentityService,
        UserContextSubject,
    },
};

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn auth_identities_isolate_deployment_host_pairs_and_host_users() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();

    let deployment_a = DeploymentId(Uuid::new_v4());
    let deployment_b = DeploymentId(Uuid::new_v4());
    let host_a = HostAppId(Uuid::new_v4());
    let host_b = HostAppId(Uuid::new_v4());
    let host_c = HostAppId(Uuid::new_v4());
    let organization_a = HostOrganizationId(Uuid::new_v4());
    let organization_b = HostOrganizationId(Uuid::new_v4());
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
            let same_deployment_host = subjects[left].deployment_id
                == subjects[right].deployment_id
                && subjects[left].host_app_id == subjects[right].host_app_id;
            let same_host_user =
                subjects[left].host_user_id.trim() == subjects[right].host_user_id.trim();
            if same_deployment_host && same_host_user {
                assert_eq!(contexts[left].id, contexts[right].id);
                assert_eq!(contexts[left].user_id, contexts[right].user_id);
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
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();

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
