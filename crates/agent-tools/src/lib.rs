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
