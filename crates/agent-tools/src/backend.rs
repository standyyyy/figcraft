//! Backends a tool call can run against.
//!
//! A [`Backend`] is the seam between the shared catalog and whatever executes commands: the MCP
//! server's headless session or attached app, and the embedded Agent's shadow session. Keeping it
//! a trait here lets both surfaces call the same definitions without knowing about each other.

use serde_json::Value;

/// Executes one tool method and returns its JSON result.
pub trait Backend {
    /// Run `method` with `params`, returning the tool result or a message the model can read.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;

    /// Whether a user is watching, which decides whether a tool offers UI-only behaviour.
    fn has_ui(&self) -> bool;

    /// A short description of where commands run, for tool descriptions and diagnostics.
    fn describe(&self) -> String;
}
