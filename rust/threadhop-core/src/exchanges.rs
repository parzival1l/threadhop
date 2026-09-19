//! Exchange model — the shared conversation unit for peek/prepare (ADR-030).
//!
//! An *exchange* is one real user turn (a human prompt — NOT a tool-result
//! row, which is `type=user` in the JSONL but carries tool output) plus all
//! assistant activity until the next real user turn. Exchanges are derived
//! at parse time from the session JSONL; nothing is persisted.
//!
//! Direct port of `threadhop_core/exchanges.py`. Cleaning matches the
//! `threadhop copy` pipeline exactly:
//!
//! * user rows: `toolUseResult` rows are skipped; `<system-reminder>` /
//!   `<local-command-*>` / `<command-*>` blocks are stripped and skill-load
//!   banners collapse to nothing (via [`crate::jsonl::clean_user_text`]).
//! * assistant rows: only `text` blocks survive (`tool_use` and `thinking`
//!   blocks are dropped entirely); consecutive assistant lines sharing
//!   `message.id` merge into one logical turn (ADR-003).
//! * sidechain rows are dropped.
//! * [`strip_harness_tags`] removes `!cmd` bash-passthrough wrappers
//!   (`<bash-input>` / `<bash-stdout>` / `<bash-stderr>` /
//!   `<local-command-caveat>`) and slash-command markup.
//!
//! This module additionally tracks the **byte offsets** of each exchange in
//! the source JSONL so `threadhop prepare` can cache its head summary
//! against a stable position (ADR-033) without a second parse.

use once_cell::sync::Lazy;
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::jsonl::{clean_user_text, strip_system_reminders};

/// Harness-tooling wrappers Claude Code surfaces as plain-text content inside
/// user JSONL turns. None of this is conversation — strip it so a rendered
/// exchange reads like a chat, not a shell transcript.
///
/// Python's `HARNESS_TAG_RE` uses a backreference (`</\1>`); Rust's `regex`
/// crate does not support backreferences, so the closing tag enumerates the
/// same alternation. The corpus only emits matched pairs, so the looser form
/// is parity-safe (same reasoning as `jsonl::COMMAND_BLOCK_RE`).
static HARNESS_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)<(?:bash-input|bash-stdout|bash-stderr|local-command-caveat|command-name|command-message|command-args)>.*?</(?:bash-input|bash-stdout|bash-stderr|local-command-caveat|command-name|command-message|command-args)>",
    )
    .unwrap()
});

/// Strip `!cmd` bash-passthrough wrappers and slash-command markup, then trim.
pub fn strip_harness_tags(text: &str) -> String {
    HARNESS_TAG_RE.replace_all(text, "").trim().to_string()
}

/// One cleaned, human-visible conversation row in file order.
///
/// `start_offset` is the byte offset of the first JSONL line contributing to
/// the row; `end_offset` is one past the final byte (including the trailing
/// newline) of the last contributing line — for merged assistant chunks that
/// spans every chunk line.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanRow {
    pub role: RowRole,
    pub text: String,
    pub timestamp: Option<String>,
    pub start_offset: u64,
    pub end_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRole {
    User,
    Assistant,
}

/// Extract the user-visible text from a user JSONL object, mirroring
/// Python's `indexer._extract_user_text`. Returns `None` for tool-result
/// rows (`toolUseResult`) and rows that clean to nothing.
fn extract_user_text(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let is_tool_result = obj
        .get("toolUseResult")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    if is_tool_result {
        return None;
    }
    let content = obj.get("message").and_then(|m| m.get("content"));
    let raw = match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                let bo = b.as_object()?;
                if bo.get("type").and_then(|t| t.as_str())? == "text" {
                    Some(
                        bo.get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let cleaned = clean_user_text(&raw);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Extract the prose text blocks from an assistant JSONL object — the
/// `include_tool_calls=False` variant of `indexer._extract_assistant_blocks`.
/// `tool_use` and `thinking` blocks are dropped entirely.
fn extract_assistant_text_blocks(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let content = match obj.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for block in content {
        let bo = match block.as_object() {
            Some(b) => b,
            None => continue,
        };
        if bo.get("type").and_then(|t| t.as_str()) == Some("text") {
            let t = strip_system_reminders(bo.get("text").and_then(|x| x.as_str()).unwrap_or(""));
            if !t.is_empty() {
                out.push(t);
            }
        }
        // tool_use / thinking / other — skipped
    }
    out
}

/// In-flight assistant chunk-merge buffer (ADR-003 semantics).
struct AssistantBuffer {
    message_id: Option<String>,
    parts: Vec<String>,
    timestamp: Option<String>,
    start_offset: u64,
    end_offset: u64,
}

impl AssistantBuffer {
    /// Flush into a [`CleanRow`], or `None` when the merged text cleans to
    /// nothing.
    fn flush(self) -> Option<CleanRow> {
        let joined = self
            .parts
            .iter()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = strip_harness_tags(joined.trim());
        if text.is_empty() {
            return None;
        }
        Some(CleanRow {
            role: RowRole::Assistant,
            text,
            timestamp: self.timestamp,
            start_offset: self.start_offset,
            end_offset: self.end_offset,
        })
    }
}

/// Read cleaned, human-visible conversation rows from `session_path` in
/// file order. Malformed JSON lines are silently skipped; an unreadable file
/// yields an empty list (mirrors Python's `iter_clean_rows`, which returns
/// early on `OSError`).
pub fn read_clean_rows(session_path: &Path) -> Vec<CleanRow> {
    let file = match std::fs::File::open(session_path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut reader = BufReader::new(file);
    let mut rows: Vec<CleanRow> = Vec::new();
    let mut buf: Option<AssistantBuffer> = None;
    let mut offset: u64 = 0;
    let mut raw = Vec::new();

    loop {
        raw.clear();
        let n = match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let line_start = offset;
        offset += n as u64;
        let line_end = offset;

        let text = String::from_utf8_lossy(&raw);
        let msg: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = match msg.as_object() {
            Some(o) => o,
            None => continue,
        };
        let mtype = obj.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if mtype != "user" && mtype != "assistant" {
            continue;
        }
        if obj
            .get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let timestamp = obj
            .get("timestamp")
            .and_then(|x| x.as_str())
            .map(String::from);

        if mtype == "user" {
            if let Some(b) = buf.take() {
                if let Some(row) = b.flush() {
                    rows.push(row);
                }
            }
            let Some(text) = extract_user_text(obj) else {
                continue;
            };
            let text = strip_harness_tags(&text);
            if text.is_empty() {
                continue;
            }
            rows.push(CleanRow {
                role: RowRole::User,
                text,
                timestamp,
                start_offset: line_start,
                end_offset: line_end,
            });
            continue;
        }

        // --- assistant line ---
        let mid = obj
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let parts = extract_assistant_text_blocks(obj);

        // Streaming chunk of the current logical message → append.
        if let (Some(mid_ref), Some(current)) = (mid.as_ref(), buf.as_mut()) {
            if current.message_id.as_ref() == Some(mid_ref) {
                current.parts.extend(parts);
                current.end_offset = line_end;
                continue;
            }
        }

        if let Some(b) = buf.take() {
            if let Some(row) = b.flush() {
                rows.push(row);
            }
        }
        buf = Some(AssistantBuffer {
            message_id: mid,
            parts,
            timestamp,
            start_offset: line_start,
            end_offset: line_end,
        });
    }

    if let Some(b) = buf.take() {
        if let Some(row) = b.flush() {
            rows.push(row);
        }
    }
    rows
}

// --- Exchange grouping -----------------------------------------------------

/// One user turn plus everything until the next user turn.
///
/// `user_text` is `None` for a leading exchange in transcripts that open
/// with assistant output (rare, but resumed sessions can). Offsets reference
/// the source JSONL bytes — see [`read_clean_rows`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exchange {
    pub user_text: Option<String>,
    pub assistant_texts: Vec<String>,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
    pub start_offset: u64,
    pub end_offset: u64,
}

impl Exchange {
    /// Render as `User:` / `Assistant:` labelled blocks.
    pub fn render(&self) -> String {
        let mut blocks: Vec<String> = Vec::new();
        if let Some(user) = &self.user_text {
            blocks.push(format!("User:\n{user}"));
        }
        for text in &self.assistant_texts {
            blocks.push(format!("Assistant:\n{text}"));
        }
        blocks.join("\n\n")
    }

    /// Plain concatenated text — the grep target for `peek --grep`.
    pub fn text(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(user) = &self.user_text {
            parts.push(user.as_str());
        }
        parts.extend(self.assistant_texts.iter().map(|s| s.as_str()));
        parts.join("\n\n")
    }
}

/// Parse a session JSONL into its ordered list of exchanges.
///
/// A new exchange starts at every real user turn (tool-result rows were
/// already dropped by [`read_clean_rows`], so they never split an exchange).
/// Assistant rows before the first user turn group into a leading exchange
/// with `user_text = None`.
pub fn load_exchanges(session_path: &Path) -> Vec<Exchange> {
    let mut exchanges: Vec<Exchange> = Vec::new();
    let mut current: Option<Exchange> = None;

    for row in read_clean_rows(session_path) {
        match row.role {
            RowRole::User => {
                if let Some(ex) = current.take() {
                    exchanges.push(ex);
                }
                current = Some(Exchange {
                    user_text: Some(row.text),
                    assistant_texts: Vec::new(),
                    first_timestamp: row.timestamp.clone(),
                    last_timestamp: row.timestamp,
                    start_offset: row.start_offset,
                    end_offset: row.end_offset,
                });
            }
            RowRole::Assistant => {
                let ex = current.get_or_insert_with(|| Exchange {
                    user_text: None,
                    assistant_texts: Vec::new(),
                    first_timestamp: row.timestamp.clone(),
                    last_timestamp: None,
                    start_offset: row.start_offset,
                    end_offset: 0,
                });
                ex.assistant_texts.push(row.text);
                if row.timestamp.is_some() {
                    ex.last_timestamp = row.timestamp;
                }
                ex.end_offset = ex.end_offset.max(row.end_offset);
            }
        }
    }

    if let Some(ex) = current.take() {
        exchanges.push(ex);
    }
    exchanges
}

/// Render several exchanges, blank-line separated.
pub fn render_exchanges(exchanges: &[Exchange]) -> String {
    exchanges
        .iter()
        .map(Exchange::render)
        .collect::<Vec<_>>()
        .join("\n\n")
}

// --- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user_line(uuid: &str, text: &str, ts: &str) -> String {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "timestamp": ts,
            "message": {"content": [{"type": "text", "text": text}]},
        })
        .to_string()
    }

    fn assistant_line(uuid: &str, mid: &str, text: &str, ts: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "timestamp": ts,
            "message": {"id": mid, "content": [{"type": "text", "text": text}]},
        })
        .to_string()
    }

    fn tool_result_line(uuid: &str) -> String {
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "toolUseResult": {"output": "ok"},
            "message": {"content": [{"type": "tool_result", "content": "raw tool output"}]},
        })
        .to_string()
    }

    fn write_session(lines: &[String]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        (dir, path)
    }

    #[test]
    fn groups_one_exchange_per_real_user_turn() {
        let (_d, path) = write_session(&[
            user_line("u1", "first question", "2026-01-01T00:00:01Z"),
            assistant_line("a1", "m1", "first answer", "2026-01-01T00:00:02Z"),
            user_line("u2", "second question", "2026-01-01T00:00:03Z"),
            assistant_line("a2", "m2", "second answer", "2026-01-01T00:00:04Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 2);
        assert_eq!(exs[0].user_text.as_deref(), Some("first question"));
        assert_eq!(exs[0].assistant_texts, vec!["first answer"]);
        assert_eq!(exs[1].user_text.as_deref(), Some("second question"));
    }

    #[test]
    fn tool_result_rows_do_not_split_an_exchange() {
        // The tool-result user row is type=user in the JSONL, but it is NOT
        // a human prompt — the assistant activity around it stays in ONE
        // exchange.
        let (_d, path) = write_session(&[
            user_line("u1", "run the tests", "2026-01-01T00:00:01Z"),
            assistant_line("a1", "m1", "running now", "2026-01-01T00:00:02Z"),
            tool_result_line("tr1"),
            assistant_line("a2", "m2", "all green", "2026-01-01T00:00:03Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 1, "tool-result row must not start a new exchange");
        assert_eq!(exs[0].assistant_texts, vec!["running now", "all green"]);
        assert!(!exs[0].text().contains("raw tool output"));
    }

    #[test]
    fn assistant_chunks_sharing_message_id_merge() {
        let (_d, path) = write_session(&[
            user_line("u1", "q", "2026-01-01T00:00:01Z"),
            assistant_line("a1", "m1", "part one", "2026-01-01T00:00:02Z"),
            assistant_line("a2", "m1", "part two", "2026-01-01T00:00:03Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 1);
        assert_eq!(exs[0].assistant_texts, vec!["part one\n\npart two"]);
    }

    #[test]
    fn sidechain_rows_are_dropped() {
        let sidechain = serde_json::json!({
            "type": "assistant",
            "uuid": "sc1",
            "isSidechain": true,
            "message": {"id": "ms", "content": [{"type": "text", "text": "subagent noise"}]},
        })
        .to_string();
        let (_d, path) = write_session(&[
            user_line("u1", "q", "2026-01-01T00:00:01Z"),
            sidechain,
            assistant_line("a1", "m1", "real answer", "2026-01-01T00:00:02Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 1);
        assert_eq!(exs[0].assistant_texts, vec!["real answer"]);
    }

    #[test]
    fn leading_assistant_rows_form_exchange_with_no_user_text() {
        let (_d, path) = write_session(&[
            assistant_line("a0", "m0", "resumed context", "2026-01-01T00:00:01Z"),
            user_line("u1", "q", "2026-01-01T00:00:02Z"),
            assistant_line("a1", "m1", "a", "2026-01-01T00:00:03Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 2);
        assert!(exs[0].user_text.is_none());
        assert_eq!(exs[0].assistant_texts, vec!["resumed context"]);
    }

    #[test]
    fn harness_tags_are_stripped_from_user_text() {
        let text = "<bash-input>ls</bash-input><bash-stdout>a b c</bash-stdout>real words";
        let (_d, path) = write_session(&[user_line("u1", text, "2026-01-01T00:00:01Z")]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 1);
        assert_eq!(exs[0].user_text.as_deref(), Some("real words"));
    }

    #[test]
    fn system_reminders_are_stripped() {
        let text = "keep this<system-reminder>drop this</system-reminder>";
        let (_d, path) = write_session(&[
            user_line("u1", text, "2026-01-01T00:00:01Z"),
            assistant_line(
                "a1",
                "m1",
                "answer<system-reminder>noise</system-reminder>",
                "2026-01-01T00:00:02Z",
            ),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs[0].user_text.as_deref(), Some("keep this"));
        assert_eq!(exs[0].assistant_texts, vec!["answer"]);
    }

    #[test]
    fn offsets_track_source_jsonl_bytes() {
        let l1 = user_line("u1", "q1", "2026-01-01T00:00:01Z");
        let l2 = assistant_line("a1", "m1", "a1", "2026-01-01T00:00:02Z");
        let l3 = user_line("u2", "q2", "2026-01-01T00:00:03Z");
        let (_d, path) = write_session(&[l1.clone(), l2.clone(), l3.clone()]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 2);
        // +1 per line for the trailing newline written by writeln!.
        let l1_end = (l1.len() + 1) as u64;
        let l2_end = l1_end + (l2.len() + 1) as u64;
        let l3_end = l2_end + (l3.len() + 1) as u64;
        assert_eq!(exs[0].start_offset, 0);
        assert_eq!(exs[0].end_offset, l2_end);
        assert_eq!(exs[1].start_offset, l2_end);
        assert_eq!(exs[1].end_offset, l3_end);
    }

    #[test]
    fn render_labels_user_and_assistant_blocks() {
        let ex = Exchange {
            user_text: Some("hello".into()),
            assistant_texts: vec!["hi".into(), "more".into()],
            ..Default::default()
        };
        assert_eq!(ex.render(), "User:\nhello\n\nAssistant:\nhi\n\nAssistant:\nmore");
        assert_eq!(ex.text(), "hello\n\nhi\n\nmore");
    }

    #[test]
    fn missing_file_yields_no_exchanges() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.jsonl");
        assert!(load_exchanges(&missing).is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let (_d, path) = write_session(&[
            "not json at all".to_string(),
            user_line("u1", "q", "2026-01-01T00:00:01Z"),
        ]);
        let exs = load_exchanges(&path);
        assert_eq!(exs.len(), 1);
    }
}
