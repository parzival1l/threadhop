//! Canonical filesystem paths. macOS-only — HOME, no XDG fallback.
use std::path::PathBuf;

fn home() -> PathBuf {
    dirs::home_dir().expect("HOME must be set on macOS")
}

pub fn config_dir() -> PathBuf {
    home().join(".config").join("threadhop")
}

pub fn db_path() -> PathBuf {
    config_dir().join("sessions.db")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Where `threadhop prepare` writes transfer tickets (ADR-029). Shared with
/// the Python CLI's `transfers.TRANSFERS_DIR`.
pub fn transfers_dir() -> PathBuf {
    config_dir().join("transfers")
}

pub fn logs_dir() -> PathBuf {
    config_dir().join("logs")
}

pub fn claude_projects_dir() -> PathBuf {
    home().join(".claude").join("projects")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_path_is_under_home_config_threadhop() {
        let p = db_path();
        assert!(p.ends_with(".config/threadhop/sessions.db"));
    }

    #[test]
    fn transfers_dir_matches_python() {
        let p = transfers_dir();
        assert!(p.ends_with(".config/threadhop/transfers"));
    }

    #[test]
    fn claude_projects_dir_matches_python() {
        let p = claude_projects_dir();
        assert!(p.ends_with(".claude/projects"));
    }
}
