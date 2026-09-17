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
    UnsupportedKind { kind: EnvironmentKind },
}

impl EnvironmentDispatchError {
    pub fn into_error_body(self) -> ErrorBody {
        match self {
            Self::UnsupportedKind { kind } => ErrorBody::new(
                ErrorCode::Unauthorized,
                format!("{kind} environment is registered but not an exec path"),
            ),
        }
    }
}

pub fn require_host_execution(kind: EnvironmentKind) -> Result<(), EnvironmentDispatchError> {
    match kind {
        EnvironmentKind::Host => Ok(()),
        EnvironmentKind::LinuxContainer => Err(EnvironmentDispatchError::UnsupportedKind { kind }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_allowed() {
        assert!(require_host_execution(EnvironmentKind::Host).is_ok());
    }

    #[test]
    fn linux_container_is_closed_failure() {
        let err = require_host_execution(EnvironmentKind::LinuxContainer).unwrap_err();
        let body = err.into_error_body();
        assert_eq!(body.code, ErrorCode::Unauthorized);
        assert!(body.message.contains("linux-container"));
        assert!(body.operation_id.is_none());
    }
}
