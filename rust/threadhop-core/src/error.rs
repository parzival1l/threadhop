//! Per-module error enums. Functions never panic on bad input.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema version mismatch: db={db}, expected={expected}")]
    SchemaMismatch { db: u32, expected: u32 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum JsonlError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum FtsError {
    #[error("db: {0}")]
    Db(#[from] DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum ObservationError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("db: {0}")]
    Db(#[from] DbError),
}

#[derive(Debug, Error)]
pub enum SessionDetectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_error_schema_mismatch_display() {
        let err = DbError::SchemaMismatch { db: 8, expected: 9 };
        assert_eq!(err.to_string(), "schema version mismatch: db=8, expected=9");
    }

    #[test]
    fn db_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: DbError = io_err.into();
        assert!(matches!(err, DbError::Io(_)));
        assert!(err.to_string().starts_with("io: "));
    }

    #[test]
    fn db_error_from_rusqlite() {
        let sql_err = rusqlite::Error::QueryReturnedNoRows;
        let err: DbError = sql_err.into();
        assert!(matches!(err, DbError::Sqlite(_)));
        assert!(err.to_string().starts_with("sqlite error: "));
    }

    #[test]
    fn jsonl_error_from_serde() {
        let serde_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err: JsonlError = serde_err.into();
        assert!(matches!(err, JsonlError::Decode(_)));
        assert!(err.to_string().starts_with("decode: "));
    }

    #[test]
    fn jsonl_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let err: JsonlError = io_err.into();
        assert!(matches!(err, JsonlError::Io(_)));
    }

    #[test]
    fn fts_error_from_db() {
        let db_err = DbError::SchemaMismatch { db: 1, expected: 9 };
        let err: FtsError = db_err.into();
        assert!(matches!(err, FtsError::Db(_)));
        assert!(err.to_string().starts_with("db: "));
    }

    #[test]
    fn fts_error_from_rusqlite() {
        let sql_err = rusqlite::Error::QueryReturnedNoRows;
        let err: FtsError = sql_err.into();
        assert!(matches!(err, FtsError::Sqlite(_)));
    }

    #[test]
    fn observation_error_from_db() {
        let db_err = DbError::SchemaMismatch { db: 1, expected: 9 };
        let err: ObservationError = db_err.into();
        assert!(matches!(err, ObservationError::Db(_)));
    }

    #[test]
    fn observation_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "x");
        let err: ObservationError = io_err.into();
        assert!(matches!(err, ObservationError::Io(_)));
    }

    #[test]
    fn observation_error_from_serde() {
        let serde_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err: ObservationError = serde_err.into();
        assert!(matches!(err, ObservationError::Decode(_)));
    }

    #[test]
    fn session_detect_error_parse_display() {
        let err = SessionDetectError::Parse("bad pid".into());
        assert_eq!(err.to_string(), "parse: bad pid");
    }

    #[test]
    fn session_detect_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "x");
        let err: SessionDetectError = io_err.into();
        assert!(matches!(err, SessionDetectError::Io(_)));
    }

    #[test]
    fn theme_error_from_io_and_serde() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: ThemeError = io_err.into();
        assert!(matches!(err, ThemeError::Io(_)));

        let serde_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err: ThemeError = serde_err.into();
        assert!(matches!(err, ThemeError::Decode(_)));
    }

    #[test]
    fn config_error_from_io_and_serde() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: ConfigError = io_err.into();
        assert!(matches!(err, ConfigError::Io(_)));

        let serde_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err: ConfigError = serde_err.into();
        assert!(matches!(err, ConfigError::Decode(_)));
    }
}
