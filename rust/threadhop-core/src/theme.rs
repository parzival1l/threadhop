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

/// Blend two `#rrggbb` hex colors with `alpha` (fraction of `fg` mixed into
/// `bg`). Returns a `#rrggbb` string. Math: `out = bg * (1 - alpha) + fg *
/// alpha`, per channel, clamped to `[0, 255]`. `alpha` is clamped to
/// `[0.0, 1.0]`. Malformed hex returns `bg_hex` unchanged so callers never
/// panic on a missing or garbled palette value.
///
/// Phase A line-shaper uses this for selection-mode row tints and the
/// `$color 15%` Textual idiom the parity plan calls out in §3.1.
pub fn blend(fg_hex: &str, bg_hex: &str, alpha: f32) -> String {
    let Some((fr, fg, fb)) = hex_to_rgb(fg_hex) else {
        return bg_hex.to_string();
    };
    let Some((br, bg, bb)) = hex_to_rgb(bg_hex) else {
        return bg_hex.to_string();
    };
    let a = alpha.clamp(0.0, 1.0);
    let mix = |fg_c: u8, bg_c: u8| -> u8 {
        let v = (bg_c as f32) * (1.0 - a) + (fg_c as f32) * a;
        v.round().clamp(0.0, 255.0) as u8
    };
    let r = mix(fr, br);
    let g = mix(fg, bg);
    let b = mix(fb, bb);
    format!("#{r:02x}{g:02x}{b:02x}")
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
            // Phase A.5: lifted from #141414 → #1c1c1c so panel rows
            // (digest bar, sidebar tints) sit clearly above the canvas
            // background on most terminals. ~11% luminance bump puts the
            // delta past the just-noticeable-difference threshold.
            background_panel: "#1c1c1c".into(),
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

impl Theme {
    /// A built-in light variant. Mirrors `default_dark` but with the
    /// canvas / panel / element backgrounds inverted toward white so
    /// users on light terminals can pick this without loading a JSON
    /// theme. Phase A.5 lifted dark `background_panel` past JND; we
    /// drop the light panel slightly so the elevation reads the same
    /// direction (panel sits *above* canvas).
    pub fn default_light() -> Self {
        let steps: [&str; 13] = [
            "", "#ffffff", "#f5f5f5", "#ececec", "#dcdcdc", "#cccccc", "#bcbcbc", "#a8a8a8",
            "#8a8a8a", "#fab283", "#ffc09f", "#606060", "#1a1a1a",
        ];
        let steps: [String; 13] = std::array::from_fn(|i| steps[i].to_string());

        Theme {
            name: "default-light".to_string(),
            variant: Variant::Light,
            primary: "#fab283".into(),
            secondary: "#3a78d4".into(),
            accent: "#7a5fcf".into(),
            success: "#3aa755".into(),
            warning: "#c47b1a".into(),
            error: "#c0392b".into(),
            info: "#247bb0".into(),
            foreground: "#1a1a1a".into(),
            text_muted: "#606060".into(),
            background: "#ffffff".into(),
            background_panel: "#f0f0f0".into(),
            background_element: "#ececec".into(),
            border: "#c0c0c0".into(),
            border_active: "#909090".into(),
            border_subtle: "#dcdcdc".into(),
            steps,
            diff_added: "#1a8a6a".into(),
            diff_removed: "#a02c3c".into(),
            diff_added_bg: "#d9f5ea".into(),
            diff_removed_bg: "#f9d9df".into(),
            syntax_keyword: "#7a5fcf".into(),
            syntax_function: "#b06b1a".into(),
            syntax_string: "#3aa755".into(),
            syntax_comment: "#808080".into(),
            syntax_type: "#a07e1a".into(),
            syntax_number: "#c47b1a".into(),
            syntax_variable: "#c0392b".into(),
        }
    }

    /// Resolve a theme by user-facing name. Mirrors the Python loader's
    /// `<stem>-<variant>` naming so config files written by the Python
    /// TUI are recognised by the Rust binary without translation.
    ///
    /// Recognised names:
    ///   * `default-dark`, `default_dark`, `default`  — built-in dark
    ///   * `default-light`, `default_light`           — built-in light
    ///   * `cursor-dark`, `cursor_dark`               — Cursor IDE dark palette (built-in dark today, real vendored load lands with Worker H+1)
    ///   * `opencode-dark`, `opencode-light`, `nord-dark`, etc. — currently fall through to default_dark; real vendored JSON loading is a follow-up
    ///
    /// Unknown names fall back to [`Self::default_dark`] so a stale
    /// config never bricks the TUI. Pre-pop intentionally does NOT
    /// touch the filesystem here — the vendored-theme loader path
    /// stays as `load_theme(path)` for callers that want JSON.
    pub fn load_by_name(name: &str) -> Self {
        let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "default-light" | "light" => Self::default_light(),
            "default-dark" | "default" | "dark" => Self::default_dark(),
            // `cursor-dark` is the canonical name the user's config
            // ships today. Pre-pop returns default_dark but with a
            // tweaked accent so call sites can verify the lookup ran.
            // A later pass (Worker H+1 or a follow-up theme task) will
            // load `cursor.json` from the vendored set and resolve it
            // properly.
            "cursor-dark" => {
                let mut t = Self::default_dark();
                t.name = "cursor-dark".to_string();
                // Cursor's accent leans purple-blue; nudge toward it so
                // `App::new` can prove the rename + recolour worked.
                t.accent = "#5b8def".into();
                t
            }
            _ => Self::default_dark(),
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

    // ---- blend ----------------------------------------------------------

    #[test]
    fn blend_at_zero_returns_bg() {
        assert_eq!(blend("#ffffff", "#000000", 0.0), "#000000");
    }

    #[test]
    fn blend_at_one_returns_fg() {
        assert_eq!(blend("#ffffff", "#000000", 1.0), "#ffffff");
    }

    #[test]
    fn blend_mid_alpha_mixes() {
        // 0.5 between red and black should produce something close to #7f0000
        // (off-by-one rounding tolerance allowed).
        let out = blend("#ff0000", "#000000", 0.5);
        let (r, g, b) = hex_to_rgb(&out).unwrap();
        assert!((r as i32 - 0x7f).abs() <= 1, "r={r:#x}");
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn blend_clamps_oob_alpha() {
        // alpha > 1.0 should behave like alpha = 1.0 (full fg).
        assert_eq!(blend("#ffffff", "#000000", 5.0), "#ffffff");
        // alpha < 0.0 should behave like alpha = 0.0 (full bg).
        assert_eq!(blend("#ffffff", "#123456", -1.0), "#123456");
    }

    #[test]
    fn blend_malformed_fg_returns_bg() {
        assert_eq!(blend("garbage", "#123456", 0.5), "#123456");
        assert_eq!(blend("#ffffff", "garbage", 0.5), "garbage");
    }

    #[test]
    fn load_by_name_resolves_known_aliases() {
        // Built-in defaults via several aliases.
        assert_eq!(Theme::load_by_name("default-dark").name, "default-dark");
        assert_eq!(Theme::load_by_name("default_dark").name, "default-dark");
        assert_eq!(Theme::load_by_name("dark").name, "default-dark");
        assert_eq!(Theme::load_by_name("default-light").name, "default-light");
        assert_eq!(Theme::load_by_name("light").name, "default-light");
    }

    #[test]
    fn load_by_name_cursor_dark_overrides_accent() {
        // The cursor-dark recognised name should produce a theme whose
        // accent is visibly distinct from default_dark — that's how
        // `App::new`'s test proves the config-driven lookup actually
        // ran.
        let default = Theme::default_dark();
        let cursor = Theme::load_by_name("cursor-dark");
        assert_eq!(cursor.name, "cursor-dark");
        assert_ne!(
            cursor.accent, default.accent,
            "cursor-dark must distinguish itself from default-dark on at least one cell"
        );
    }

    #[test]
    fn load_by_name_unknown_falls_back_to_default_dark() {
        let t = Theme::load_by_name("not-a-real-theme");
        assert_eq!(t.name, Theme::default_dark().name);
        assert_eq!(t.accent, Theme::default_dark().accent);
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
