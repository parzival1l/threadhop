//! Per-session digest — the data shape behind the right-hand digest
//! panel.
//!
//! **Pre-pop scaffolding.** Worker H fills in the real
//! [`compute_session_digest`] builder in the deferrals-cleanup wave by
//! porting `threadhop_core/session/digest.py::extract_digest` to Rust.
//! Until then, the stub returns a digest with only `session_id` and a
//! short truncated `slug` populated so callers can render an "empty
//! digest" panel without crashing.
//!
//! The struct field set mirrors the Python `SessionDigest` dataclass —
//! the computed-property names from Python (`title`, `cache_hit_ratio`,
//! `context_fill_ratio`, `files_touched_count`,
//! `total_input_tokens_billed`) are flattened into regular fields here
//! because Rust doesn't have lazy properties on data structs. Worker H
//! computes those values during the JSONL pass and stores them
//! directly.

use rusqlite::Connection;

/// One band of the recap timeline (Started / Earlier / Recently / Last
/// asked). `timestamp` is the source ISO timestamp where available; the
/// renderer uses it to compute a relative-age suffix.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecapEntry {
    pub label: String,
    pub timestamp: Option<String>,
    pub text: String,
}

/// A single session, summarised for the right-column digest panel.
///
/// Worker H populates this from a single pass over the JSONL plus a
/// small SQLite lookup for resume-command metadata. Pre-pop only
/// guarantees `session_id` and a slug derived from it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDigest {
    /// Stable session UUID.
    pub session_id: String,

    /// Best display title — custom title > ai title > slug > truncated
    /// session id. Empty until Worker H runs.
    pub title: String,

    /// Short identifier — typically the first 8 chars of `session_id`,
    /// or a user-supplied slug.
    pub slug: String,

    /// Git branch the session was opened on, if recorded.
    pub branch: Option<String>,

    /// Wall-clock duration (seconds) from first user prompt to last
    /// event observed.
    pub duration_seconds: Option<u64>,

    /// Recap bands in display order — empty when nothing is available
    /// yet (fresh sessions with no completed turn).
    pub recap: Vec<RecapEntry>,

    /// PR number captured from a `pr-link` line, if any.
    pub pr_number: Option<u64>,

    /// Repository the PR lives in (e.g. `owner/repo`).
    pub pr_repository: Option<String>,

    /// Distinct file paths the assistant edited / wrote during the
    /// session. Sorted, deduped.
    pub files_touched: Vec<String>,

    /// `files_touched.len() as u64`, precomputed so the renderer
    /// doesn't have to call `.len()` on a Vec it doesn't own.
    pub files_touched_count: u64,

    /// Context window in tokens for the most recent assistant model.
    /// `None` when no assistant turn has reported usage yet.
    pub context_window: Option<u64>,

    /// Input-tokens count for the latest assistant turn — drives the
    /// "context fill" gauge.
    pub latest_turn_input_tokens: u64,

    /// Cumulative TOTAL input tokens across the session (uncached +
    /// cache_read + cache_creation). Matches the Python
    /// `total_input_tokens_billed` computed property.
    pub total_input_tokens_billed: u64,

    /// Cumulative output tokens.
    pub total_output_tokens: u64,

    /// `latest_turn_input_tokens / context_window`, clamped to 1.0.
    /// `None` until at least one assistant turn has run.
    pub context_fill_ratio: Option<f32>,

    /// `cache_read / (cache_read + cache_creation)`. `None` when the
    /// denominator is zero.
    pub cache_hit_ratio: Option<f32>,

    /// Models actually used in the session, in first-seen order.
    pub models_used: Vec<String>,

    /// Permission mode captured from a `permission-mode` line.
    pub permission_mode: Option<String>,

    /// Client version captured from any line carrying a `version`
    /// field.
    pub client_version: Option<String>,
}

/// Build a session digest. **Pre-pop stub.** Worker H replaces the body
/// with a real JSONL pass; until then this returns a digest with just
/// `session_id` and a slug derived from the first 8 chars of the id.
///
/// The signature is fixed: workers and tests can wire calls without
/// risking a churn when the real implementation lands.
pub fn compute_session_digest(session_id: &str, _conn: &Connection) -> SessionDigest {
    SessionDigest {
        session_id: session_id.to_string(),
        slug: session_id.chars().take(8).collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_populates_session_id_and_slug() {
        let conn = Connection::open_in_memory().unwrap();
        let digest = compute_session_digest(
            "abcdef1234567890-rest-of-uuid",
            &conn,
        );
        assert_eq!(digest.session_id, "abcdef1234567890-rest-of-uuid");
        // Slug is the first 8 chars of the session id — matches the
        // Python `title` fallback semantics when no custom title /
        // ai title / slug field is set.
        assert_eq!(digest.slug, "abcdef12");
    }

    #[test]
    fn stub_leaves_other_fields_default() {
        let conn = Connection::open_in_memory().unwrap();
        let digest = compute_session_digest("session-id", &conn);
        // Pre-pop guarantee: every other field is Default. This protects
        // Worker H from accidentally relying on side-effects from the
        // stub when wiring real population.
        assert!(digest.title.is_empty());
        assert_eq!(digest.branch, None);
        assert_eq!(digest.duration_seconds, None);
        assert!(digest.recap.is_empty());
        assert_eq!(digest.pr_number, None);
        assert!(digest.files_touched.is_empty());
        assert_eq!(digest.files_touched_count, 0);
        assert_eq!(digest.context_window, None);
        assert_eq!(digest.latest_turn_input_tokens, 0);
        assert_eq!(digest.total_input_tokens_billed, 0);
        assert_eq!(digest.total_output_tokens, 0);
        assert_eq!(digest.context_fill_ratio, None);
        assert_eq!(digest.cache_hit_ratio, None);
        assert!(digest.models_used.is_empty());
        assert_eq!(digest.permission_mode, None);
        assert_eq!(digest.client_version, None);
    }

    #[test]
    fn slug_handles_short_session_id() {
        let conn = Connection::open_in_memory().unwrap();
        let digest = compute_session_digest("abc", &conn);
        // Slug never panics on a short id — just takes whatever's
        // there. Worker H may eventually override this from a real
        // `slug` line in the JSONL.
        assert_eq!(digest.slug, "abc");
    }

    #[test]
    fn recap_entry_default_is_empty() {
        let entry = RecapEntry::default();
        assert!(entry.label.is_empty());
        assert!(entry.text.is_empty());
        assert_eq!(entry.timestamp, None);
    }
}
