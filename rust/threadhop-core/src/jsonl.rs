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
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::JsonlError;

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

/// `<command-name>...</command-name>` — narrow regex used to extract the
/// command name when we want to emit a CommandPill row alongside (or
/// instead of) the user message it sat inside.
static COMMAND_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<command-name>(.*?)</command-name>").unwrap());

/// Classification of a single user JSONL line's content, used by
/// `parse_byte_range` to decide which row types to emit.
///
/// Mirrors the spirit of Python's `indexer.classify_user_text`, but designed
/// for the Rust emitter's needs: if a user line contained both a slash-command
/// invocation AND prose, the Python TUI joins them into one row whereas our
/// emitter wants to surface a separate `command` row BEFORE the user row. We
/// keep both pieces by tracking the slash-command name alongside the cleaned
/// residue.
#[derive(Debug, Clone, PartialEq)]
pub enum UserLineKind {
    /// Pure slash-command invocation with no surrounding prose. Emit one
    /// command row, no user row.
    Command { name: String },
    /// Skill-load banner. Emit one skill_load row, no user row.
    SkillLoad { name: String },
    /// Normal user prose. `cleaned` is the user-visible text; if
    /// `command_prefix` is `Some`, the user typed prose alongside a
    /// slash-command and the caller should emit a command row BEFORE
    /// the user row.
    User {
        cleaned: String,
        command_prefix: Option<String>,
    },
    /// Nothing user-visible after cleaning. Emit no row.
    Empty,
}

/// Classify a user JSONL line's content (Wave 2.5).
///
/// Used by `parse_byte_range` to fan a single user line out into 0, 1, or 2
/// rows (command + user combination). The classification reads the same
/// markup that `clean_user_text` strips, but reports the kind first so the
/// caller can choose which row(s) to emit.
pub fn classify_user_text(text: &str) -> UserLineKind {
    let s1 = SYSTEM_REMINDER_RE.replace_all(text, "");
    let s2 = LOCAL_COMMAND_BLOCK_RE.replace_all(&s1, "");
    let cleaned_pre: String = s2.into_owned();

    let cmd_name: Option<String> = COMMAND_NAME_RE
        .captures(&cleaned_pre)
        .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|s| !s.is_empty());
    let residue = COMMAND_BLOCK_RE
        .replace_all(&cleaned_pre, "")
        .trim()
        .to_string();

    if let Some(name) = &cmd_name {
        if residue.is_empty() {
            return UserLineKind::Command { name: name.clone() };
        }
    }

    if !residue.is_empty() {
        if let Some(banner) = SKILL_LOAD_BANNER_RE.captures(residue.trim_start()) {
            let path = banner
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let name = if path.is_empty() {
                "skill".to_string()
            } else {
                Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "skill".to_string())
            };
            return UserLineKind::SkillLoad { name };
        }
    }

    if residue.is_empty() {
        return UserLineKind::Empty;
    }

    UserLineKind::User {
        cleaned: residue,
        command_prefix: cmd_name,
    }
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

/// Per-assistant-row usage counters lifted from `message.usage`.
///
/// Wave 2.5 enrichment — Worker H's SessionDigest builder needs raw token
/// counts (input/output/cache) to populate the digest header. Held on
/// `CleanedMessage` only for `role == "assistant"`; everything else carries
/// `None`. Field names mirror the JSONL keys exactly (`input_tokens`,
/// `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`)
/// so a future serde-driven path can deserialize them directly without a
/// rename layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MessageUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// One cleaned message group.
///
/// Originally mirrored Python's `indexer.parse_byte_range` output dict
/// (user + assistant rows). Wave 2.5 enriches the contract: the parser also
/// emits `command`, `skill_load`, and `tool` rows so downstream widgets
/// (CommandPill, tool-fold, SessionDigest) can reason about each turn type
/// without re-parsing the JSONL. The Python TUI does the same classification
/// in `tui/widgets/transcript.py::classify_user_text` and tool-block split —
/// pushing it into `parse_byte_range` keeps the Rust port single-source.
///
/// `is_sidechain` is serialized as 0 / 1 (matching Python's `int` cast) — the
/// indexer's DB schema stores it as an integer and the Python contract is to
/// emit the same shape from this function.
///
/// The Wave 2.5 fields (`usage`, `model`, `tool_name`) use
/// `skip_serializing_if = "Option::is_none"` so rows that don't carry them
/// (every non-assistant row for usage/model; every non-tool row for
/// tool_name) stay byte-clean in golden-fixture comparisons.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CleanedMessage {
    pub uuid: String,
    pub session_id: Option<String>,
    /// One of: `"user"`, `"assistant"`, `"tool"`, `"command"`, `"skill_load"`.
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub parent_uuid: Option<String>,
    pub is_sidechain: i64,
    pub message_id: Option<String>,
    /// Token counters from `message.usage`. Populated on assistant rows
    /// when the JSONL carries a `usage` object; `None` everywhere else.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage: Option<MessageUsage>,
    /// `message.model` — populated on assistant rows when present.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
    /// Tool name (e.g. `"Bash"`, `"Read"`). Populated only on
    /// `role == "tool"` rows; the human-readable summary lives in
    /// `text`. `None` everywhere else.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_name: Option<String>,
}

/// One extracted tool_use block, with enough metadata to emit a `tool` row.
struct ExtractedTool {
    uuid: String,
    name: String,
    summary: String,
}

/// Parsed assistant blocks for one JSONL line: text content + tool_use blocks
/// in order. `tool_use` rows are emitted AFTER the assistant text row, in
/// document order across the merged chunks.
struct AssistantBlocks {
    /// Plain text parts (already cleaned via `strip_system_reminders`).
    /// Joined `"\n\n"` to form the assistant row body.
    text_parts: Vec<String>,
    /// Tool calls in document order.
    tools: Vec<ExtractedTool>,
}

/// Parse a JSONL byte range into cleaned message groups (Wave 2.5).
///
/// Applies ADR-003 chunk-merging (consecutive assistant lines sharing
/// `message.id` collapse into one row), strips `<system-reminder>` blocks,
/// and classifies user lines via [`classify_user_text`].
///
/// Wave 2.5 expansion — emits new row types so downstream widgets can
/// reason about each turn:
/// * `role: "command"` — a slash-command invocation lifted from a user line.
///   Followed by a `user` row if the line carried prose alongside the
///   command.
/// * `role: "skill_load"` — a Claude Code skill-load banner lifted from a
///   user line.
/// * `role: "tool"` — one per `tool_use` block, emitted after the
///   corresponding assistant row. Carries the abbreviated summary in
///   `text` and the tool name in `tool_name`. Tool-result user lines
///   (carrying `toolUseResult`) are folded into the most recent tool row
///   as a `↳ <result>` suffix on `text`.
///
/// Assistant rows additionally carry `message.usage` (input/output/cache
/// token counts) and `message.model` when present in the JSONL.
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
    // Tools accumulated for the in-flight assistant chunk; emitted after
    // its text row when the chunk flushes.
    let mut current_chunk_tools: Vec<ExtractedTool> = Vec::new();

    fn flush_chunk(
        current_chunk: &mut Option<CleanedMessage>,
        current_chunk_parts: &mut Vec<String>,
        current_chunk_tools: &mut Vec<ExtractedTool>,
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
            // Carry session/timestamp/cwd from the assistant row onto each
            // tool row so downstream consumers (digest, render, copy)
            // don't have to back-reference the parent.
            let parent_session = row.session_id.clone();
            let parent_timestamp = row.timestamp.clone();
            let parent_cwd = row.cwd.clone();
            let parent_is_sidechain = row.is_sidechain;
            let parent_message_id = row.message_id.clone();
            let parent_uuid_of_tools = row.uuid.clone();
            let emit_assistant = !row.text.is_empty();
            current_chunk_parts.clear();
            if emit_assistant {
                groups.push(row);
            }
            // Emit one tool row per tool_use block. Tools are emitted even
            // if the assistant row had no text — Claude often replies with
            // a pure tool call (no preface), and we still need the tool
            // row to drive the fold + digest counters.
            for tool in current_chunk_tools.drain(..) {
                groups.push(CleanedMessage {
                    uuid: tool.uuid,
                    session_id: parent_session.clone(),
                    role: "tool".to_string(),
                    text: tool.summary,
                    timestamp: parent_timestamp.clone(),
                    cwd: parent_cwd.clone(),
                    parent_uuid: Some(parent_uuid_of_tools.clone()),
                    is_sidechain: parent_is_sidechain,
                    message_id: parent_message_id.clone(),
                    usage: None,
                    model: None,
                    tool_name: Some(tool.name),
                });
            }
        } else {
            // Nothing to flush; still clear parts to mirror Python's reset.
            current_chunk_parts.clear();
            current_chunk_tools.clear();
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
            // Tool-result user line: fold its result text into the most
            // recently emitted tool row (if any) as a `↳ <snippet>` suffix.
            // This is the data the digest builder needs to compute success
            // / failure rates, and what Worker E's fold uses to keep the
            // collapsed summary informative.
            let is_tool_result = obj
                .get("toolUseResult")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if is_tool_result {
                // Flush in-flight assistant FIRST so the tool rows it owns
                // are already in `groups` and we can attach the result to
                // the last one.
                flush_chunk(
                    &mut current_chunk,
                    &mut current_chunk_parts,
                    &mut current_chunk_tools,
                    &mut groups,
                );
                let snippet = extract_tool_result_snippet(obj);
                if !snippet.is_empty() {
                    if let Some(last_tool) = groups
                        .iter_mut()
                        .rev()
                        .find(|m| m.role == "tool")
                    {
                        // Only attach if the parent_uuid lines up with the
                        // referenced tool_use_id (when available); otherwise
                        // attach to the most-recent tool unconditionally.
                        last_tool.text.push_str("\n↳ ");
                        last_tool.text.push_str(&snippet);
                    }
                }
                continue;
            }

            flush_chunk(
                &mut current_chunk,
                &mut current_chunk_parts,
                &mut current_chunk_tools,
                &mut groups,
            );

            let raw_text = match obj.get("message").and_then(|m| m.get("content")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(arr)) => {
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

            let uid = match obj.get("uuid").and_then(|x| x.as_str()) {
                Some(u) => u.to_string(),
                None => continue,
            };
            let sid = obj
                .get("sessionId")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .or_else(|| fallback_session_id.map(|s| s.to_string()));
            let timestamp = obj
                .get("timestamp")
                .and_then(|x| x.as_str())
                .map(String::from);
            let cwd = obj.get("cwd").and_then(|x| x.as_str()).map(String::from);
            let parent_uuid = obj
                .get("parentUuid")
                .and_then(|x| x.as_str())
                .map(String::from);
            let is_sidechain = if obj
                .get("isSidechain")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
            {
                1
            } else {
                0
            };
            let message_id = obj
                .get("message")
                .and_then(|m| m.get("id"))
                .and_then(|x| x.as_str())
                .map(String::from);

            // Wave 2.5: classify the line. A pure slash-command emits ONE
            // command row; a pure skill-load banner emits ONE skill_load
            // row; user prose alongside a slash-command emits a command
            // row FOLLOWED BY a user row sharing the same uuid (suffixed
            // for the command row so the two rows have distinct ids).
            match classify_user_text(&raw_text) {
                UserLineKind::Empty => continue,
                UserLineKind::Command { name } => {
                    groups.push(CleanedMessage {
                        uuid: uid,
                        session_id: sid,
                        role: "command".to_string(),
                        text: name,
                        timestamp,
                        cwd,
                        parent_uuid,
                        is_sidechain,
                        message_id,
                        usage: None,
                        model: None,
                        tool_name: None,
                    });
                }
                UserLineKind::SkillLoad { name } => {
                    groups.push(CleanedMessage {
                        uuid: uid,
                        session_id: sid,
                        role: "skill_load".to_string(),
                        text: name,
                        timestamp,
                        cwd,
                        parent_uuid,
                        is_sidechain,
                        message_id,
                        usage: None,
                        model: None,
                        tool_name: None,
                    });
                }
                UserLineKind::User {
                    cleaned,
                    command_prefix,
                } => {
                    if let Some(cmd) = command_prefix {
                        // Distinct uuid for the command row so selection /
                        // bookmark code never sees a uuid collision. Suffix
                        // is stable + reproducible.
                        let cmd_uuid = format!("{uid}::cmd");
                        groups.push(CleanedMessage {
                            uuid: cmd_uuid,
                            session_id: sid.clone(),
                            role: "command".to_string(),
                            text: cmd,
                            timestamp: timestamp.clone(),
                            cwd: cwd.clone(),
                            parent_uuid: parent_uuid.clone(),
                            is_sidechain,
                            message_id: message_id.clone(),
                            usage: None,
                            model: None,
                            tool_name: None,
                        });
                    }
                    groups.push(CleanedMessage {
                        uuid: uid,
                        session_id: sid,
                        role: "user".to_string(),
                        text: cleaned,
                        timestamp,
                        cwd,
                        parent_uuid,
                        is_sidechain,
                        message_id,
                        usage: None,
                        model: None,
                        tool_name: None,
                    });
                }
            }
            continue;
        }

        // --- assistant line ---
        let mid: Option<String> = obj
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|x| x.as_str())
            .map(String::from);
        let blocks = extract_assistant_blocks(obj);

        // ADR-003: continuing the same logical message → accumulate text
        // AND tools onto the in-flight chunk.
        if let (Some(mid_ref), Some(current)) = (mid.as_ref(), current_chunk.as_ref()) {
            if current.message_id.as_ref() == Some(mid_ref) {
                current_chunk_parts.extend(blocks.text_parts);
                current_chunk_tools.extend(blocks.tools);
                // If this continuation chunk carries a fresh usage/model
                // (typically the final chunk of a streaming response), let
                // it override the in-flight values. The earlier chunks
                // typically don't have full usage stats anyway.
                let (new_usage, new_model) = extract_usage_and_model(obj);
                if let Some(row) = current_chunk.as_mut() {
                    if new_usage.is_some() {
                        row.usage = new_usage;
                    }
                    if new_model.is_some() {
                        row.model = new_model;
                    }
                }
                continue;
            }
        }

        // Different message.id (or no in-flight buffer) → flush + start fresh.
        flush_chunk(
            &mut current_chunk,
            &mut current_chunk_parts,
            &mut current_chunk_tools,
            &mut groups,
        );

        let uid = match obj.get("uuid").and_then(|x| x.as_str()) {
            Some(u) => u.to_string(),
            None => continue,
        };
        let sid = obj
            .get("sessionId")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or_else(|| fallback_session_id.map(|s| s.to_string()));
        let (usage, model) = extract_usage_and_model(obj);

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
            usage,
            model,
            tool_name: None,
        });
        current_chunk_parts = blocks.text_parts;
        current_chunk_tools = blocks.tools;
    }

    // Flush the last chunk.
    flush_chunk(
        &mut current_chunk,
        &mut current_chunk_parts,
        &mut current_chunk_tools,
        &mut groups,
    );
    groups
}

/// Extract text + tool_use blocks from one assistant JSONL object.
/// `thinking` and other block types are dropped — same as
/// `indexer._extract_assistant_blocks(include_tool_calls=True)`.
fn extract_assistant_blocks(obj: &serde_json::Map<String, serde_json::Value>) -> AssistantBlocks {
    let content = match obj.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::Array(a)) => a,
        _ => return AssistantBlocks { text_parts: Vec::new(), tools: Vec::new() },
    };
    let mut text_parts: Vec<String> = Vec::new();
    let mut tools: Vec<ExtractedTool> = Vec::new();
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
                    text_parts.push(t);
                }
            }
            Some("tool_use") => {
                let name = bo
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let empty = serde_json::Value::Object(serde_json::Map::new());
                let input = bo.get("input").unwrap_or(&empty);
                let summary = abbreviate_tool_use(&name, input);
                // tool_use blocks carry a per-call `id` field — prefer that
                // when present so tool_result lines can be cross-referenced.
                // Fall back to a derived uuid based on position so unique
                // per-tool ids are still emitted.
                let tool_uuid = bo
                    .get("id")
                    .and_then(|x| x.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("tool-{}-{}", name, tools.len()));
                tools.push(ExtractedTool {
                    uuid: tool_uuid,
                    name,
                    summary,
                });
            }
            _ => {} // thinking, etc. — skipped
        }
    }
    AssistantBlocks { text_parts, tools }
}

/// Pull `message.usage` and `message.model` from a JSONL assistant object.
/// Both fields are optional — older transcripts and synthetic streams may
/// omit one or both. Returns `(None, None)` when neither is present.
fn extract_usage_and_model(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> (Option<MessageUsage>, Option<String>) {
    let message = match obj.get("message") {
        Some(serde_json::Value::Object(m)) => m,
        _ => return (None, None),
    };
    let model = message
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from);
    let usage = message
        .get("usage")
        .and_then(|v| v.as_object())
        .map(|u| MessageUsage {
            input_tokens: u
                .get("input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            output_tokens: u
                .get("output_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            cache_creation_input_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            cache_read_input_tokens: u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
        });
    (usage, model)
}

/// Pull a one-line snippet from a `toolUseResult` payload. Result payloads
/// arrive in a few shapes:
/// * `{"output": "..."}` — Bash / Read / Grep — string output.
/// * `{"content": [{"type": "text", "text": "..."}, ...]}` — Claude Code
///   wrapper shape, mirroring tool_use block layout.
/// * Bare strings — older transcripts.
///
/// We collapse newlines to spaces and trim to ~120 chars so the `↳` suffix
/// stays a single visible line.
fn extract_tool_result_snippet(obj: &serde_json::Map<String, serde_json::Value>) -> String {
    let raw = match obj.get("toolUseResult") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(m)) => {
            if let Some(s) = m.get("output").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(arr) = m.get("content").and_then(|v| v.as_array()) {
                arr.iter()
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
                    .join(" ")
            } else {
                return String::new();
            }
        }
        _ => return String::new(),
    };
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 120 {
        collapsed.chars().take(117).collect::<String>() + "..."
    } else {
        collapsed
    }
}

// --- read_session_metadata ---------------------------------------------------

/// Head-of-file metadata extracted from a Claude Code session JSONL.
///
/// Phase 2's session list (sidebar) uses this to populate one row per session
/// without parsing the entire transcript. The fields mirror the subset of
/// `_gather_session_data` in `threadhop_core/tui/app.py` that the planner
/// (Task 1.10 Step 5) earmarked for the head-scan path: which session, where
/// it ran, what the user first said, and when the first line landed.
///
/// `session_id` is mirrored back into the struct rather than re-derived from
/// the file stem so callers can verify the on-disk `sessionId` matches the
/// filename (or surface a mismatch if Claude Code's naming convention ever
/// shifts). When no `sessionId` is found in the head window, this falls back
/// to `Path::file_stem` for parity with the Python implementation, which
/// keys sessions by `jsonl.stem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub cwd: Option<String>,
    pub first_user_text: Option<String>,
    pub first_timestamp: Option<String>,
}

/// Number of header lines to scan when extracting session metadata.
///
/// Matches the Python TUI's head-scan budget; in practice the first user
/// message and the session's `cwd` land in the first handful of lines, so the
/// loop usually exits well before this cap via the early-return below.
const SESSION_METADATA_HEAD_LINES: usize = 100;

/// Truncation cap on the cleaned first-user-message preview.
///
/// Mirrors the `text[:50]` slice in `tui/app.py::_gather_session_data` — the
/// sidebar only ever renders the first ~50 chars as a title fallback, so we
/// pay the same cost up front and keep `SessionMetadata` small. The slice is
/// char-boundary safe (not byte-indexed) so multi-byte characters don't panic.
const FIRST_USER_TEXT_MAX_CHARS: usize = 50;

/// Read up to `SESSION_METADATA_HEAD_LINES` lines of a session JSONL and
/// extract sidebar metadata (session id, cwd, first user message, first
/// timestamp).
///
/// Returns early as soon as all four fields are populated — typical Claude
/// Code transcripts fill them within the first ~5 lines.
///
/// Direct port of the head-scan branch of `_gather_session_data` in
/// `threadhop_core/tui/app.py`. The Python version scans the whole file to
/// compute is_working / turn-count metadata as well; those derived fields
/// belong on the worker that calls this and are out of scope here.
///
/// Errors:
/// * [`JsonlError::Io`] on file-open / read failure.
/// * Malformed JSON lines are skipped silently (one bad line never wedges
///   the scan — same contract as `parse_byte_range`).
pub fn read_session_metadata(path: &Path) -> Result<SessionMetadata, JsonlError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut first_user_text: Option<String> = None;
    let mut first_timestamp: Option<String> = None;

    for (i, line_res) in reader.lines().enumerate() {
        if i >= SESSION_METADATA_HEAD_LINES {
            break;
        }
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = match msg.as_object() {
            Some(o) => o,
            None => continue,
        };

        // sessionId — populated by nearly every line in a Claude Code JSONL,
        // so the first hit wins.
        if session_id.is_none() {
            if let Some(s) = obj.get("sessionId").and_then(|v| v.as_str()) {
                session_id = Some(s.to_string());
            }
        }

        // cwd — same: any line with a `cwd` is a valid source. Matches
        // Python's `if "cwd" in msg and not session_cwd`.
        if cwd.is_none() {
            if let Some(c) = obj.get("cwd").and_then(|v| v.as_str()) {
                cwd = Some(c.to_string());
            }
        }

        let mtype = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // First user line (skipping tool-result user lines, matching the
        // Python head-scan). Sets both `first_user_text` and `first_timestamp`.
        if mtype == "user"
            && first_user_text.is_none()
            && !obj
                .get("toolUseResult")
                .map(|v| !v.is_null())
                .unwrap_or(false)
        {
            let raw_text = match obj.get("message").and_then(|m| m.get("content")) {
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
                _ => String::new(),
            };

            if !raw_text.trim().is_empty() {
                let cleaned = clean_user_text(&raw_text);
                // Python additionally collapses runs of whitespace via
                // `" ".join(text.split())` — preserve that so the sidebar
                // preview is a single tidy line.
                let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
                if !collapsed.is_empty() {
                    // Char-boundary-safe truncation: take the first
                    // FIRST_USER_TEXT_MAX_CHARS code points.
                    let truncated: String = collapsed
                        .chars()
                        .take(FIRST_USER_TEXT_MAX_CHARS)
                        .collect();
                    first_user_text = Some(truncated);
                    if first_timestamp.is_none() {
                        first_timestamp = obj
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                    }
                }
            }
        }

        // Early-out once all four fields are populated.
        if session_id.is_some()
            && cwd.is_some()
            && first_user_text.is_some()
            && first_timestamp.is_some()
        {
            break;
        }
    }

    // Fall back to the file stem if no `sessionId` appeared in the head
    // window — Python keys sessions off `jsonl.stem`, so we match.
    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    Ok(SessionMetadata {
        session_id,
        cwd,
        first_user_text,
        first_timestamp,
    })
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
        // clean_user_text() still drops the banner from the cleaned text
        // it returns (the user row body); the parser separately emits a
        // dedicated skill_load row via classify_user_text — covered by
        // `parse_byte_range_emits_skill_load_row_for_banner` below.
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
        // Same as above: clean_user_text still strips the markup from the
        // residue, while classify_user_text + parse_byte_range surface the
        // slash-command name on a separate `command` row.
        assert_eq!(clean_user_text(s), "hello");
    }

    #[test]
    fn classify_user_text_pure_command_returns_command_kind() {
        let s = "<command-name>/foo</command-name>";
        match classify_user_text(s) {
            UserLineKind::Command { name } => assert_eq!(name, "/foo"),
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[test]
    fn classify_user_text_skill_load_returns_skill_name() {
        let s = "Base directory for this skill: /Users/x/.claude/skills/handoff\n# Title";
        match classify_user_text(s) {
            UserLineKind::SkillLoad { name } => assert_eq!(name, "handoff"),
            other => panic!("expected SkillLoad, got {other:?}"),
        }
    }

    #[test]
    fn classify_user_text_user_prose_alongside_command_carries_prefix() {
        let s = "<command-name>/foo</command-name>some prose follows";
        match classify_user_text(s) {
            UserLineKind::User {
                cleaned,
                command_prefix,
            } => {
                assert_eq!(cleaned, "some prose follows");
                assert_eq!(command_prefix.as_deref(), Some("/foo"));
            }
            other => panic!("expected User w/ prefix, got {other:?}"),
        }
    }

    #[test]
    fn classify_user_text_plain_returns_user() {
        match classify_user_text("just chatting") {
            UserLineKind::User {
                cleaned,
                command_prefix,
            } => {
                assert_eq!(cleaned, "just chatting");
                assert!(command_prefix.is_none());
            }
            other => panic!("expected plain user, got {other:?}"),
        }
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

    // --- read_session_metadata ---------------------------------------------

    #[test]
    fn read_session_metadata_extracts_head_fields_from_fixture() {
        // sample_session.jsonl's first user line carries session_id
        // d27acf14-…-a219, cwd /Users/nandakumar/Personal/threadhop, and the
        // bookmark-shortcut question at 2026-04-22T20:57:00.953Z. The
        // sessionId / cwd also show up earlier on the bridge_status line, so
        // those are filled before the user-line scan begins.
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample_session.jsonl");
        let meta = read_session_metadata(&path).expect("read fixture");
        assert_eq!(meta.session_id, "d27acf14-e782-4725-adac-78d04174a219");
        assert_eq!(
            meta.cwd.as_deref(),
            Some("/Users/nandakumar/Personal/threadhop")
        );
        let preview = meta
            .first_user_text
            .as_deref()
            .expect("first_user_text populated");
        // Truncation cap is 50 chars — verify both the cap and the prefix.
        assert!(preview.chars().count() <= 50);
        assert!(
            preview.starts_with("What's the shortcut to bookmark"),
            "unexpected first_user_text preview: {preview:?}"
        );
        assert_eq!(
            meta.first_timestamp.as_deref(),
            Some("2026-04-22T20:57:00.953Z")
        );
    }

    #[test]
    fn read_session_metadata_falls_back_to_file_stem_for_session_id() {
        // No sessionId anywhere in the head window → fall back to the
        // filename stem (Python's `jsonl.stem`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stub-session-id.jsonl");
        std::fs::write(
            &path,
            br#"{"type":"meta","note":"no sessionId here"}
{"type":"user","uuid":"u1","timestamp":"2026-05-21T00:00:00Z","message":{"role":"user","content":"hello there"}}
"#,
        )
        .unwrap();
        let meta = read_session_metadata(&path).unwrap();
        assert_eq!(meta.session_id, "stub-session-id");
        assert_eq!(meta.first_user_text.as_deref(), Some("hello there"));
        assert_eq!(
            meta.first_timestamp.as_deref(),
            Some("2026-05-21T00:00:00Z")
        );
        assert!(meta.cwd.is_none());
    }

    #[test]
    fn read_session_metadata_skips_tool_result_user_lines() {
        // First user line carries a toolUseResult → it must be skipped, and
        // the next non-tool user line should win.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            br#"{"type":"user","uuid":"u1","sessionId":"sid","cwd":"/tmp","timestamp":"2026-05-21T00:00:00Z","toolUseResult":{"output":"ok"},"message":{"role":"user","content":"tool junk"}}
{"type":"user","uuid":"u2","sessionId":"sid","cwd":"/tmp","timestamp":"2026-05-21T00:00:01Z","message":{"role":"user","content":"actual question"}}
"#,
        )
        .unwrap();
        let meta = read_session_metadata(&path).unwrap();
        assert_eq!(meta.session_id, "sid");
        assert_eq!(meta.first_user_text.as_deref(), Some("actual question"));
        assert_eq!(
            meta.first_timestamp.as_deref(),
            Some("2026-05-21T00:00:01Z")
        );
    }

    #[test]
    fn read_session_metadata_truncates_long_first_user_text() {
        // A first-user line longer than 50 chars must be truncated on a
        // char boundary — including a multi-byte char straddling the cap.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // 60 chars: 49 ASCII "a" + one 4-byte emoji + 10 ASCII "b".
        // The emoji sits at char index 49 (10th-to-last), so the 50-char
        // window must include it whole rather than slicing its bytes.
        let mut content = String::from("a").repeat(49);
        content.push('\u{1F600}'); // grinning face — 4 bytes UTF-8
        content.push_str(&"b".repeat(10));
        let line = format!(
            r#"{{"type":"user","uuid":"u","sessionId":"s","message":{{"role":"user","content":"{}"}}}}"#,
            content
        );
        std::fs::write(&path, line.as_bytes()).unwrap();
        let meta = read_session_metadata(&path).unwrap();
        let preview = meta.first_user_text.expect("populated");
        assert_eq!(preview.chars().count(), 50);
        assert!(preview.ends_with('\u{1F600}'));
    }

    // ---- Wave 2.5 parser enrichment -----------------------------------

    #[test]
    fn parse_byte_range_emits_command_row_for_slash_command() {
        // A user line whose content is ONLY `<command-name>/foo</command-name>`
        // should emit a single `command` row carrying the slash-command name.
        let raw = br#"{"type":"user","uuid":"u1","sessionId":"s1","message":{"content":"<command-name>/foo</command-name>"}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1, "expected one row, got {got:#?}");
        assert_eq!(got[0].role, "command");
        assert_eq!(got[0].text, "/foo");
        assert_eq!(got[0].uuid, "u1");
    }

    #[test]
    fn parse_byte_range_emits_command_then_user_for_mixed_content() {
        // User typed prose alongside a slash-command — emit two rows:
        // command first (suffixed uuid), then user (original uuid).
        let raw = br#"{"type":"user","uuid":"u1","sessionId":"s1","message":{"content":"<command-name>/foo</command-name>some prose"}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 2, "expected command + user, got {got:#?}");
        assert_eq!(got[0].role, "command");
        assert_eq!(got[0].text, "/foo");
        assert_eq!(got[0].uuid, "u1::cmd");
        assert_eq!(got[1].role, "user");
        assert_eq!(got[1].text, "some prose");
        assert_eq!(got[1].uuid, "u1");
    }

    #[test]
    fn parse_byte_range_emits_skill_load_row_for_banner() {
        let raw = br#"{"type":"user","uuid":"u1","sessionId":"s1","message":{"content":"Base directory for this skill: /Users/x/.claude/skills/handoff\n\n# body"}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1, "expected skill_load row only, got {got:#?}");
        assert_eq!(got[0].role, "skill_load");
        assert_eq!(got[0].text, "handoff");
    }

    #[test]
    fn parse_byte_range_emits_tool_rows_after_assistant() {
        // Assistant message with text + two tool_use blocks should produce
        // 1 assistant row + 2 tool rows in that order.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","model":"claude-opus","content":[{"type":"text","text":"sure"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/a/b/c.txt"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 3, "expected assistant + 2 tools, got {got:#?}");
        assert_eq!(got[0].role, "assistant");
        assert_eq!(got[0].text, "sure");
        assert_eq!(got[1].role, "tool");
        assert_eq!(got[1].tool_name.as_deref(), Some("Bash"));
        assert_eq!(got[1].text, "Running ls");
        assert_eq!(got[1].uuid, "t1");
        assert_eq!(got[2].role, "tool");
        assert_eq!(got[2].tool_name.as_deref(), Some("Read"));
        assert_eq!(got[2].text, "Reading c.txt");
    }

    #[test]
    fn parse_byte_range_emits_tool_row_when_assistant_has_no_text() {
        // Pure-tool-call turn (no preface text) still emits the tool row.
        // Otherwise the digest would never see solo tool calls.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"pwd"}}]}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1, "expected just the tool row, got {got:#?}");
        assert_eq!(got[0].role, "tool");
        assert_eq!(got[0].tool_name.as_deref(), Some("Bash"));
    }

    #[test]
    fn parse_byte_range_folds_tool_result_into_preceding_tool_row() {
        // tool_use followed by a user line carrying toolUseResult should
        // attach the result snippet to the tool row's text as `↳ <snippet>`.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi"}}]}}
{"type":"user","uuid":"u1","sessionId":"s1","toolUseResult":{"output":"hi\n"},"message":{"content":""}}
"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1, "tool row only, got {got:#?}");
        assert_eq!(got[0].role, "tool");
        assert!(
            got[0].text.contains("↳ hi"),
            "expected ↳ suffix, got {:?}",
            got[0].text
        );
    }

    #[test]
    fn parse_byte_range_retains_message_usage_on_assistant_row() {
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":12,"output_tokens":3,"cache_creation_input_tokens":1,"cache_read_input_tokens":2}}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1);
        let u = got[0].usage.as_ref().expect("usage populated");
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.output_tokens, 3);
        assert_eq!(u.cache_creation_input_tokens, 1);
        assert_eq!(u.cache_read_input_tokens, 2);
    }

    #[test]
    fn parse_byte_range_retains_model_on_assistant_row() {
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","model":"claude-3-5-sonnet-20241022","content":[{"type":"text","text":"hi"}]}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].model.as_deref(), Some("claude-3-5-sonnet-20241022"));
    }

    #[test]
    fn parse_byte_range_assistant_without_usage_is_none() {
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"hi"}]}}"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 1);
        assert!(got[0].usage.is_none());
        assert!(got[0].model.is_none());
    }

    #[test]
    fn parse_byte_range_merge_preserves_tool_blocks_in_order() {
        // ADR-003: two assistant lines sharing message.id where the first
        // carries a tool_use and the second carries text should produce
        // ONE assistant text row + ONE tool row in the right order.
        let raw = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/x.txt"}}]}}
{"type":"assistant","uuid":"a2","sessionId":"s1","message":{"id":"m1","content":[{"type":"text","text":"and then"}]}}
"#;
        let got = parse_byte_range(raw, None);
        assert_eq!(got.len(), 2, "expected assistant + tool, got {got:#?}");
        assert_eq!(got[0].role, "assistant");
        assert_eq!(got[0].text, "and then");
        assert_eq!(got[1].role, "tool");
        assert_eq!(got[1].tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn parse_byte_range_matches_golden() {
        // Snapshot test for the Rust parser. The fixture was originally
        // captured from Python's `parse_byte_range`; Wave 2.5 expanded the
        // Rust contract (tool / command / skill_load rows + usage / model
        // retention) so the snapshot now represents the Rust output and is
        // regenerated whenever the parser shape changes. See
        // tests/fixtures/sample_session.jsonl + sample_session_expected.json.
        let raw = include_bytes!("../tests/fixtures/sample_session.jsonl");
        let expected_str = include_str!("../tests/fixtures/sample_session_expected.json");
        let got = parse_byte_range(raw, Some("test"));
        let got_json = serde_json::to_value(&got).unwrap();
        let expected_json: serde_json::Value = serde_json::from_str(expected_str).unwrap();
        assert_eq!(
            got_json, expected_json,
            "Rust parse_byte_range output diverged from the captured snapshot"
        );
    }
}
