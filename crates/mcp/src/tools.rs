//! MCP's view of the shared canvas tool catalog.
//!
//! The tool definitions, schemas, argument validation and dispatch all live in
//! [`figcraft_agent_tools`], so the in-app Agent gets exactly the same behaviour. This module is the
//! thin adapter that pins MCP's surface: every tool, in the order `tools/list` has always used, with
//! no filtering.

pub use figcraft_agent_tools::catalog::ToolResult;
pub use figcraft_agent_tools::{TOOL_NAMES, ToolPolicy, call_tool as call_catalog_tool, tool_definitions as catalog_definitions};

/// Every tool, in `tools/list` order: external MCP exposes the complete set.
pub fn tool_definitions() -> Vec<serde_json::Value> {
    catalog_definitions(ToolPolicy::ExternalMcp)
}

/// Run one tool as an external MCP client would.
pub fn call_tool(b: &mut dyn figcraft_agent_tools::Backend, name: &str, args: &serde_json::Value) -> ToolResult {
    call_catalog_tool(b, ToolPolicy::ExternalMcp, name, args)
}
