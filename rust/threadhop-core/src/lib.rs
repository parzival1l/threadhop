//! ThreadHop core — pure data and I/O. No TUI imports.
//!
//! This crate is the data layer for the Rust TUI port. It owns:
//! - Filesystem paths (`paths`) and the compile-time `EXPECTED_SCHEMA_VERSION`.
//! - Serde row/JSONL shapes (`models`) and per-module error enums (`error`).
//! - SQLite read/write helpers (`db`) and FTS5 search (`fts`).
//! - JSONL transcript cleaning (`jsonl`), observation reads (`observations`).
//! - macOS session detection (`session_detect`, feature-gated on `async`).
//! - OpenCode theme loader (`theme`) and recent-searches config I/O (`recent_searches`).
//!
//! Module bodies are populated by Phase 1 tasks 1.2–1.15 of the Rust TUI port plan.

pub mod db;
pub mod digest;
pub mod error;
pub mod fts;
pub mod jsonl;
pub mod models;
pub mod observations;
pub mod paths;
pub mod recent_searches;
pub mod session_detect;
pub mod theme;

pub const EXPECTED_SCHEMA_VERSION: u32 = 9;
