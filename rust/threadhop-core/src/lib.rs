//! ThreadHop core — pure data and I/O. No TUI imports.
//!
//! This crate is the data layer for the Rust TUI port. It owns:
//! - Filesystem paths (`paths`) and the compile-time `EXPECTED_SCHEMA_VERSION`.
//! - Serde row/JSONL shapes (`models`) and per-module error enums (`error`).
//! - SQLite read/write helpers (`db`) and FTS5 search (`fts`).
//! - JSONL transcript cleaning (`jsonl`) and the exchange model (`exchanges`).
//! - Transfer tickets for `prepare` / `receive` (`transfers`) and the
//!   `claude -p` subprocess adapter (`harness`) — the ADR-029 borrow surface.
//! - macOS session detection (`session_detect`, feature-gated on `async`).
//! - OpenCode theme loader (`theme`) and recent-searches config I/O (`recent_searches`).
//!
//! Module bodies are populated by Phase 1 tasks 1.2–1.15 of the Rust TUI port
//! plan. ADR-029 removed the observation/reflector/conflict layer in favor of
//! the lazy borrow surface (peek / search / prepare / receive).

pub mod db;
pub mod digest;
pub mod error;
pub mod exchanges;
pub mod fts;
pub mod harness;
pub mod jsonl;
pub mod models;
pub mod paths;
pub mod recent_searches;
pub mod session_detect;
pub mod theme;
pub mod transfers;

/// Python owns the schema (threadhop_core/storage/db.py). Version 11 =
/// migration 010 (drop observation_state + conflict_reviews, ADR-029) +
/// migration 011 (transfer_state summary cache, ADR-033).
pub const EXPECTED_SCHEMA_VERSION: u32 = 11;
