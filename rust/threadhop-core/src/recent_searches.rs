//! Atomic JSON read/write of the `recent_searches` key in
//! `~/.config/threadhop/config.json`.
//!
//! Mirrors `threadhop_core/storage/recent_searches.py`:
//! - MRU-ordered list of unique non-empty strings, capped at
//!   [`MAX_RECENT_SEARCHES`].
//! - Reads dedupe + strip blanks + drop non-string entries.
//! - Writes preserve any unknown keys in `config.json` (theme, sidebar_width,
//!   …) by parsing into a generic [`serde_json::Value`] and only mutating
//!   the `recent_searches` field.
//! - Writes are atomic on Unix: serialize to `config.json.tmp` next to the
//!   target file, then `rename` over the destination. An in-process
//!   [`Mutex`] serializes the read–modify–write cycle so concurrent callers
//!   inside the same process do not race; cross-process atomicity rests on
//!   `rename(2)`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::ConfigError;

/// Maximum number of MRU entries persisted. Mirrors the Python constant in
/// `threadhop_core/storage/recent_searches.py`.
pub const MAX_RECENT_SEARCHES: usize = 8;

/// Serializes the read-modify-write cycle inside a single process. The OS
/// `rename` provides cross-process atomicity; this mutex is just to keep
/// two threads in the same process from clobbering each other.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Read the MRU recent-search list from the default config path
/// (`~/.config/threadhop/config.json`).
pub fn get_recent_searches() -> Result<Vec<String>, ConfigError> {
    get_recent_searches_from(&crate::paths::config_path())
}

/// Read + clean the MRU recent-search list from `path`.
///
/// Missing file, missing key, non-array values, and non-string entries all
/// degrade silently to an empty result — matching the Python helper's
/// permissive contract.
pub fn get_recent_searches_from(path: &Path) -> Result<Vec<String>, ConfigError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(extract_cleaned(&raw))
}

/// Promote `query` to the front of the MRU list and persist.
///
/// - Trims `query`; empty queries are a no-op that still returns the current
///   list.
/// - Removes any prior occurrence (after dedupe) so the result is unique.
/// - Truncates to [`MAX_RECENT_SEARCHES`].
/// - Preserves unknown JSON keys in `config.json`.
pub fn save_recent_search(query: &str) -> Result<Vec<String>, ConfigError> {
    save_recent_search_to(&crate::paths::config_path(), query)
}

/// Test-friendly variant of [`save_recent_search`] that writes to `path`.
pub fn save_recent_search_to(path: &Path, query: &str) -> Result<Vec<String>, ConfigError> {
    let trimmed = query.trim();
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut raw = load_or_empty(path)?;
    let mut list = extract_cleaned(&raw);

    if trimmed.is_empty() {
        // Match Python: no-op write but still return current list.
        return Ok(list);
    }

    list.retain(|q| q != trimmed);
    list.insert(0, trimmed.to_string());
    list.truncate(MAX_RECENT_SEARCHES);

    write_list(path, &mut raw, &list)?;
    Ok(list)
}

/// Clear the persisted MRU list at the default config path.
pub fn clear_recent_searches() -> Result<(), ConfigError> {
    clear_recent_searches_at(&crate::paths::config_path())
}

/// Test-friendly variant of [`clear_recent_searches`] that targets `path`.
pub fn clear_recent_searches_at(path: &Path) -> Result<(), ConfigError> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut raw = load_or_empty(path)?;
    write_list(path, &mut raw, &[])
}

// ---------- internal helpers ----------

fn load_or_empty(path: &Path) -> Result<serde_json::Value, ConfigError> {
    if !path.exists() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(match raw {
        serde_json::Value::Object(_) => raw,
        _ => serde_json::Value::Object(serde_json::Map::new()),
    })
}

fn extract_cleaned(raw: &serde_json::Value) -> Vec<String> {
    let Some(arr) = raw.get("recent_searches").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else { continue };
        let trimmed = s.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

fn write_list(
    path: &Path,
    raw: &mut serde_json::Value,
    list: &[String],
) -> Result<(), ConfigError> {
    if !raw.is_object() {
        *raw = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "recent_searches".to_string(),
            serde_json::Value::Array(
                list.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let tmp: PathBuf = tmp_path(path);
    let bytes = serde_json::to_vec_pretty(raw)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    // `Path::with_extension` would drop a leading dot and lose `.json`, so
    // build the sibling file name by appending `.tmp` to the file's full
    // name instead.
    let file_name = path
        .file_name()
        .map(|f| f.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("config.json"));
    let mut tmp_name = file_name;
    tmp_name.push(".tmp");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_config(path: &Path, json: &str) {
        std::fs::write(path, json).unwrap();
    }

    fn read_value(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn read_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert!(get_recent_searches_from(&path).unwrap().is_empty());
    }

    #[test]
    fn read_returns_cleaned_dedup_list() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(
            &path,
            r#"{"recent_searches": ["one", " one ", "", 42, "two"]}"#,
        );
        assert_eq!(
            get_recent_searches_from(&path).unwrap(),
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn read_handles_missing_key_and_wrong_type() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"{"theme":"dark"}"#);
        assert!(get_recent_searches_from(&path).unwrap().is_empty());

        write_config(&path, r#"{"recent_searches":"nope"}"#);
        assert!(get_recent_searches_from(&path).unwrap().is_empty());
    }

    #[test]
    fn push_promotes_existing_to_front_and_caps() {
        // Mirrors the plan's golden test, adjusted for the JSON-pretty output.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"{"theme":"dark","recent_searches":["b","a"]}"#);
        save_recent_search_to(&path, "a").unwrap();
        let raw = read_value(&path);
        assert_eq!(raw["recent_searches"], serde_json::json!(["a", "b"]));
        assert_eq!(raw["theme"], "dark"); // preserved
    }

    #[test]
    fn push_inserts_new_at_front() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"{"recent_searches":["a","b"]}"#);
        let list = save_recent_search_to(&path, "c").unwrap();
        assert_eq!(list, vec!["c", "a", "b"]);
    }

    #[test]
    fn push_caps_at_max() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        // Pre-seed with MAX entries, oldest last.
        let seed: Vec<String> = (0..MAX_RECENT_SEARCHES)
            .map(|i| format!("q{i}"))
            .collect();
        let initial = serde_json::json!({ "recent_searches": seed });
        write_config(&path, &initial.to_string());

        let list = save_recent_search_to(&path, "fresh").unwrap();
        assert_eq!(list.len(), MAX_RECENT_SEARCHES);
        assert_eq!(list[0], "fresh");
        // The oldest entry ("q{MAX-1}") must have been dropped.
        assert!(!list.contains(&format!("q{}", MAX_RECENT_SEARCHES - 1)));
    }

    #[test]
    fn push_creates_config_file_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("config.json");
        let list = save_recent_search_to(&path, "first").unwrap();
        assert_eq!(list, vec!["first"]);
        let raw = read_value(&path);
        assert_eq!(raw["recent_searches"], serde_json::json!(["first"]));
    }

    #[test]
    fn push_empty_or_whitespace_is_noop_but_returns_current() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"{"recent_searches":["a","b"]}"#);
        let list = save_recent_search_to(&path, "   ").unwrap();
        assert_eq!(list, vec!["a", "b"]);
        // File untouched: still has original ordering.
        assert_eq!(
            read_value(&path)["recent_searches"],
            serde_json::json!(["a", "b"])
        );
    }

    #[test]
    fn push_trims_query_before_storing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"{"recent_searches":["a"]}"#);
        let list = save_recent_search_to(&path, "  b  ").unwrap();
        assert_eq!(list, vec!["b", "a"]);
    }

    #[test]
    fn unknown_keys_roundtrip_unchanged() {
        // Golden test: theme, sidebar_width, nested objects, arrays of
        // foreign shape must all survive a save_recent_search write.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = serde_json::json!({
            "theme": "opencode-dark",
            "sidebar_width": 36,
            "future_setting": {
                "nested": true,
                "list": [1, 2, 3],
            },
            "another_array": ["x", "y"],
            "recent_searches": ["old"],
        });
        write_config(&path, &original.to_string());

        save_recent_search_to(&path, "new").unwrap();

        let after = read_value(&path);
        assert_eq!(after["theme"], "opencode-dark");
        assert_eq!(after["sidebar_width"], 36);
        assert_eq!(after["future_setting"]["nested"], true);
        assert_eq!(after["future_setting"]["list"], serde_json::json!([1, 2, 3]));
        assert_eq!(after["another_array"], serde_json::json!(["x", "y"]));
        assert_eq!(after["recent_searches"], serde_json::json!(["new", "old"]));
    }

    #[test]
    fn clear_empties_list_and_preserves_other_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = serde_json::json!({
            "theme": "dark",
            "recent_searches": ["a", "b", "c"],
        });
        write_config(&path, &original.to_string());

        clear_recent_searches_at(&path).unwrap();

        let after = read_value(&path);
        assert_eq!(after["recent_searches"], serde_json::json!([]));
        assert_eq!(after["theme"], "dark");
        assert!(get_recent_searches_from(&path).unwrap().is_empty());
    }

    #[test]
    fn clear_on_missing_file_creates_empty_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        clear_recent_searches_at(&path).unwrap();
        let after = read_value(&path);
        assert_eq!(after["recent_searches"], serde_json::json!([]));
    }

    #[test]
    fn tmp_file_is_cleaned_up_after_rename() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        save_recent_search_to(&path, "hello").unwrap();
        let tmp = tmp_path(&path);
        assert!(!tmp.exists(), "{tmp:?} should have been renamed away");
        assert!(path.exists());
    }

    #[test]
    fn corrupt_or_non_object_top_level_recovers_to_fresh_object() {
        // If the existing file isn't a JSON object (e.g. someone dropped an
        // array in there), we reset to an empty object on write rather than
        // refusing to persist. Reads still succeed.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        write_config(&path, r#"["not","an","object"]"#);
        assert!(get_recent_searches_from(&path).unwrap().is_empty());

        save_recent_search_to(&path, "q").unwrap();
        let after = read_value(&path);
        assert!(after.is_object());
        assert_eq!(after["recent_searches"], serde_json::json!(["q"]));
    }
}
