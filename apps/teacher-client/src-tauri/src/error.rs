//! Unified error type for every command exposed to the webview.
//!
//! Commands used to return `Result<T, String>`. They now return
//! [`AppResult<T>`], but [`AppError`] still serializes to a plain string, so the
//! JSON error payload the frontend receives is unchanged.

use serde::ser::{Serialize, Serializer};

/// Failure returned by a Tauri command.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Generic, human-readable failure (validation, keyring, ...).
    #[error("{0}")]
    Message(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Tauri window/runtime failure.
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
}

impl AppError {
    /// Build a generic error from anything string-like.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

// Tauri serializes command errors as JSON values; the frontend expects a plain
// string payload, which is exactly what `String`-returning commands produced.
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Convenience alias used by every fallible command.
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::AppError;

    #[test]
    fn message_serializes_to_plain_string() {
        let json = serde_json::to_string(&AppError::msg("boom")).expect("serialize error");
        assert_eq!(json, "\"boom\"");
    }

    #[test]
    fn io_error_serializes_to_its_display_string() {
        let error = AppError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing file",
        ));
        let json = serde_json::to_string(&error).expect("serialize error");
        assert_eq!(json, "\"missing file\"");
    }
}
