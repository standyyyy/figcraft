//! FigCraft's embedded Agent runtime.
//!
//! Wraps Rig's agent loop, runs tool calls against a shadow [`figcraft_engine::Session`], and
//! commits a completed turn to the live session as a single `command.batch` undo step. This crate
//! is L6 and UI-independent: the egui panel drives it through an `AgentHost` implemented by each
//! application target.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
