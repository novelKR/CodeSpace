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

    /// Backend currently implements apply_patch / workspace file ops.
    pub fn patch_supported(self) -> bool {
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
    UnsupportedPatch { kind: EnvironmentKind },
}

impl EnvironmentDispatchError {
    pub fn into_error_body(self) -> ErrorBody {
        match self {
            Self::UnsupportedExec { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not an exec path"),
            ),
            Self::UnsupportedPatch { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not a patch path"),
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

pub fn require_patch(kind: EnvironmentKind) -> Result<(), EnvironmentDispatchError> {
    if kind.patch_supported() {
        Ok(())
    } else {
        Err(EnvironmentDispatchError::UnsupportedPatch { kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_supports_exec_and_patch() {
        assert!(EnvironmentKind::Host.exec_supported());
        assert!(EnvironmentKind::Host.patch_supported());
        assert!(require_exec(EnvironmentKind::Host).is_ok());
        assert!(require_patch(EnvironmentKind::Host).is_ok());
    }

    #[test]
    fn linux_container_is_closed_failure() {
        assert!(!EnvironmentKind::LinuxContainer.exec_supported());
        assert!(!EnvironmentKind::LinuxContainer.patch_supported());
        let exec_err = require_exec(EnvironmentKind::LinuxContainer).unwrap_err();
        let exec_body = exec_err.into_error_body();
        assert_eq!(exec_body.code, ErrorCode::Unauthorized);
        assert!(exec_body.message.contains("linux-container"));
        assert!(exec_body.message.contains("exec path"));
        assert!(exec_body.operation_id.is_none());
        let patch_err = require_patch(EnvironmentKind::LinuxContainer).unwrap_err();
        let patch_body = patch_err.into_error_body();
        assert_eq!(patch_body.code, ErrorCode::Unauthorized);
        assert!(patch_body.message.contains("patch path"));
        assert!(patch_body.operation_id.is_none());
    }
}
