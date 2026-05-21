//! JSONL transcript cleaning + ADR-003 chunk merging.
//!
//! Direct port of `threadhop_core/indexer.py`'s `parse_byte_range` and the
//! text-cleaning helpers it leans on. The contract is identical: feed in the
//! raw bytes of (a window of) a Claude Code session JSONL, get back a list of
//! cleaned message groups where consecutive assistant lines sharing the same
//! `message.id` have been merged into one row, `<system-reminder>` blocks
//! have been stripped, and `tool_use` blocks have been abbreviated to one
//! human-readable line each.
//!
//! Anti-pattern (from CLAUDE.md): "Don't feed the observer raw JSONL." This
//! module is the cleaned-transcript view shared by the TUI and the observer.
//!
//! No DB or filesystem I/O lives here — callers stream bytes in and serialize
//! output however they need.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::path::Path;

// --- Compiled regexes --------------------------------------------------------

/// `<system-reminder>…</system-reminder>`, spanning newlines.
static SYSTEM_REMINDER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<system-reminder>.*?</system-reminder>").unwrap()
});

/// `<local-command-{caveat,stdout,stderr}>…</local-command-…>` — bash-passthrough
/// harness plumbing, stripped before indexing.
static LOCAL_COMMAND_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)<local-command-(?:caveat|stdout|stderr)>.*?</local-command-(?:caveat|stdout|stderr)>",
    )
    .unwrap()
});

/// `<command-name|command-message|command-args>…</…>` — slash-command markup.
/// The Python source uses a backreference (`</\1>`); Rust's `regex` crate does
/// not support backreferences, so we enumerate the matched closing tags
/// explicitly. The set of valid open/close pairs is closed (three tags) and
/// the corpus only emits matched pairs, so the looser form is parity-safe.
static COMMAND_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)<(?:command-name|command-message|command-args)>.*?</(?:command-name|command-message|command-args)>",
    )
    .unwrap()
});

/// Skill-load banner that Claude Code injects as a synthetic user line when a
/// skill loads. Matching this on `lstrip()`'d text means the whole user line
/// collapses to "" — see `clean_user_text`.
static SKILL_LOAD_BANNER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*Base directory for this skill:\s*(\S+)").unwrap()
});

// --- Text cleanup ------------------------------------------------------------

/// Strip `<system-reminder>` blocks and trim surrounding whitespace.
///
/// Mirrors `indexer.strip_system_reminders`.
pub fn strip_system_reminders(text: &str) -> String {
    SYSTEM_REMINDER_RE.replace_all(text, "").trim().to_string()
}

/// Strip every flavour of harness chrome from a user-line content string.
///
/// Removes `<system-reminder>` blocks, `<local-command-*>` bash-passthrough
/// plumbing, and `<command-{name,message,args}>` slash-command markup. If
/// the residue is a skill-load banner, the whole line collapses to `""` —
/// it's plumbing, not user content.
///
/// Mirrors `indexer.clean_user_text`.
pub fn clean_user_text(text: &str) -> String {
    let s1 = SYSTEM_REMINDER_RE.replace_all(text, "");
    let s2 = LOCAL_COMMAND_BLOCK_RE.replace_all(&s1, "");
    let s3 = COMMAND_BLOCK_RE.replace_all(&s2, "");
    let trimmed_left = s3.trim_start();
    if SKILL_LOAD_BANNER_RE.is_match(trimmed_left) {
        return String::new();
    }
    s3.trim().to_string()
}

/// Render a `tool_use` block as one human-readable line.
///
/// Mirrors `indexer.abbreviate_tool_use`. Kept in lockstep with the TUI
/// renderer on purpose — a divergence would mean FTS hits point at labels
/// the user never saw.
pub fn abbreviate_tool_use(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let get = |key: &str| -> &str {
        tool_input
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let basename = |p: &str| -> String {
        if p.is_empty() {
            String::new()
        } else {
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        }
    };
    match tool_name {
        "Read" => {
            let p = get("file_path");
            if p.is_empty() {
                "Reading file".to_string()
            } else {
                format!("Reading {}", basename(p))
            }
        }
        "Write" => {
            let p = get("file_path");
            if p.is_empty() {
                "Writing file".to_string()
            } else {
                format!("Writing {}", basename(p))
            }
        }
        "Edit" => {
            let p = get("file_path");
            if p.is_empty() {
                "Editing file".to_string()
            } else {
                format!("Editing {}", basename(p))
            }
        }
        "Bash" => {
            let cmd = get("command");
            let first = cmd.split_whitespace().next().unwrap_or("command");
            format!("Running {first}")
        }
        "Glob" => {
            let pat = get("pattern");
            if pat.is_empty() {
                "Searching files".to_string()
            } else {
                format!("Searching for {pat}")
            }
        }
        "Grep" => {
            let pat = get("pattern");
            if pat.is_empty() {
                "Searching content".to_string()
            } else {
                format!("Searching for '{pat}'")
            }
        }
        "Agent" => {
            let desc = get("description");
            if desc.is_empty() {
                "Running agent".to_string()
            } else {
                format!("Agent: {desc}")
            }
        }
        "WebFetch" => {
            let url = get("url");
            if url.len() > 50 {
                format!("Fetching {}...", &url[..50])
            } else {
                format!("Fetching {url}")
            }
        }
        "WebSearch" => {
            let q = get("query");
            format!("Searching web for '{q}'")
        }
        "TodoWrite" => "Updating todo list".to_string(),
        other => other.to_string(),
    }
}

// --- parse_byte_range --------------------------------------------------------

/// One cleaned message group, mirroring the dict that Python's
/// `indexer.parse_byte_range` yields. Field order and `null` shape match
/// the Python output so the golden parity test compares as a JSON value.
///
/// `is_sidechain` is serialized as 0 / 1 (matching Python's `int` cast) — the
/// indexer's DB schema stores it as an integer and the Python contract is to
/// emit the same shape from this function.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CleanedMessage {
    pub uuid: String,
    pub session_id: Option<String>,
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub parent_uuid: Option<String>,
    pub is_sidechain: i64,
    pub message_id: Option<String>,
}

/// Parse a JSONL byte range into cleaned message groups.
///
/// Direct port of `threadhop_core.indexer.parse_byte_range`. Applies ADR-003
/// chunk-merging (consecutive assistant lines sharing `message.id` collapse
/// into one row), strips `<system-reminder>` blocks, skips tool-result user
/// lines (those that carry a `toolUseResult`), and abbreviates `tool_use`
/// blocks. Empty rows are dropped.
///
/// Malformed JSON lines and lines with non-object roots / unknown types are
/// silently skipped — one corrupt line should not abort the rest.
///
/// `fallback_session_id` is substituted when a line omits its own `sessionId`
/// (rare, but happens on older transcripts).
pub fn parse_byte_range(
    raw_bytes: &[u8],
    fallback_session_id: Option<&str>,
) -> Vec<CleanedMessage> {
    let text = String::from_utf8_lossy(raw_bytes);
    let mut raw_lines: Vec<&str> = text.split('\n').collect();
    // split() on "line1\nline2\n" → ["line1", "line2", ""] — drop trailing empty.
    if raw_lines.last() == Some(&"") {
        raw_lines.pop();
    }

    let mut groups: Vec<CleanedMessage> = Vec::new();
    let mut current_chunk: Option<CleanedMessage> = None;
    let mut current_chunk_parts: Vec<String> = Vec::new();

    fn flush_chunk(
        current_chunk: &mut Option<CleanedMessage>,
        current_chunk_parts: &mut Vec<String>,
        groups: &mut Vec<CleanedMessage>,
    ) {
        if let Some(mut row) = current_chunk.take() {
            // Two newlines between chunks — readable in snippets, clean
            // for FTS tokenization. Matches Python's "\n\n".join(...).strip().
            let joined = current_chunk_parts
                .iter()
                .filter(|p| !p.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n");
            row.text = joined.trim().to_string();
            current_chunk_parts.clear();
            if !row.text.is_empty() {
                groups.push(row);
            }
        } else {
            // Nothing to flush; still clear parts to mirror Python's reset.
            current_chunk_parts.clear();
        }
    }

    for raw_line in raw_lines {
        if raw_line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(raw_line) {
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

        if mtype == "user" {
            flush_chunk(&mut current_chunk, &mut current_chunk_parts, &mut groups);

            // Skip tool-output user lines: anything with a non-null
            // `toolUseResult` is the harness writing tool output, not a
            // human message — would dominate FTS otherwise.
            if obj
                .get("toolUseResult")
                .map(|v| !v.is_null())
                .unwrap_or(false)
            {
                continue;
            }

            let raw_text = match obj.get("message").and_then(|m| m.get("content")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(arr)) => {
                    // Python: " ".join(b.get("text","") for b in content
                    //                   if isinstance(b, dict) and b.get("type") == "text")
                    let parts: Vec<String> = arr
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
                        .collect();
                    parts.join(" ")
                }
                _ => continue,
            };

            let cleaned = clean_user_text(&raw_text);
            if cleaned.is_empty() {
                continue;
            }

            let uid = match obj.get("uuid").and_then(|x| x.as_str()) {
                Some(u) => u.to_string(),
                None => continue,
            };
            let sid = obj
                .get("sessionId")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .or_else(|| fallback_session_id.map(|s| s.to_string()));

            groups.push(CleanedMessage {
                uuid: uid,
                session_id: sid,
                role: "user".to_string(),
                text: cleaned,
                timestamp: obj
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                cwd: obj.get("cwd").and_then(|x| x.as_str()).map(String::from),
                parent_uuid: obj
                    .get("parentUuid")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                is_sidechain: if obj
                    .get("isSidechain")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false)
                {
                    1
                } else {
                    0
                },
                message_id: obj
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(|x| x.as_str())
                    .map(String::from),
            });
            continue;
        }

        // --- assistant line ---
        let mid: Option<String> = obj
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let parts = extract_assistant_blocks(obj);

        // ADR-003: continuing the same logical message → accumulate.
        if let (Some(mid_ref), Some(current)) = (mid.as_ref(), current_chunk.as_ref()) {
            if current.message_id.as_ref() == Some(mid_ref) {
                current_chunk_parts.extend(parts);
                continue;
            }
        }

        // Different message.id (or no in-flight buffer) → flush + start fresh.
        flush_chunk(&mut current_chunk, &mut current_chunk_parts, &mut groups);

        let uid = match obj.get("uuid").and_then(|x| x.as_str()) {
            Some(u) => u.to_string(),
            None => continue,
        };
        let sid = obj
            .get("sessionId")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| fallback_session_id.map(|s| s.to_string()));

        current_chunk = Some(CleanedMessage {
            uuid: uid,
            session_id: sid,
            role: "assistant".to_string(),
            text: String::new(), // populated by flush_chunk
            timestamp: obj
                .get("timestamp")
                .and_then(|x| x.as_str())
                .map(String::from),
            cwd: obj.get("cwd").and_then(|x| x.as_str()).map(String::from),
            parent_uuid: obj
                .get("parentUuid")
                .and_then(|x| x.as_str())
                .map(String::from),
            is_sidechain: if obj
                .get("isSidechain")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                1
            } else {
                0
            },
            message_id: mid,
        });
        current_chunk_parts = parts;
    }

    // Flush the last chunk.
    flush_chunk(&mut current_chunk, &mut current_chunk_parts, &mut groups);
    groups
}

/// Extract text + abbreviated tool-call snippets from one assistant JSONL
/// object. `thinking` and other block types are dropped — same as
/// `indexer._extract_assistant_blocks(include_tool_calls=True)`.
fn extract_assistant_blocks(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let content = match obj.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for block in content {
        let bo = match block.as_object() {
            Some(b) => b,
            None => continue,
        };
        match bo.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                let t = strip_system_reminders(
                    bo.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                );
                if !t.is_empty() {
                    out.push(t);
                }
            }
            Some("tool_use") => {
                let name = bo.get("name").and_then(|x| x.as_str()).unwrap_or("Unknown");
                let empty = serde_json::Value::Object(serde_json::Map::new());
                let input = bo.get("input").unwrap_or(&empty);
                out.push(abbreviate_tool_use(name, input));
            }
            _ => {} // thinking, etc. — skipped
        }
    }
    out
}

// --- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_system_reminder_spanning_newlines() {
        let s = "before\n<system-reminder>\nfoo\n</system-reminder>\nafter";
        // Python's strip_system_reminders does .strip() at the end, so the
        // outer whitespace is trimmed but interior whitespace is preserved.
        assert_eq!(strip_system_reminders(s), "before\n\nafter");
    }

    #[test]
    fn clean_user_text_drops_skill_load_banner() {
        let s = "Base directory for this skill: /path/to/skill\n\n# heading\n";
        assert_eq!(clean_user_text(s), "");
    }

    #[test]
    fn clean_user_text_strips_local_command_blocks() {
        let s = "<local-command-stdout>ok</local-command-stdout>actual text";
        assert_eq!(clean_user_text(s), "actual text");
    }

    #[test]
    fn clean_user_text_strips_command_blocks() {
        let s = "<command-name>/foo</command-name>hello";
        assert_eq!(clean_user_text(s), "hello");
    }

    #[test]
    fn abbreviate_tool_use_read_uses_basename() {
        let inp = serde_json::json!({"file_path": "/a/b/c.txt"});
        assert_eq!(abbreviate_tool_use("Read", &inp), "Reading c.txt");
    }

    #[test]
    fn abbreviate_tool_use_read_no_path() {
        let inp = serde_json::json!({});
        assert_eq!(abbreviate_tool_use("Read", &inp), "Reading file");
    }

    #[test]
    fn abbreviate_tool_use_bash_uses_first_word() {
        let inp = serde_json::json!({"command": "git status --short"});
        assert_eq!(abbreviate_tool_use("Bash", &inp), "Running git");
    }

    #[test]
    fn abbreviate_tool_use_unknown_returns_name() {
        let inp = serde_json::json!({});
        assert_eq!(abbreviate_tool_use("Weirdo", &inp), "Weirdo");
    }

    #[test]
    fn parse_byte_range_handles_empty_input() {
        assert!(parse_byte_range(b"", None).is_empty());
        assert!(parse_byte_range(b"\n\n", None).is_empty());
    }

    #[test]
    fn parse_byte_range_skips_malformed_json_lines() {
        let raw = b"not json\n{\"type\":\"summary\",\"uuid\":\"x\"}\n";
        assert!(parse_byte_range(raw, None).is_empty());
    }

    #[test]
    fn parse_byte_range_merges_assistant_chunks_by_message_id() {
        // Two assistant lines sharing message.id "m1" → one row, text concatenated.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","uuid":"a2","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"world"}]}}
"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1, "ADR-003 merge: same message.id → one row");
        let row = &got[0];
        assert_eq!(row.uuid, "a1", "first chunk's uuid wins");
        assert_eq!(row.message_id.as_deref(), Some("m1"));
        assert_eq!(row.text, "hello\n\nworld");
    }

    #[test]
    fn parse_byte_range_splits_on_different_message_id() {
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","uuid":"a2","sessionId":"s1","message":{"id":"m2","content":[{"type":"text","text":"world"}]}}
"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "hello");
        assert_eq!(got[1].text, "world");
    }

    #[test]
    fn parse_byte_range_skips_user_lines_with_tool_use_result() {
        let raw = br#"{"type":"user","uuid":"u1","sessionId":"s1","toolUseResult":{"output":"ok"},"message":{"content":"should not appear"}}
{"type":"user","uuid":"u2","sessionId":"s1","message":{"content":"keep me"}}
"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].uuid, "u2");
        assert_eq!(got[0].text, "keep me");
    }

    #[test]
    fn parse_byte_range_applies_fallback_session_id() {
        let raw = br#"{"type":"user","uuid":"u1","message":{"content":"hi"}}"#;
        let got = parse_byte_range(raw, Some("fallback-sid"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id.as_deref(), Some("fallback-sid"));
    }

    #[test]
    fn parse_byte_range_drops_assistant_with_only_thinking_blocks() {
        // A row whose blocks are all `thinking` → empty text → dropped.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        let got = parse_byte_range(raw, None);
        assert!(got.is_empty(), "empty-text rows dropped");
    }

    #[test]
    fn parse_byte_range_skips_lines_without_uuid() {
        let raw = br#"{"type":"user","sessionId":"s1","message":{"content":"hi"}}"#;
        assert!(parse_byte_range(raw, None).is_empty());
    }

    #[test]
    fn parse_byte_range_matches_python_golden() {
        // ADR-003 parity check. Fixture captured from the real Python
        // `parse_byte_range` against a vendored threadhop session — see
        // tests/fixtures/sample_session.jsonl and sample_session_expected.json.
        let raw = include_bytes!("../tests/fixtures/sample_session.jsonl");
        let expected_str = include_str!("../tests/fixtures/sample_session_expected.json");
        let got = parse_byte_range(raw, Some("test"));
        let got_json = serde_json::to_value(&got).unwrap();
        let expected_json: serde_json::Value = serde_json::from_str(expected_str).unwrap();
        assert_eq!(
            got_json, expected_json,
            "Rust parse_byte_range diverged from Python golden"
        );
    }
}
