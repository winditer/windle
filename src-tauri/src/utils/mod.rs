pub mod format;
pub mod fs_ops;
pub mod history;
pub mod permissions;

use serde::ser::{Serialize, SerializeStruct, Serializer};

/// Every command returns this so the frontend always receives an `AppError`
/// shaped `{ code, message, path? }` payload on failure.
#[derive(Debug, thiserror::Error)]
pub enum WindleError {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("path not found: {0}")]
    NotFound(String),

    #[error("access denied: {0} — Full Disk Access may be required")]
    AccessDenied(String),

    #[error("refused to touch protected path: {0}")]
    Protected(String),

    #[error("{0} requires administrator privileges")]
    NeedsElevation(String),

    #[error("`{command}` failed: {message}")]
    Command { command: String, message: String },

    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

impl WindleError {
    /// Stable machine-readable discriminant, mirrored by `AppError.code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::NotFound(_) => "not_found",
            Self::AccessDenied(_) => "access_denied",
            Self::Protected(_) => "protected",
            Self::NeedsElevation(_) => "needs_elevation",
            Self::Command { .. } => "command_failed",
            Self::NotImplemented(_) => "not_implemented",
        }
    }

    /// The offending path, when the variant carries one.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::NotFound(path) | Self::AccessDenied(path) | Self::Protected(path) => Some(path),
            _ => None,
        }
    }
}

impl Serialize for WindleError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let path = self.path();
        let mut state = serializer.serialize_struct("AppError", if path.is_some() { 3 } else { 2 })?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        if let Some(path) = path {
            state.serialize_field("path", path)?;
        }
        state.end()
    }
}

pub type Result<T> = std::result::Result<T, WindleError>;
