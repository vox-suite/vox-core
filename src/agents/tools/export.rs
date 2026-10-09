use super::{
    agent_memory, connections, data_query, dependencies::ToolDependencies, event_actions,
    google_maps, library, map_scene, records, spans, visits, web_search, wiz,
};
use crate::{
    db::Db,
    fresh_connections::FreshConnectionsService,
    identity::ResolvedUserContext,
    memory::MemoryService,
    realtime::{DeviceHub, UserEventHub},
};
use rig::tool::{ToolContext, ToolSet};
use serde::Serialize;
use serde_json::Value;

const EXPORT_AGENT: &str = "general";

#[derive(Clone)]
pub struct ToolExport {
    pub db: Db,
    pub memory: MemoryService,
    pub connections: FreshConnectionsService,
    pub connected_apps: Option<std::sync::Arc<crate::connected_apps::ConnectedAppsService>>,
    pub user_events: UserEventHub,
    pub device_hub: DeviceHub,
    pub google_maps_api_key: Option<String>,
}

#[derive(Serialize)]
pub struct ToolManifestEntry {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Serialize)]
pub struct ToolInvocation {
    pub ok: bool,
    pub output: Value,
}

impl ToolExport {
    pub fn toolset(&self, ctx: &ResolvedUserContext) -> ToolSet {
        let db = Some(self.db.clone());
        let owner = ctx.owner();
        let user_id = ctx.user_id;
        let http = ToolDependencies::new().map(|d| d.http).unwrap_or_default();
        let mut set = ToolSet::default();
        set.add_tool(library::AgentLibrary::new(
            db.clone(),
            self.connected_apps.clone(),
            ctx.clone(),
            EXPORT_AGENT.into(),
        ));
        set.add_tool(agent_memory::GetAgentMemory::new(
            db.clone(),
            owner.clone(),
            EXPORT_AGENT.into(),
        ));
        set.add_tool(agent_memory::UpdateAgentMemory::new(
            db.clone(),
            owner.clone(),
            EXPORT_AGENT.into(),
        ));
        set.add_tool(data_query::FindSchemas::new(db.clone(), user_id));
        set.add_tool(data_query::QueryUserData::new(db.clone(), user_id));
        set.add_tool(connections::ReadConnectedApp {
            service: Some(self.connections.clone()),
            context: ctx.clone(),
            agent: EXPORT_AGENT.into(),
        });
        set.add_tool(wiz::ControlWizLights {
            db: db.clone(),
            hub: Some(self.device_hub.clone()),
            user_id: user_id.0,
        });
        set.add_tool(map_scene::ShowOnMap::new(Some(self.user_events.clone()), user_id.0));
        set.add_tool(map_scene::ClearMap::new(Some(self.user_events.clone()), user_id.0));
        set.add_tool(visits::ListVisits::new(db.clone(), user_id));
        set.add_tool(spans::CreateSpan::new(db.clone(), owner.clone(), self.user_events.clone()));
        set.add_tool(spans::ListSpans::new(db.clone(), owner.clone()));
        set.add_tool(spans::GetSpan::new(db.clone(), owner.clone()));
        set.add_tool(spans::UpdateSpan::new(db.clone(), owner, self.user_events.clone()));
        set.add_tool(event_actions::AdjustSpan::new(self.db.clone(), user_id.0));
        set.add_tool(records::DefineDataSchema::new(db.clone(), user_id));
        set.add_tool(records::ListDataSchemas::new(db, user_id));
        set.add_tool(google_maps::SearchPlaces::new(
            http.clone(),
            self.google_maps_api_key.clone(),
        ));
        set.add_tool(google_maps::GetRoute::new(http, self.google_maps_api_key.clone()));
        if let Ok(search) = web_search::WebSearch::from_env() {
            set.add_tool(search);
        }
        set
    }

    pub fn manifest(&self, ctx: &ResolvedUserContext) -> Vec<ToolManifestEntry> {
        self.toolset(ctx)
            .get_tool_definitions()
            .into_iter()
            .map(|d| ToolManifestEntry {
                name: d.name,
                description: d.description,
                parameters: d.parameters,
            })
            .collect()
    }

    pub async fn invoke(
        &self,
        ctx: &ResolvedUserContext,
        name: &str,
        arguments: Value,
    ) -> Option<ToolInvocation> {
        let set = self.toolset(ctx);
        if !set.contains(name) {
            return None;
        }
        let result = set
            .execute(name, arguments.to_string(), &mut ToolContext::new())
            .await;
        let output = result.output();
        let value = output
            .as_json()
            .cloned()
            .unwrap_or_else(|| Value::String(output.render()));
        Some(ToolInvocation {
            ok: result.is_success(),
            output: value,
        })
    }
}
