use chrono::Utc;
use serde_json::json;
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest,
    },
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    skills::{PublishSkillRequest, SkillError, SkillService},
};

fn skill(key: &str, instructions: &str) -> PublishSkillRequest {
    PublishSkillRequest {
        external_key: key.into(),
        title: key.into(),
        summary: "A reusable process".into(),
        instructions: instructions.into(),
        requested_capabilities: vec!["calendar.read".into()],
        resources: json!({"checklist": "Ask for the meeting date"}),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn private_and_curated_skills_are_isolated_and_updates_require_install() {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("skills-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let make_context = |user: &str| {
        let request = HostContextRequest {
            host_user_id: user.into(),
            organization_external_key: None,
        };
        let assertion = host
            .credential
            .sign_context_request(&request, Utc::now(), Uuid::new_v4())
            .unwrap();
        (request, assertion)
    };
    let (a_request, a_assertion) = make_context("alice");
    let alice = trust
        .resolve_authenticated_context(&a_assertion, &a_request, None, Utc::now())
        .await
        .unwrap();
    let (b_request, b_assertion) = make_context("bob");
    let bob = trust
        .resolve_authenticated_context(&b_assertion, &b_request, None, Utc::now())
        .await
        .unwrap();

    let skills = SkillService::new(db.clone());
    let private = skills
        .publish_private(&alice, skill("my-notes", "Organize notes"))
        .await
        .unwrap();
    assert!(!private.curated);
    assert_eq!(private.installed_version, Some(1));
    assert!(skills.list(&bob).await.unwrap().is_empty());
    assert!(matches!(
        skills.install(&bob, private.id, 1).await,
        Err(SkillError::NotFound)
    ));

    let curated_id = skills
        .publish_curated(
            &host.deployment_external_key,
            skill("meeting-prep", "Draft an agenda"),
        )
        .await
        .unwrap();
    let available = skills.list(&bob).await.unwrap();
    assert_eq!(available.len(), 1);
    assert!(available[0].curated);
    assert_eq!(available[0].installed_version, None);
    skills.install(&bob, curated_id, 1).await.unwrap();
    skills
        .publish_curated(
            &host.deployment_external_key,
            skill("meeting-prep", "Draft an agenda and ask for missing items"),
        )
        .await
        .unwrap();
    let stale = skills.list(&bob).await.unwrap().pop().unwrap();
    assert_eq!(stale.installed_version, Some(1));
    assert_eq!(stale.latest_version, 2);
    assert!(stale.update_available);
    assert!(matches!(
        skills.install(&bob, curated_id, 1).await,
        Err(SkillError::Conflict)
    ));
    assert_eq!(
        skills
            .version(&bob, curated_id, 1)
            .await
            .unwrap()
            .instructions,
        "Draft an agenda"
    );

    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "assistant".into(),
            purpose: "Help with meetings".into(),
            requested_capability_categories: vec!["calendar.read".into()],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            agent_external_key: "assistant".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test".into(),
                model: "test".into(),
                configuration: json!({}),
            },
        })
        .await
        .unwrap();
    assert!(
        skills
            .effective(&bob, "assistant")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        skills.load_for_agent(&bob, "assistant", curated_id).await,
        Err(SkillError::NotFound)
    ));
    skills
        .set_agent_enabled(&bob, "assistant", curated_id, true)
        .await
        .unwrap();
    let effective = skills.effective(&bob, "assistant").await.unwrap();
    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].version, 1);
    assert!(effective[0].available_capabilities.is_empty());
    assert_eq!(
        skills
            .load_for_agent(&bob, "assistant", curated_id)
            .await
            .unwrap()
            .instructions,
        "Draft an agenda"
    );
    assert!(matches!(
        skills.load_for_agent(&bob, "assistant", private.id).await,
        Err(SkillError::NotFound)
    ));

    skills.install(&bob, curated_id, 2).await.unwrap();
    assert_eq!(
        skills.effective(&bob, "assistant").await.unwrap()[0].version,
        2
    );
    skills.disable(&bob, curated_id).await.unwrap();
    assert!(
        skills
            .effective(&bob, "assistant")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        skills.load_for_agent(&bob, "assistant", curated_id).await,
        Err(SkillError::NotFound)
    ));
    skills.install(&bob, curated_id, 2).await.unwrap();
    skills
        .set_agent_enabled(&bob, "assistant", curated_id, false)
        .await
        .unwrap();
    assert!(
        skills
            .effective(&bob, "assistant")
            .await
            .unwrap()
            .is_empty()
    );
}
