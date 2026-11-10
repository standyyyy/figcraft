//! The shared tool catalog: definitions, policy, argument validation and dispatch.

/// Which tools a surface exposes.
///
/// `ExternalMcp` keeps every tool the MCP server has always offered. `EmbeddedAgent` is limited to
/// document-local, replayable tools, because a turn commits as one `command.batch` and anything
/// outside the document (file I/O, sharing, plugins, live sessions, undo/redo, pointer or key
/// injection) could not be replayed safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Everything, for external MCP clients.
    ExternalMcp,
    /// Document-local, replayable tools only, for the in-app Agent.
    EmbeddedAgent,
}
