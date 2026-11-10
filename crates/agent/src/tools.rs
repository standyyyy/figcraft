//! Rig tool adapters over the shared catalog.
//!
//! One [`DynamicTool`] per catalog entry the embedded policy offers. Each forwards its arguments
//! unchanged to [`figcraft_agent_tools::call_tool`], so a tool behaves the same here as it does for
//! an external MCP client, and a tool failure reaches the model as a tool result rather than ending
//! the turn.

use std::sync::{Arc, Mutex};

use figcraft_agent_tools::{ToolPolicy, ToolResult};
use rig_agent::tool::{DynamicTool, ToolOutput};
use rig_core::message::ToolName;
use serde_json::Value;

use crate::shadow::Shadow;

/// The tools an Agent turn may call, sharing one shadow document.
///
/// The shadow is behind a mutex because Rig's tool callbacks are `Fn` and may run concurrently in
/// principle; a turn runs its tools one after another, so the lock is never contended in practice.
#[derive(Clone)]
pub struct ShadowTools {
    shadow: Arc<Mutex<Shadow>>,
    policy: ToolPolicy,
}

impl std::fmt::Debug for ShadowTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShadowTools").field("policy", &self.policy).finish()
    }
}

impl ShadowTools {
    /// Wrap `shadow` so tools run against it under `policy`.
    pub fn new(shadow: Shadow, policy: ToolPolicy) -> Self {
        Self { shadow: Arc::new(Mutex::new(shadow)), policy }
    }

    /// Run one catalog tool against the shadow, with the lock held for that call only.
    pub fn call(&self, name: &str, args: &Value) -> ToolResult {
        match self.shadow.lock() {
            Ok(mut sh) => sh.call(self.policy, name, args),
            // A poisoned lock means a tool panicked; the engine's guards turn that into an error the
            // model can read rather than taking the app down.
            Err(_) => ToolResult::error("the Agent's working document is unavailable after an earlier failure"),
        }
    }

    /// The replay steps the turn recorded so far.
    pub fn replay_len(&self) -> usize {
        self.shadow.lock().map(|sh| sh.replay().len()).unwrap_or(0)
    }

    /// The shadow, once the turn is over and no tool can run.
    pub fn into_shadow(self) -> Option<Shadow> {
        // A poisoned lock still holds the shadow: the turn is over, so recovering it is safe.
        Arc::try_unwrap(self.shadow).ok().map(|m| m.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Every tool Rig should advertise, in catalog order.
    pub fn definitions(&self) -> Vec<DynamicTool> {
        figcraft_agent_tools::tool_definitions(self.policy).into_iter().filter_map(|def| self.adapter(def)).collect()
    }

    /// One catalog definition as a Rig tool.
    fn adapter(&self, def: Value) -> Option<DynamicTool> {
        let name = def.get("name").and_then(Value::as_str)?.to_string();
        let description = def.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        let schema = def.get("inputSchema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        let tool_name = ToolName::new(&name).ok()?;
        let tools = self.clone();
        Some(DynamicTool::new(tool_name, &description, schema, move |args: Value| {
            // The catalog owns validation and dispatch; Rig only carries the call.
            let result = tools.call(&name, &args);
            let text = tool_text(&result);
            Box::pin(async move { Ok(ToolOutput::text(text)) })
        }))
    }

    /// The catalog tools as a Rig [`ToolSet`](rig_agent::tool::ToolSet), in catalog order.
    pub fn tool_set(&self) -> rig_agent::tool::ToolSet {
        let mut set = rig_agent::tool::ToolSet::default();
        for tool in self.definitions() {
            set.add_dynamic_tool(tool);
        }
        set
    }
}

/// What a model sees from a tool result.
///
/// A failed tool keeps its message but is marked, so the model can tell a refusal from content
/// without the turn ending: the engine's rule is that a tool failure is information, not a crash.
fn tool_text(result: &ToolResult) -> String {
    let mut out = String::new();
    if result.is_error {
        out.push_str("Error: ");
    }
    let mut first = true;
    for block in &result.content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if !first {
                out.push('\n');
            }
            out.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
            first = false;
        }
    }
    if out.is_empty() {
        // An image-only result (a screenshot) still needs something a model can read.
        out = if result.is_error { "Error: the tool failed".to_string() } else { "(the tool returned no text)".to_string() };
    }
    out
}

/// [`tool_text`] for the tests in this crate: what a model would read from a result.
#[cfg(test)]
pub(crate) fn tool_text_for_test(result: &ToolResult) -> String {
    tool_text(result)
}
