//! Mirrors `threadhop_core/models.py`. ADR-004: enums here pair with CHECK
//! constraints in `storage/db.py` — keep in lockstep.
//!
//! Two responsibilities live here:
//!
//! 1. **DB row shapes** (`Session`, `Message`, `Bookmark`, `MemoryEntry`) plus
//!    the `Literal`-equivalent enums (`SessionStatus`, `MessageRole`,
//!    `MemoryType`, `MemorySource`, `BookmarkKind`). Variants are
//!    `#[serde(rename_all = "snake_case")]` so the wire form matches the SQL
//!    CHECK values exactly.
//! 2. **JSONL transcript line types** plus [`parse_transcript_line`], the
//!    typed boundary the indexer (Task 1.10) folds over. Malformed lines log
//!    a warning and yield `None` — one bad line never wedges the indexer.

use serde::{Deserialize, Serialize};

// --- Enum-like aliases ---------------------------------------------------
// Mirrors of Python `Literal[...]` types. snake_case rename keeps the wire
// form aligned with the SQL CHECK constraints in `db.py` (ADR-004).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Active,
    InProgress,
    InReview,
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Decision,
    Todo,
    Done,
    Adr,
    Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    #[default]
    Explicit,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarkKind {
    #[default]
    Bookmark,
    Research,
}

// --- DB row shapes -------------------------------------------------------
// One struct per SQL table; pairs with a migration in `db.rs`.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub session_path: String,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub custom_name: Option<String>,
    #[serde(default)]
    pub status: SessionStatus,
    pub sort_order: Option<i64>,
    pub last_viewed: Option<f64>,
    pub created_at: Option<f64>,
    pub modified_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub uuid: String,
    pub session_id: String,
    pub role: MessageRole,
    pub text: String,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub parent_uuid: Option<String>,
    #[serde(default)]
    pub is_sidechain: bool,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Option<i64>,
    pub message_uuid: String,
    pub note: Option<String>,
    #[serde(default)]
    pub kind: BookmarkKind,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: Option<i64>,
    pub project: String,
    #[serde(rename = "type")]
    pub memory_type: MemoryType,
    pub text: String,
    pub session_id: Option<String>,
    #[serde(default)]
    pub source: MemorySource,
    #[serde(default)]
    pub resolved: bool,
    pub created_at: f64,
}

// --- JSONL content blocks ------------------------------------------------
// Discriminated union over the three block shapes Claude Code uses inside
// `message.content`. `#[serde(other)]` on `Other` tolerates future block
// types without rejecting the whole line.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
    #[serde(other)]
    Other,
}

/// User-line content is either a plain string (normal prompts) or a list of
/// blocks (tool-result routing back to the model).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessagePayload {
    pub role: String,
    pub content: UserContent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessagePayload {
    /// Shared across streaming chunks (ADR-003). Optional so a missing id
    /// doesn't drop the whole line; the indexer falls back to `uuid`.
    #[serde(default)]
    pub id: Option<String>,
    pub role: String,
    #[serde(default)]
    pub model: Option<String>,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserTranscriptLine {
    pub uuid: String,
    #[serde(rename = "parentUuid", default)]
    pub parent_uuid: Option<String>,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,
    pub message: UserMessagePayload,
    #[serde(rename = "toolUseResult", default)]
    pub tool_use_result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantTranscriptLine {
    pub uuid: String,
    #[serde(rename = "parentUuid", default)]
    pub parent_uuid: Option<String>,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,
    pub message: AssistantMessagePayload,
}

#[derive(Debug, Clone)]
pub enum TranscriptLine {
    User(UserTranscriptLine),
    Assistant(AssistantTranscriptLine),
}

/// Parse one JSONL line. Returns `None` for malformed JSON, non-object roots,
/// `summary` / `ai-title` / `custom-title` / `system` / `meta` / unknown
/// types, or validation failures. Logs at warn level on validation failure
/// (parity with the Python implementation).
pub fn parse_transcript_line(raw: &[u8]) -> Option<TranscriptLine> {
    let value: serde_json::Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let obj = value.as_object()?;
    let ty = obj.get("type")?.as_str()?;
    match ty {
        "summary" | "ai-title" | "custom-title" | "system" | "meta" => None,
        "user" => match serde_json::from_value::<UserTranscriptLine>(value) {
            Ok(u) => Some(TranscriptLine::User(u)),
            Err(e) => {
                tracing::warn!("user line validation failed: {e}");
                None
            }
        },
        "assistant" => match serde_json::from_value::<AssistantTranscriptLine>(value) {
            Ok(a) => Some(TranscriptLine::Assistant(a)),
            Err(e) => {
                tracing::warn!("assistant line validation failed: {e}");
                None
            }
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task 1.3: enums + DB row shapes ---------------------------------

    #[test]
    fn session_status_round_trips_snake_case() {
        let s = SessionStatus::InProgress;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"in_progress\"");
        let back: SessionStatus = serde_json::from_str("\"in_progress\"").unwrap();
        assert!(matches!(back, SessionStatus::InProgress));
    }

    #[test]
    fn session_status_rejects_unknown() {
        let r: Result<SessionStatus, _> = serde_json::from_str("\"backlog\"");
        assert!(r.is_err(), "ADR-004: unknown status must be rejected");
    }

    #[test]
    fn message_role_round_trips() {
        assert_eq!(
            serde_json::to_string(&MessageRole::User).unwrap(),
            "\"user\""
        );
        assert_eq!(
            serde_json::to_string(&MessageRole::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn bookmark_kind_defaults_to_bookmark() {
        let raw = r#"{"id":1,"message_uuid":"u","created_at":0.0}"#;
        let b: Bookmark = serde_json::from_str(raw).unwrap();
        assert!(matches!(b.kind, BookmarkKind::Bookmark));
        assert!(b.tags.is_empty());
    }

    // --- Task 1.4: JSONL transcript line types ---------------------------

    #[test]
    fn parses_user_text_line() {
        let raw = br#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-05-20T00:00:00Z","message":{"role":"user","content":"hello"}}"#;
        let line = parse_transcript_line(raw).unwrap();
        match line {
            TranscriptLine::User(u) => {
                assert_eq!(u.uuid, "u1");
                assert_eq!(u.session_id.as_deref(), Some("s1"));
            }
            _ => panic!("expected user line"),
        }
    }

    #[test]
    fn parses_assistant_streaming_chunk_with_id() {
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"msg_abc","role":"assistant","content":[{"type":"text","text":"hi"}]}}"#;
        let line = parse_transcript_line(raw).unwrap();
        match line {
            TranscriptLine::Assistant(a) => {
                assert_eq!(a.message.id.as_deref(), Some("msg_abc"));
            }
            _ => panic!("expected assistant line"),
        }
    }

    #[test]
    fn skips_summary_and_unknown_types() {
        assert!(parse_transcript_line(br#"{"type":"summary","uuid":"x"}"#).is_none());
        assert!(parse_transcript_line(br#"{"type":"weirdo","uuid":"x"}"#).is_none());
    }

    #[test]
    fn returns_none_on_invalid_json() {
        assert!(parse_transcript_line(b"not json").is_none());
    }

    #[test]
    fn parent_uuid_alias_works() {
        let raw = br#"{"type":"user","uuid":"u","parentUuid":"p","sessionId":"s","message":{"role":"user","content":"x"}}"#;
        let line = parse_transcript_line(raw).unwrap();
        match line {
            TranscriptLine::User(u) => assert_eq!(u.parent_uuid.as_deref(), Some("p")),
            _ => panic!(),
        }
    }
}
