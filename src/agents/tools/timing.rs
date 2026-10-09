use rig::{prelude::ToolContext, tool::Tool};
use serde_json::Value;
use std::time::Instant;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnCorrelation {
    pub conversation_id: Option<String>,
    pub turn_id: Option<String>,
    pub revision: Option<u64>,
}
pub struct TimedTool<T> {
    inner: T,
    correlation: TurnCorrelation,
}
impl<T> TimedTool<T> {
    pub fn new(inner: T, correlation: TurnCorrelation) -> Self {
        Self { inner, correlation }
    }
}
impl<T: Tool> Tool for TimedTool<T> {
    const NAME: &'static str = T::NAME;
    type Args = T::Args;
    type Output = T::Output;
    type Error = T::Error;
    fn description(&self) -> String {
        self.inner.description()
    }
    fn parameters(&self) -> Value {
        self.inner.parameters()
    }
    async fn call(
        &self,
        context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let execution_id = uuid::Uuid::new_v4().to_string();
        let mut report = ToolReport {
            execution_id,
            started: Instant::now(),
            name: T::NAME,
            correlation: &self.correlation,
            outcome: "cancelled",
        };
        tracing::info!(execution_id = %report.execution_id, tool = T::NAME, conversation_id = self.correlation.conversation_id.as_deref(), turn_id = self.correlation.turn_id.as_deref(), revision = self.correlation.revision, "CORE_TOOL_STARTED");
        let result = self.inner.call(context, args).await;
        report.outcome = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        result
    }
}
struct ToolReport<'a> {
    execution_id: String,
    started: Instant,
    name: &'static str,
    correlation: &'a TurnCorrelation,
    outcome: &'static str,
}
impl Drop for ToolReport<'_> {
    fn drop(&mut self) {
        tracing::info!(execution_id = %self.execution_id, tool = self.name, conversation_id = self.correlation.conversation_id.as_deref(), turn_id = self.correlation.turn_id.as_deref(), revision = self.correlation.revision, outcome = self.outcome, elapsed_ms = self.started.elapsed().as_millis() as u64, "CORE_TOOL_FINISHED");
    }
}
