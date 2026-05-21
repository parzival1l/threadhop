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

pub fn observations_dir() -> PathBuf {
    config_dir().join("observations")
}

pub fn observation_file(session_id: &str) -> PathBuf {
    observations_dir().join(format!("{session_id}.jsonl"))
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
    fn observations_dir_matches_python() {
        let p = observations_dir();
        assert!(p.ends_with(".config/threadhop/observations"));
    }

    #[test]
    fn claude_projects_dir_matches_python() {
        let p = claude_projects_dir();
        assert!(p.ends_with(".claude/projects"));
    }
}
