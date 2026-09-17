//! Operator-registered execution location. Not an MCP tool field.

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

pub fn require_host_execution(kind: EnvironmentKind) -> Result<(), ErrorBody> {
    match kind {
        EnvironmentKind::Host => Ok(()),
        EnvironmentKind::LinuxContainer => Err(ErrorBody::new(
            ErrorCode::Unauthorized,
            "linux-container environment is registered but not an exec path",
        )),
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
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(err.message.contains("linux-container"));
    }
}
