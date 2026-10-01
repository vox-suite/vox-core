use crate::{
    agents::{
        AgentError,
        tools::{
            data_query::{FindSchemas, QueryUserData},
            google_maps::{GetRoute, SearchPlaces},
            space_graph::{AddEdge, AddNode, RemoveNode, SpawnBranch, UpdateNode},
            spans::{GetSpan, ListSpans},
            web_search::WebSearch,
        },
    },
    config::Config,
    db::Db,
    domain::spaces::{AgentSpec, RunState, SpaceGraph, SpaceState},
    identity::{ResourceOwner, UserId},
    realtime::UserEventHub,
    storage::spaces::SpaceRepository,
};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use uuid::Uuid;

pub struct SpaceRuntime {
    db: Db,
    gemini_api_key: String,
    gemini_model: String,
    exa_api_key: String,
    google_maps_api_key: Option<String>,
    http: reqwest::Client,
    user_events: Option<UserEventHub>,
    locks:
        std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl SpaceRuntime {
    pub fn new(db: Db, config: &Config, user_events: Option<UserEventHub>) -> Self {
        Self {
            db,
            gemini_api_key: config.gemini_api_key.clone(),
            gemini_model: config.gemini_model.clone(),
            exa_api_key: config.exa_api_key.clone(),
            google_maps_api_key: config.google_maps_api_key.clone(),
            http: reqwest::Client::new(),
            user_events,
            locks: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub async fn run_space(
        &self,
        space_id: Uuid,
        user_message: Option<String>,
    ) -> Result<(), AgentError> {
        let lock = self
            .locks
            .lock()
            .map_err(|_| AgentError::Provider)?
            .entry(space_id)
            .or_default()
            .clone();
        let _guard = lock.lock().await;

        let repo = SpaceRepository::new(self.db.pool().clone());
        let Some(space) = repo
            .get_space_by_id_only(space_id)
            .await
            .map_err(|_| AgentError::Provider)?
        else {
            return Ok(());
        };

        let _ = repo.set_run_state(space_id, RunState::Running, None).await;
        if let Some(hub) = &self.user_events {
            hub.notify(
                space.user_id,
                serde_json::json!({
                    "type": "space_run_started",
                    "space_id": space_id
                }),
            );
        }

        let user_msg_text = match user_message {
            Some(ref msg) => msg.clone(),
            None => space.intent.clone(),
        };
        let user_msg = repo.add_message(space_id, "user", &user_msg_text).await;
        if let (Ok(msg), Some(hub)) = (user_msg, &self.user_events) {
            hub.notify(
                space.user_id,
                serde_json::json!({
                    "type": "space_message_created",
                    "space_id": space_id,
                    "message": msg
                }),
            );
        }

        let graph: Option<SpaceGraph> = repo
            .get_graph(space.user_id, space_id)
            .await
            .map_err(|_| AgentError::Provider)?;
        let existing_nodes = graph.as_ref().map(|g| &g.nodes);
        let existing_edges = graph.as_ref().map(|g| &g.edges);

        let spec: AgentSpec = serde_json::from_value(space.agent_spec.clone()).unwrap_or_default();
        let max_steps = if spec.limits.max_steps > 0 {
            spec.limits.max_steps
        } else {
            crate::config::DEFAULT_SPACE_MAX_STEPS
        };
        let max_children = if spec.limits.max_children > 0 {
            spec.limits.max_children
        } else {
            crate::config::DEFAULT_SPACE_MAX_CHILDREN
        };

        let user_id = UserId(space.user_id);
        let owner = ResourceOwner {
            user_context_id: crate::identity::UserContextId(space.user_id),
            user_id,
        };

        let preamble = format!(
            "You are the autonomous Space Agent for Vox. Your role is ideation: understanding the user's vision, \
             querying their past data to understand constraints, conducting external research, offering concrete \
             options with pros/cons, and synthesizing a realistic plan before any real-world commitment.\n\n\
             Space Title: {}\n\
             Original Vision/Intent: {}\n\
             Mission: {}\n\
             What to look for: {:?}\n\
             Done when: {}\n\n\
             Instructions:\n\
             1. If the graph is empty, start by creating a root 'goal' node with add_node.\n\
             2. Use find_schemas and query_user_data to inspect the user's expenditure, budgets, or past spans. Add 'data' nodes linked from the goal.\n\
             3. Formulate a synthesis node ('What I know about you') summarizing available budget and constraints.\n\
             4. Use web_search or search_places to research viable options/destinations/alternatives. Add 'option' nodes with pros, cons, costs, and travel times.\n\
             5. Connect nodes using add_edge to form a clean, flowing DAG directed toward a conclusion.\n\
             6. When options are evaluated, synthesize a 'plan' or 'decision' node with concrete steps.\n\
             7. If the user sends a steer message, adapt the graph accordingly, updating or removing affected nodes.",
            space.title, space.intent, spec.mission, spec.look_for, spec.done_when
        );

        let Ok(client) = gemini::Client::new(&self.gemini_api_key) else {
            let err_text = "Failed to initialize Gemini client".to_string();
            let _ = repo
                .set_run_state(space_id, RunState::Failed, Some(&err_text))
                .await;
            let _ = repo.add_message(space_id, "system", &err_text).await;
            if let Some(hub) = &self.user_events {
                hub.notify(
                    space.user_id,
                    serde_json::json!({
                        "type": "space_run_failed",
                        "space_id": space_id,
                        "error": err_text
                    }),
                );
            }
            return Err(AgentError::Provider);
        };

        let existing_graph_summary = format!(
            "Existing Nodes ({}):\n{}\nExisting Edges ({}):\n{}",
            existing_nodes.map_or(0, |n| n.len()),
            serde_json::to_string_pretty(existing_nodes.unwrap_or(&vec![])).unwrap_or_default(),
            existing_edges.map_or(0, |e| e.len()),
            serde_json::to_string_pretty(existing_edges.unwrap_or(&vec![])).unwrap_or_default()
        );

        let user_turn = match user_message {
            Some(ref msg) => format!("User instruction / feedback: {}\nUpdate and expand the space graph.", msg),
            None => "Begin autonomous research and graph generation for this space. Query necessary user data first, then explore options and draft the plan.".to_string(),
        };

        let prompt_input = format!(
            "Current Space Graph State:\n{}\n\nTask:\n{}",
            existing_graph_summary, user_turn
        );

        let agent = client
            .agent(&self.gemini_model)
            .name("space-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&preamble)
            .tool(AddNode::new(
                Some(self.db.clone()),
                user_id,
                space_id,
                self.user_events.clone(),
            ))
            .tool(UpdateNode::new(
                Some(self.db.clone()),
                user_id,
                space_id,
                self.user_events.clone(),
            ))
            .tool(AddEdge::new(
                Some(self.db.clone()),
                user_id,
                space_id,
                self.user_events.clone(),
            ))
            .tool(RemoveNode::new(
                Some(self.db.clone()),
                user_id,
                space_id,
                self.user_events.clone(),
            ))
            .tool(SpawnBranch::new(
                Some(self.db.clone()),
                user_id,
                space_id,
                max_children,
                self.user_events.clone(),
            ))
            .tool(FindSchemas::new(Some(self.db.clone()), user_id))
            .tool(QueryUserData::new(Some(self.db.clone()), user_id))
            .tool(ListSpans::new(Some(self.db.clone()), owner))
            .tool(GetSpan::new(Some(self.db.clone()), owner))
            .tool(WebSearch::new(self.http.clone(), self.exa_api_key.clone()))
            .tool(SearchPlaces::new(
                self.http.clone(),
                self.google_maps_api_key.clone(),
            ))
            .tool(GetRoute::new(
                self.http.clone(),
                self.google_maps_api_key.clone(),
            ))
            .default_max_turns(max_steps)
            .build();

        let mut failed = false;
        match agent.prompt(prompt_input).await {
            Ok(reply) => {
                let asst_msg = repo.add_message(space_id, "assistant", &reply).await;
                if let (Ok(msg), Some(hub)) = (asst_msg, &self.user_events) {
                    hub.notify(
                        space.user_id,
                        serde_json::json!({
                            "type": "space_message_created",
                            "space_id": space_id,
                            "message": msg
                        }),
                    );
                }
            }
            Err(err) => {
                let err_text = err.to_string();
                let _ = repo
                    .set_run_state(space_id, RunState::Failed, Some(&err_text))
                    .await;
                let _ = repo.add_message(space_id, "system", &err_text).await;
                if let Some(hub) = &self.user_events {
                    hub.notify(
                        space.user_id,
                        serde_json::json!({
                            "type": "space_run_failed",
                            "space_id": space_id,
                            "error": err_text
                        }),
                    );
                }
                failed = true;
            }
        }

        let _ = repo.finish_running_nodes(space_id).await;

        let final_nodes = repo.list_nodes(space_id).await.unwrap_or_default();
        let has_decision = final_nodes
            .iter()
            .any(|n| n.kind == "decision" || n.kind == "plan");
        if has_decision && space.state == SpaceState::Ideating {
            let _ = repo
                .update_space_state(space.user_id, space_id, SpaceState::Planned)
                .await;
        }

        if let Some(hub) = &self.user_events {
            hub.notify(
                space.user_id,
                serde_json::json!({
                    "type": "space_updated",
                    "space_id": space_id
                }),
            );
        }

        if !failed {
            let _ = repo.set_run_state(space_id, RunState::Idle, None).await;
            if let Some(hub) = &self.user_events {
                hub.notify(
                    space.user_id,
                    serde_json::json!({
                        "type": "space_run_finished",
                        "space_id": space_id
                    }),
                );
            }
        }

        Ok(())
    }
}
