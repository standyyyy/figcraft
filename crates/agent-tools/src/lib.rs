//! FigCraft's shared canvas tool catalog.
//!
//! Both the MCP server and the in-app Agent consume these definitions, so a tool behaves the same
//! on every surface. Each surface selects a [`ToolPolicy`]: external MCP exposes the full set, while
//! the embedded Agent is limited to document-local, replayable tools.
//!
//! This crate is L6 and UI-independent: it never depends on egui, eframe, rfd or winit.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod backend;
pub mod catalog;

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;

#[cfg(test)]
#[path = "gradient_tests.rs"]
mod gradient_tests;

pub use backend::{Backend, decode_data, page_children};
pub use catalog::{TOOL_NAMES, ToolPolicy, ToolResult, call_tool, tool_definitions};

/// Width and height of a PNG from its IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((w, h))
}
