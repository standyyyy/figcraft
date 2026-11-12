//! FigCraft's MCP server.
//!
//! [Model Context Protocol](https://modelcontextprotocol.io) over stdio: newline-delimited JSON-RPC
//! 2.0, hand-written (no async runtime). The server exposes FigCraft as MCP tools, resources and
//! prompts, and forwards everything to a [`figcraft_agent_tools::Backend`]:
//!
//! - [`Headless`] hosts an in-process [`figcraft_engine::Session`] and answers the control-channel
//!   methods itself (rendering with `figcraft-render`), so agents can design without a window.
//! - [`Attached`] talks to a running desktop app through its loopback JSON-lines control channel
//!   (`figcraft --control PORT`): one `{"id","method","params"}` line in, one
//!   `{"id","ok","result"|"error"}` line out.
//!
//! The canvas tools themselves live in [`figcraft_agent_tools`], which the in-app Agent consumes
//! too; this crate adds the MCP protocol, resources, prompts and the two transports.
//!
//! Entry points: [`Server::serve`] (stdio loop) and [`Server::handle_line`] (one message).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod attached;
mod headless;
mod prompts;
mod resources;
mod server;
mod tools;

pub use attached::Attached;
/// Re-exported from the shared catalog so existing callers keep one path.
pub use figcraft_agent_tools::{Backend, ToolPolicy, ToolResult, decode_data, page_children};
pub use headless::Headless;
pub use prompts::{PROMPTS, PromptArg, PromptDef};
pub use resources::{COMMAND_TEMPLATE, COMMANDS_URI, DOC_URI, NODE_TEMPLATE};
pub use server::{INSTRUCTIONS, PROTOCOL_VERSION, SUPPORTED_VERSIONS, Server};
pub use tools::{TOOL_NAMES, call_tool, tool_definitions};

/// Width and height of a PNG from its IHDR chunk.
pub use figcraft_agent_tools::png_size;
