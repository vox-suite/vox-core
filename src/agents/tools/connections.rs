use crate::fresh_connections::{FreshConnectionError, FreshConnectionsService};
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
#[derive(Clone)]
pub struct ReadConnectedApp {
    pub service: Option<FreshConnectionsService>,
    pub user_id: Uuid,
}
#[derive(Deserialize)]
pub struct Args {
    pub connector_id: String,
    pub limit: Option<usize>,
}
impl Tool for ReadConnectedApp {
    const NAME: &'static str = "read_connected_app";
    type Args = Args;
    type Output = Value;
    type Error = FreshConnectionError;
    fn description(&self) -> String {
        "Read the user's connected Google Calendar, PlayStation, food delivery, Spotify or YouTube account. Consent is enforced on every invocation. Respect completeness and observed_at. Gaming playDuration is a lifetime counter, not a session duration; exact gaming session times are unknown. Spotify listening timestamps do not prove playback duration. YouTube watch history requires a consented Takeout import; maps_timeline holds places the user visited from a consented Google Maps Timeline import; subscriptions and likes are not watch history.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"connector_id":{"type":"string","enum":["google_calendar","playstation","swiggy","zomato","spotify","youtube","youtube_history","maps_timeline"]},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["connector_id"]})
    }
    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: Args,
    ) -> Result<Value, FreshConnectionError> {
        self.service
            .as_ref()
            .ok_or(FreshConnectionError::Unauthorized)?
            .assistant_read(self.user_id, &args.connector_id, args.limit.unwrap_or(25))
            .await
    }
}
