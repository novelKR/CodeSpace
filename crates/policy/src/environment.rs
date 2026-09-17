//! Operator-registered execution location. Not an MCP tool field.
//!
//! `EnvironmentKind` is not a transport. `Host` may use in-process or an
//! opt-in UDS worker on the same host. `LinuxContainer` stays fail-closed.

use codespace_domain::{ErrorBody, ErrorCode};
use serde::{Deserialize, Serialize};

pub const DEFAULT_ENVIRONMENT_ID: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentKind {
    #[default]
    Host,
    LinuxContainer,
}

impl EnvironmentKind {
    /// Backend currently implements exec for this environment.
    pub fn exec_supported(self) -> bool {
        matches!(self, Self::Host)
    }

    /// Backend currently implements workspace file reads.
    pub fn file_read_supported(self) -> bool {
        matches!(self, Self::Host)
    }

    /// Backend currently implements workspace file writes / apply_patch.
    pub fn file_write_supported(self) -> bool {
        matches!(self, Self::Host)
    }
}

impl std::fmt::Display for EnvironmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host => write!(f, "host"),
            Self::LinuxContainer => write!(f, "linux-container"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub kind: EnvironmentKind,
}

impl Environment {
    pub fn local_host() -> Self {
        Self {
            id: DEFAULT_ENVIRONMENT_ID.to_string(),
            kind: EnvironmentKind::Host,
        }
    }
}

/// Internal dispatch refusal. MCP never sees this type; map with
/// [`EnvironmentDispatchError::into_error_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentDispatchError {
    UnsupportedExec { kind: EnvironmentKind },
    UnsupportedFileRead { kind: EnvironmentKind },
    UnsupportedFileWrite { kind: EnvironmentKind },
}

impl EnvironmentDispatchError {
    pub fn into_error_body(self) -> ErrorBody {
        match self {
            Self::UnsupportedExec { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not an exec path"),
            ),
            Self::UnsupportedFileRead { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not a file-read path"),
            ),
            Self::UnsupportedFileWrite { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not a file-write path"),
            ),
        }
    }
}

pub fn require_exec(kind: EnvironmentKind) -> Result<(), EnvironmentDispatchError> {
    if kind.exec_supported() {
        Ok(())
    } else {
        Err(EnvironmentDispatchError::UnsupportedExec { kind })
    }
}

pub fn require_file_read(kind: EnvironmentKind) -> Result<(), EnvironmentDispatchError> {
    if kind.file_read_supported() {
        Ok(())
    } else {
        Err(EnvironmentDispatchError::UnsupportedFileRead { kind })
    }
}

pub fn require_file_write(kind: EnvironmentKind) -> Result<(), EnvironmentDispatchError> {
    if kind.file_write_supported() {
        Ok(())
    } else {
        Err(EnvironmentDispatchError::UnsupportedFileWrite { kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_supports_exec_and_file_ops() {
        assert!(EnvironmentKind::Host.exec_supported());
        assert!(EnvironmentKind::Host.file_read_supported());
        assert!(EnvironmentKind::Host.file_write_supported());
        assert!(require_exec(EnvironmentKind::Host).is_ok());
        assert!(require_file_read(EnvironmentKind::Host).is_ok());
        assert!(require_file_write(EnvironmentKind::Host).is_ok());
    }

    #[test]
    fn linux_container_is_closed_failure() {
        assert!(!EnvironmentKind::LinuxContainer.exec_supported());
        assert!(!EnvironmentKind::LinuxContainer.file_read_supported());
        assert!(!EnvironmentKind::LinuxContainer.file_write_supported());
        let exec_err = require_exec(EnvironmentKind::LinuxContainer).unwrap_err();
        let exec_body = exec_err.into_error_body();
        assert_eq!(exec_body.code, ErrorCode::Unauthorized);
        assert!(exec_body.message.contains("linux-container"));
        assert!(exec_body.message.contains("exec path"));
        assert!(exec_body.operation_id.is_none());
        let read_err = require_file_read(EnvironmentKind::LinuxContainer).unwrap_err();
        let read_body = read_err.into_error_body();
        assert_eq!(read_body.code, ErrorCode::Unauthorized);
        assert!(read_body.message.contains("file-read path"));
        assert!(read_body.operation_id.is_none());
        let write_err = require_file_write(EnvironmentKind::LinuxContainer).unwrap_err();
        let write_body = write_err.into_error_body();
        assert_eq!(write_body.code, ErrorCode::Unauthorized);
        assert!(write_body.message.contains("file-write path"));
        assert!(write_body.operation_id.is_none());
    }
}
