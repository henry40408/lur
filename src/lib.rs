//! `lur` — a sandboxed Lua (Luau) script runtime.
//!
//! This crate exposes the shared core used by both execution modes (one-shot
//! and server). The binary in `main.rs` is a thin CLI on top of it.

pub mod capabilities;
pub mod policy;
pub mod runtime;
