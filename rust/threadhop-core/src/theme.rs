//! OpenCode theme JSON loader.
//!
//! OpenCode themes (https://opencode.ai/theme.json) ship a `defs` table of
//! named hex colors plus a `theme` table that maps semantic roles to those
//! defs. Each role carries a `dark` and a `light` slot — each slot is either
//! a literal hex string (`#rrggbb`) or a key into `defs`.
//!
//! This module parses such files into a `Theme` struct with a single
//! resolved variant (caller picks `dark` or `light` at load time). All
//! colors land as hex strings (`#rrggbb`); the TUI converts to
//! `ratatui::style::Color::Rgb(..)` at render time via `hex_to_rgb`.
//!
//! The struct intentionally exposes named fields (instead of an opaque map)
//! so the Phase 6 TUI can pattern-match without string lookups, and
//! `Default` + `default_dark()` give Phase 2 widgets a forward-compatible
//! `&Theme` to take by reference before any JSON is actually loaded.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::ThemeError;

/// Whether to resolve the `dark` or the `light` slot of each role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Dark,
    Light,
}

/// A resolved OpenCode theme — all colors are literal `#rrggbb` hex strings.
///
/// Fields are grouped by concern (semantic roles, surfaces, borders, the
/// 12-step neutral scale, diff palette, syntax palette). The TUI does not
/// need every field today, but Phase 6 styles will reach for syntax/diff
/// colors and we'd rather pre-allocate the shape than chase strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Human-facing name, derived from the JSON filename + variant suffix
    /// (e.g. `opencode-dark`).
    pub name: String,
    pub variant: Variant,

    // Semantic roles ---------------------------------------------------
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
    pub info: String,

    // Text / surfaces --------------------------------------------------
    pub foreground: String,
    pub text_muted: String,
    pub background: String,
    pub background_panel: String,
    pub background_element: String,

    // Borders ----------------------------------------------------------
    pub border: String,
    pub border_active: String,
    pub border_subtle: String,

    // 12-step neutral scale (step1..step12). Indexed 0..12, so `steps[0]`
    // is unused and `steps[i]` (1..=12) is `darkStepN` / `lightStepN`.
    pub steps: [String; 13],

    // Diff palette -----------------------------------------------------
    pub diff_added: String,
    pub diff_removed: String,
    pub diff_added_bg: String,
    pub diff_removed_bg: String,

    // Syntax palette ---------------------------------------------------
    pub syntax_keyword: String,
    pub syntax_function: String,
    pub syntax_string: String,
    pub syntax_comment: String,
    pub syntax_type: String,
    pub syntax_number: String,
    pub syntax_variable: String,
}

// ---- raw JSON shape ------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawTheme {
    defs: HashMap<String, String>,
    theme: HashMap<String, RawSlot>,
}

#[derive(Debug, Deserialize)]
struct RawSlot {
    dark: Option<String>,
    light: Option<String>,
}

// ---- loaders -------------------------------------------------------------

/// Load an OpenCode JSON theme from `path`, resolving the `dark` slot.
///
/// This is the variant the plan's test ("`assert!(!theme.foreground.is_empty())`")
/// implicitly exercises and is the one the TUI defaults to.
pub fn load_theme(path: &Path) -> Result<Theme, ThemeError> {
    load_theme_variant(path, Variant::Dark)
}

/// Load an OpenCode JSON theme from `path`, resolving the given variant.
pub fn load_theme_variant(path: &Path, variant: Variant) -> Result<Theme, ThemeError> {
    let bytes = std::fs::read(path)?;
    let raw: RawTheme = serde_json::from_slice(&bytes)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("theme")
        .to_string();
    let suffix = match variant {
        Variant::Dark => "dark",
        Variant::Light => "light",
    };
    Ok(build_theme(format!("{stem}-{suffix}"), raw, variant))
}

fn build_theme(name: String, raw: RawTheme, variant: Variant) -> Theme {
    let defs = &raw.defs;
    let table = &raw.theme;

    let get = |role: &str| -> String {
        let Some(slot) = table.get(role) else {
            return String::new();
        };
        let value = match variant {
            Variant::Dark => slot.dark.as_deref(),
            Variant::Light => slot.light.as_deref(),
        };
        match value {
            Some(v) if v.starts_with('#') => v.to_string(),
            Some(v) => defs.get(v).cloned().unwrap_or_else(|| v.to_string()),
            None => String::new(),
        }
    };

    let step_prefix = match variant {
        Variant::Dark => "dark",
        Variant::Light => "light",
    };
    // 13-element array, index 0 is a sentinel so steps[N] == stepN.
    let steps: [String; 13] = std::array::from_fn(|i| {
        if i == 0 {
            return String::new();
        }
        defs.get(&format!("{step_prefix}Step{i}"))
            .cloned()
            .unwrap_or_default()
    });

    Theme {
        name,
        variant,
        primary: get("primary"),
        secondary: get("secondary"),
        accent: get("accent"),
        success: get("success"),
        warning: get("warning"),
        error: get("error"),
        info: get("info"),
        foreground: get("text"),
        text_muted: get("textMuted"),
        background: get("background"),
        background_panel: get("backgroundPanel"),
        background_element: get("backgroundElement"),
        border: get("border"),
        border_active: get("borderActive"),
        border_subtle: get("borderSubtle"),
        steps,
        diff_added: get("diffAdded"),
        diff_removed: get("diffRemoved"),
        diff_added_bg: get("diffAddedBg"),
        diff_removed_bg: get("diffRemovedBg"),
        syntax_keyword: get("syntaxKeyword"),
        syntax_function: get("syntaxFunction"),
        syntax_string: get("syntaxString"),
        syntax_comment: get("syntaxComment"),
        syntax_type: get("syntaxType"),
        syntax_number: get("syntaxNumber"),
        syntax_variable: get("syntaxVariable"),
    }
}

// ---- helpers -------------------------------------------------------------

/// Convert a `#rrggbb` (or `#rgb`) hex string to an `(r, g, b)` tuple.
///
/// Returns `None` for malformed input; the TUI is expected to fall back to
/// a default color rather than panic.
pub fn hex_to_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.strip_prefix('#')?;
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some((r, g, b))
        }
        3 => {
            // Shorthand: expand each nibble to a byte (`#abc` → `#aabbcc`).
            let r = u8::from_str_radix(&s[0..1], 16).ok()?;
            let g = u8::from_str_radix(&s[1..2], 16).ok()?;
            let b = u8::from_str_radix(&s[2..3], 16).ok()?;
            Some((r * 17, g * 17, b * 17))
        }
        _ => None,
    }
}

// ---- defaults ------------------------------------------------------------

impl Theme {
    /// A built-in dark theme that doesn't touch the filesystem. Used by
    /// Phase 2 TUI widgets that take `&Theme` before any JSON is loaded.
    ///
    /// Values mirror the dark variant of the vendored `opencode.json` so
    /// rendered output is recognisable even with no theme file present.
    pub fn default_dark() -> Self {
        let steps: [&str; 13] = [
            "", "#0a0a0a", "#141414", "#1e1e1e", "#282828", "#323232", "#3c3c3c", "#484848",
            "#606060", "#fab283", "#ffc09f", "#808080", "#eeeeee",
        ];
        let steps: [String; 13] = std::array::from_fn(|i| steps[i].to_string());

        Theme {
            name: "default-dark".to_string(),
            variant: Variant::Dark,
            primary: "#fab283".into(),
            secondary: "#5c9cf5".into(),
            accent: "#9d7cd8".into(),
            success: "#7fd88f".into(),
            warning: "#f5a742".into(),
            error: "#e06c75".into(),
            info: "#56b6c2".into(),
            foreground: "#eeeeee".into(),
            text_muted: "#808080".into(),
            background: "#0a0a0a".into(),
            background_panel: "#141414".into(),
            background_element: "#1e1e1e".into(),
            border: "#484848".into(),
            border_active: "#606060".into(),
            border_subtle: "#3c3c3c".into(),
            steps,
            diff_added: "#4fd6be".into(),
            diff_removed: "#c53b53".into(),
            diff_added_bg: "#20303b".into(),
            diff_removed_bg: "#37222c".into(),
            syntax_keyword: "#9d7cd8".into(),
            syntax_function: "#fab283".into(),
            syntax_string: "#7fd88f".into(),
            syntax_comment: "#808080".into(),
            syntax_type: "#e5c07b".into(),
            syntax_number: "#f5a742".into(),
            syntax_variable: "#e06c75".into(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vendored(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../threadhop_core/tui/theme/vendored")
            .join(name)
    }

    #[test]
    fn loads_opencode_theme_from_repo() {
        let p = vendored("opencode.json");
        let theme = load_theme(&p).unwrap();
        assert!(!theme.foreground.is_empty());
        assert_eq!(theme.name, "opencode-dark");
        assert_eq!(theme.variant, Variant::Dark);
    }

    #[test]
    fn resolves_defs_references_to_literal_hex() {
        // `primary.dark` points at `darkStep9` in defs, which is `#fab283`.
        let theme = load_theme(&vendored("opencode.json")).unwrap();
        assert_eq!(theme.primary, "#fab283");
        // `background.dark` -> darkStep1 -> #0a0a0a.
        assert_eq!(theme.background, "#0a0a0a");
        // Direct hex (no defs indirection): diffAdded.dark = "#4fd6be".
        assert_eq!(theme.diff_added, "#4fd6be");
    }

    #[test]
    fn light_variant_resolves_against_light_steps() {
        let theme =
            load_theme_variant(&vendored("opencode.json"), Variant::Light).unwrap();
        assert_eq!(theme.variant, Variant::Light);
        assert_eq!(theme.name, "opencode-light");
        // lightStep1 = "#ffffff" per opencode.json.
        assert_eq!(theme.background, "#ffffff");
        assert_eq!(theme.foreground, "#1a1a1a");
    }

    #[test]
    fn steps_array_is_indexed_one_based() {
        let theme = load_theme(&vendored("opencode.json")).unwrap();
        assert_eq!(theme.steps[0], "");
        assert_eq!(theme.steps[1], "#0a0a0a");
        assert_eq!(theme.steps[12], "#eeeeee");
    }

    #[test]
    fn loads_other_vendored_themes_without_panicking() {
        for name in ["catppuccin.json", "gruvbox.json", "nord.json", "tokyonight.json"] {
            let p = vendored(name);
            let theme = load_theme(&p).unwrap();
            assert!(!theme.background.is_empty(), "{name} missing background");
            assert!(!theme.foreground.is_empty(), "{name} missing foreground");
        }
    }

    #[test]
    fn hex_to_rgb_parses_full_and_short_form() {
        assert_eq!(hex_to_rgb("#ffffff"), Some((255, 255, 255)));
        assert_eq!(hex_to_rgb("#000000"), Some((0, 0, 0)));
        assert_eq!(hex_to_rgb("#fab283"), Some((0xfa, 0xb2, 0x83)));
        // Shorthand: #abc → #aabbcc.
        assert_eq!(hex_to_rgb("#abc"), Some((0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn hex_to_rgb_rejects_malformed_input() {
        assert_eq!(hex_to_rgb("ffffff"), None); // no leading #
        assert_eq!(hex_to_rgb("#fffff"), None); // odd length
        assert_eq!(hex_to_rgb("#gggggg"), None); // not hex digits
        assert_eq!(hex_to_rgb(""), None);
    }

    #[test]
    fn load_theme_returns_io_error_for_missing_file() {
        let err = load_theme(std::path::Path::new("/nonexistent/path/theme.json"));
        assert!(matches!(err, Err(ThemeError::Io(_))));
    }

    #[test]
    fn load_theme_returns_decode_error_for_bad_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        std::fs::write(&p, b"{not json").unwrap();
        let err = load_theme(&p);
        assert!(matches!(err, Err(ThemeError::Decode(_))));
    }

    #[test]
    fn default_dark_has_populated_fields_and_matches_default_trait() {
        let t = Theme::default_dark();
        assert_eq!(t.name, "default-dark");
        assert_eq!(t.variant, Variant::Dark);
        assert!(!t.foreground.is_empty());
        assert!(!t.background.is_empty());
        // Spot-check a value mirrored from vendored opencode.json.
        assert_eq!(t.primary, "#fab283");
        assert_eq!(t.steps[0], "");
        assert_eq!(t.steps[1], "#0a0a0a");

        let d: Theme = Default::default();
        assert_eq!(d, t);
    }
}
