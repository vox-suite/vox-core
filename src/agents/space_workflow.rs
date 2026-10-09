use super::*;
use crate::storage::space_tasks::{ClaimedTask, TaskProposal, TaskRepository};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::{Value, json};
use sqlx::Row;

impl SpaceRuntime {
    pub(super) async fn run_workflow(
        &self,
        space: crate::domain::spaces::Space,
        message: Option<String>,
    ) -> Result<(), AgentError> {
        let repo = SpaceRepository::new(self.db.pool().clone());
        let tasks = TaskRepository::new(self.db.pool().clone());
        let graph = repo
            .get_graph(space.user_id, space.id)
            .await
            .map_err(|_| AgentError::Provider)?
            .ok_or(AgentError::Provider)?;
        let root = graph
            .nodes
            .iter()
            .find(|n| n.kind == "goal")
            .ok_or(AgentError::Provider)?
            .id;
        repo.set_run_state(space.id, RunState::Running, None)
            .await
            .map_err(|_| AgentError::Provider)?;
        // Replaying all initial proposals is safe: database dedupe repairs partial initialization.
        let generation: i32 =
            sqlx::query_scalar("SELECT workflow_generation FROM spaces WHERE id=$1")
                .bind(space.id)
                .fetch_one(self.db.pool())
                .await
                .map_err(|_| AgentError::Provider)?;
        if generation == 1 {
            let web = tasks
                .spawn_generation(
                    space.id,
                    &TaskProposal {
                        role: "web_search".into(),
                        title: "Web research".into(),
                        brief: space.intent.clone(),
                        dependencies: vec![root],
                    },
                    crate::config::DEFAULT_SPACE_MAX_STEPS as i64,
                    crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,
                    generation,
                )
                .await
                .map_err(|_| AgentError::Provider)?;
            let data = tasks
                .spawn_generation(
                    space.id,
                    &TaskProposal {
                        role: "user_data".into(),
                        title: "Your context".into(),
                        brief: space.intent.clone(),
                        dependencies: vec![root],
                    },
                    crate::config::DEFAULT_SPACE_MAX_STEPS as i64,
                    crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,
                    generation,
                )
                .await
                .map_err(|_| AgentError::Provider)?;
            if let (Some(web), Some(data)) = (web, data) {
                tasks.spawn_generation(space.id,&TaskProposal{role:"synthesis".into(),title:"Bring findings together".into(),brief:format!("Synthesize the vision and prerequisites. Request further research only when needed: {}",space.intent),dependencies:vec![web,data]},crate::config::DEFAULT_SPACE_MAX_STEPS as i64,crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,generation).await.map_err(|_|AgentError::Provider)?;
            }
        }
        let _ = message; // Version 2 requests are persisted by the HTTP route before acknowledgment.
        let mut running = FuturesUnordered::new();
        loop {
            self.consume_workflow_requests(&space, root, &tasks).await?;
            self.expand_workflow_outputs(space.id, &tasks).await?;
            while running.len() < 3 {
                let Ok(permit) = self.workflow_slots.clone().try_acquire_owned() else {
                    break;
                };
                let Some(task) = tasks
                    .claim(space.id)
                    .await
                    .map_err(|_| AgentError::Provider)?
                else {
                    break;
                };
                self.workflow_notify(space.user_id, space.id);
                running.push(self.execute_workflow_task(space.user_id, task, permit));
            }
            if let Some(result) = running.next().await {
                result?;
                continue;
            }
            let pending:i64=sqlx::query_scalar("SELECT count(*) FROM space_tasks WHERE space_id=$1 AND status IN ('queued','running')").bind(space.id).fetch_one(self.db.pool()).await.map_err(|_| AgentError::Provider)?;
            if pending == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let failed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM space_tasks WHERE space_id=$1 AND status IN ('failed','blocked')",
        )
        .bind(space.id)
        .fetch_one(self.db.pool())
        .await
        .map_err(|_| AgentError::Provider)?;
        repo.set_run_state(
            space.id,
            if failed > 0 {
                RunState::Failed
            } else {
                RunState::Idle
            },
            if failed > 0 {
                Some("A workflow task needs attention")
            } else {
                None
            },
        )
        .await
        .map_err(|_| AgentError::Provider)?;
        sqlx::query("UPDATE spaces SET state='planned' WHERE id=$1 AND state='ideating' AND NOT EXISTS(SELECT 1 FROM space_tasks WHERE space_id=$1 AND status!='done') AND EXISTS(SELECT 1 FROM space_tasks WHERE space_id=$1 AND role='plan' AND status='done')").bind(space.id).execute(self.db.pool()).await.map_err(|_|AgentError::Provider)?;
        self.workflow_notify(space.user_id, space.id);
        Ok(())
    }
    fn workflow_notify(&self, user: Uuid, space: Uuid) {
        if let Some(hub) = &self.user_events {
            hub.notify(user, json!({"type":"space_graph_updated","space_id":space}));
        }
    }
    async fn stream_task_text(
        &self,
        agent: rig::agent::Agent,
        prompt: &str,
        user: Uuid,
        space: Uuid,
        node: Uuid,
    ) -> Result<String, AgentError> {
        use rig::agent::MultiTurnStreamItem;
        use rig::streaming::{StreamedAssistantContent, StreamingPrompt};
        let mut stream = agent.stream_prompt(prompt).await;
        let mut raw = String::new();
        let mut sent = 0;
        let mut last = std::time::Instant::now();
        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                    chunk,
                ))) => {
                    raw.push_str(&chunk.text);
                    if last.elapsed() >= std::time::Duration::from_millis(150) {
                        last = std::time::Instant::now();
                        if let Some(text) = partial_summary(&raw)
                            && text.len() > sent
                            && let Some(hub) = &self.user_events
                        {
                            sent = text.len();
                            hub.notify(
                                user,
                                json!({"type":"space_node_stream","space_id":space,"node_id":node,"text":text}),
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => return Err(AgentError::Provider),
            }
        }
        Ok(raw)
    }
    async fn execute_workflow_task(
        &self,
        user: Uuid,
        task: ClaimedTask,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), AgentError> {
        let tasks = TaskRepository::new(self.db.pool().clone());
        let inputs=sqlx::query("SELECT n.id,n.title,n.body,n.provenance FROM space_edges e JOIN space_nodes n ON n.id=e.from_node WHERE e.to_node=$1").bind(task.node_id).fetch_all(self.db.pool()).await.map_err(|_|AgentError::Provider)?;
        let inputs:Vec<Value>=inputs.iter().map(|r|json!({"id":r.get::<Uuid,_>("id"),"title":r.get::<String,_>("title"),"summary":r.get::<String,_>("body"),"evidence":r.get::<Value,_>("provenance")})).collect();
        let prompt = format!("Task: {}\nPrerequisites: {}", task.brief, json!(inputs));
        let preamble = "Followups can use local aliases: give a proposal an id such as search2; later proposals may reference search2 in dependencies alongside actual prerequisite UUIDs. For a second web search, depend on the first web node; for a join, depend on search2 and user_data. Complete only this assigned task. Return JSON {\"summary\":\"compact useful findings\",\"evidence\":[],\"followups\":[]}. Evidence must reference actual tool results; preserve unknowns. Followups may contain role (web_search,user_data,synthesis,plan), title, brief, dependencies (UUIDs). Use your own node ID as dependency for sequential work and other prerequisite IDs for joins. Never claim external actions were performed. Synthesis should request a plan or necessary follow-up research; plan must not request more work. No graph mutation tools.";
        let client = gemini::Client::new(&self.gemini_api_key).map_err(|_| AgentError::Provider)?;
        let owner = ResourceOwner {
            user_context_id: crate::identity::UserContextId(user),
            user_id: UserId(user),
        };
        let work = async {
            let base = client
                .agent(&self.gemini_model)
                .preamble(&format!("{preamble}\nYour node ID: {}", task.node_id))
                .default_max_turns(8);
            let agent = if task.role == "web_search" {
                base.tool(WebSearch::new(self.http.clone(), self.exa_api_key.clone()))
                    .build()
            } else if task.role == "user_data" {
                base.tool(FindSchemas::new(Some(self.db.clone()), UserId(user)))
                    .tool(QueryUserData::new(Some(self.db.clone()), UserId(user)))
                    .tool(ListSpans::new(Some(self.db.clone()), owner))
                    .tool(GetSpan::new(Some(self.db.clone()), owner))
                    .build()
            } else {
                base.build()
            };
            let text = self
                .stream_task_text(agent, &prompt, user, task.space_id, task.node_id)
                .await?;
            serde_json::from_str::<Value>(
                text.trim()
                    .trim_start_matches("```json")
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim(),
            )
            .map_err(|_| AgentError::Provider)
        };
        tokio::pin!(work);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(180));
        tokio::pin!(deadline);
        let output = loop {
            tokio::select! {result=&mut work=>break result,_=&mut deadline=>break Err(AgentError::Provider),_=heartbeat.tick()=>{if !tasks.heartbeat(&task).await.map_err(|_|AgentError::Provider)? {return Ok(());}}}
        };
        let output = output.map(|mut value| {
            if let Some(summary) = value["summary"].as_str() {
                value["summary"] = json!(summary.replace("\\n", "\n"));
            }
            value
        });
        let output = output.and_then(|value| {
            if value["summary"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            {
                Ok(value)
            } else {
                Err(AgentError::Provider)
            }
        });
        let success = output.is_ok();
        let output = output.unwrap_or(json!({"error":"Task failed or timed out"}));
        tasks
            .finish(&task, &output, success)
            .await
            .map_err(|_| AgentError::Provider)?;
        self.workflow_notify(user, task.space_id);
        Ok(())
    }
}

impl SpaceRuntime {
    async fn consume_workflow_requests(
        &self,
        space: &crate::domain::spaces::Space,
        root: Uuid,
        tasks: &TaskRepository,
    ) -> Result<(), AgentError> {
        let requests=sqlx::query("SELECT id,message,node_id,generation FROM space_workflow_requests WHERE space_id=$1 AND NOT processed ORDER BY created_at,id").bind(space.id).fetch_all(self.db.pool()).await.map_err(|_|AgentError::Provider)?;
        for row in requests {
            let id: Uuid = row.get("id");
            let generation: i32 = row.get("generation");
            let target: Option<Uuid> = row.get("node_id");
            let text: String = row.get("message");
            let role: String = if let Some(node) = target {
                sqlx::query_scalar("SELECT coalesce(data->'execution'->>'role','synthesis') FROM space_nodes WHERE id=$1 AND space_id=$2").bind(node).bind(space.id).fetch_one(self.db.pool()).await.map_err(|_|AgentError::Provider)?
            } else {
                "synthesis".into()
            };
            let context = if target.is_none() {
                SpaceRepository::new(self.db.pool().clone())
                    .get_graph(space.user_id, space.id)
                    .await
                    .map_err(|_| AgentError::Provider)?
                    .map(|g| serde_json::to_string(&g.nodes).unwrap_or_default())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let brief = format!("User follow-up {id}: {text}\nExisting workflow: {context}");
            tasks
                .spawn_generation(
                    space.id,
                    &TaskProposal {
                        role,
                        title: "Your follow-up".into(),
                        brief,
                        dependencies: vec![target.unwrap_or(root)],
                    },
                    crate::config::DEFAULT_SPACE_MAX_STEPS as i64,
                    crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,
                    generation,
                )
                .await
                .map_err(|_| AgentError::Provider)?;
            sqlx::query("UPDATE space_workflow_requests SET processed=true WHERE id=$1")
                .bind(id)
                .execute(self.db.pool())
                .await
                .map_err(|_| AgentError::Provider)?;
        }
        Ok(())
    }
    async fn expand_workflow_outputs(
        &self,
        space: Uuid,
        tasks: &TaskRepository,
    ) -> Result<(), AgentError> {
        let rows=sqlx::query("SELECT t.node_id,t.output,t.role,t.generation FROM space_tasks t JOIN spaces s ON s.id=t.space_id WHERE t.space_id=$1 AND t.status='done' AND NOT t.expanded AND t.generation=s.workflow_generation").bind(space).fetch_all(self.db.pool()).await.map_err(|_|AgentError::Provider)?;
        for row in rows {
            let parent: Uuid = row.get("node_id");
            let generation: i32 = row.get("generation");
            let output: Value = row.get("output");
            let role: String = row.get("role");
            let mut aliases = std::collections::HashMap::<String, Uuid>::new();
            if role != "plan" {
                if let Some(proposals) = output["followups"].as_array() {
                    for proposal in proposals.iter().take(4) {
                        let Some(role) = proposal["role"]
                            .as_str()
                            .filter(|r| crate::storage::space_tasks::valid_role(r))
                        else {
                            continue;
                        };
                        let mut dependencies: Vec<Uuid> = proposal["dependencies"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v.as_str())
                            .filter_map(|id| {
                                Uuid::parse_str(id)
                                    .ok()
                                    .or_else(|| aliases.get(id).copied())
                            })
                            .collect();
                        let requested = proposal["dependencies"].as_array().map_or(0, Vec::len);
                        if dependencies.len() != requested {
                            sqlx::query("UPDATE space_tasks SET status='failed',error='Follow-up contains an unresolved dependency' WHERE node_id=$1").bind(parent).execute(self.db.pool()).await.map_err(|_|AgentError::Provider)?;
                            continue;
                        }
                        if !dependencies.contains(&parent) {
                            dependencies.push(parent);
                        }
                        let p = TaskProposal {
                            role: role.into(),
                            title: proposal["title"].as_str().unwrap_or("Follow-up").into(),
                            brief: proposal["brief"].as_str().unwrap_or("").into(),
                            dependencies,
                        };
                        match tasks
                            .spawn_followup(
                                space,
                                &p,
                                crate::config::DEFAULT_SPACE_MAX_STEPS as i64,
                                crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,
                                generation,
                                parent,
                            )
                            .await
                        {
                            Ok(Some(id)) => {
                                if let Some(alias) = proposal["id"].as_str() {
                                    aliases.insert(alias.into(), id);
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                sqlx::query("UPDATE space_tasks SET status='failed',error=$2 WHERE node_id=$1")
                                    .bind(parent)
                                    .bind(error.to_string())
                                    .execute(self.db.pool())
                                    .await
                                    .map_err(|_| AgentError::Provider)?;
                            }
                        }
                    }
                }
            }
            if role == "synthesis" && output["followups"].as_array().is_none_or(|p| p.is_empty()) {
                tasks.spawn_followup(space,&TaskProposal{role:"plan".into(),title:"Proposed plan".into(),brief:"Turn these findings into a concrete plan. Preserve uncertainties and cite prerequisite evidence. Do not perform external actions.".into(),dependencies:vec![parent]},crate::config::DEFAULT_SPACE_MAX_STEPS as i64,crate::config::DEFAULT_SPACE_MAX_CHILDREN as i64,generation,parent).await.map_err(|_|AgentError::Provider)?;
            }
            let mut tx = self
                .db
                .pool()
                .begin()
                .await
                .map_err(|_| AgentError::Provider)?;
            let updated = sqlx::query(
                "UPDATE space_tasks t SET expanded=true FROM spaces s WHERE t.node_id=$1 AND NOT t.expanded AND t.status IN ('done','failed') AND s.id=t.space_id AND s.workflow_generation=t.generation",
            )
            .bind(parent)
            .execute(&mut *tx)
            .await
            .map_err(|_| AgentError::Provider)?;
            if updated.rows_affected() > 0 {
                sqlx::query(
                    "INSERT INTO space_messages(space_id,role,text) VALUES($1,'assistant',$2)",
                )
                .bind(space)
                .bind(output["summary"].as_str().unwrap_or(""))
                .execute(&mut *tx)
                .await
                .map_err(|_| AgentError::Provider)?;
            }
            tx.commit().await.map_err(|_| AgentError::Provider)?;
            tasks
                .sync_metadata(space)
                .await
                .map_err(|_| AgentError::Provider)?;
            if let Some(hub) = &self.user_events {
                let user: Uuid = sqlx::query_scalar("SELECT user_id FROM spaces WHERE id=$1")
                    .bind(space)
                    .fetch_one(self.db.pool())
                    .await
                    .map_err(|_| AgentError::Provider)?;
                hub.notify(
                    user,
                    json!({"type":"space_message_created","space_id":space}),
                );
            }
        }
        Ok(())
    }
}

fn partial_summary(raw: &str) -> Option<String> {
    let rest = &raw[raw.find("\"summary\"")? + 9..];
    let rest = rest[rest.find(':')? + 1..].trim_start();
    let mut chars = rest.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) if hex.len() == 4 => out.push(ch),
                        _ => break,
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    Some(out)
}
