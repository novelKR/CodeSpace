//! CodeSpace permission domain. Not a Codex type and not an MCP schema.
//!
//! Path glob rules are **domain only**. Live enforcement stays the coarse
//! workspace profile (`allow(Write|Exec)`) plus PathSandbox. The Codex
//! parser is not the gateway allow engine.

use codespace_domain::Profile;
use serde::{Deserialize, Serialize};

use crate::Action;

/// Read / Write / Deny on a path, glob, or special root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathAccess {
    Read,
    Write,
    Deny,
}

/// Expressed path rule. Not consulted by live `apply_patch` authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRule {
    pub pattern: String,
    pub access: PathAccess,
}

/// Network axis is recorded only. It is not an allow engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkAxis {
    Restricted,
    Enabled,
}

/// Gateway-owned permission shape. Do not import `codex_protocol::PermissionProfile`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfile {
    pub paths: Vec<PathRule>,
    /// `Action::Exec` is this axis only. Write globs do not grant exec.
    pub process_exec: bool,
    pub network: NetworkAxis,
}

impl PermissionProfile {
    pub fn from_workspace_profile(profile: Profile) -> Self {
        match profile {
            Profile::ReadOnly => Self {
                paths: vec![PathRule {
                    pattern: "**".into(),
                    access: PathAccess::Read,
                }],
                process_exec: false,
                network: NetworkAxis::Restricted,
            },
            Profile::WorkspaceWrite => Self {
                paths: vec![PathRule {
                    pattern: "**".into(),
                    access: PathAccess::Write,
                }],
                process_exec: true,
                network: NetworkAxis::Restricted,
            },
        }
    }

    pub fn allows(&self, action: Action) -> bool {
        let _ = self.network;
        match action {
            Action::Read => self
                .paths
                .iter()
                .any(|rule| !matches!(rule.access, PathAccess::Deny)),
            Action::Write => self
                .paths
                .iter()
                .any(|rule| matches!(rule.access, PathAccess::Write)),
            Action::Exec => self.process_exec,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_maps_to_read_and_restricted_network() {
        let profile = PermissionProfile::from_workspace_profile(Profile::ReadOnly);
        assert!(profile.allows(Action::Read));
        assert!(!profile.allows(Action::Write));
        assert!(!profile.allows(Action::Exec));
        assert!(!profile.process_exec);
        assert_eq!(profile.network, NetworkAxis::Restricted);
    }

    #[test]
    fn workspace_write_allows_mutation() {
        let profile = PermissionProfile::from_workspace_profile(Profile::WorkspaceWrite);
        assert!(profile.allows(Action::Write));
        assert!(profile.allows(Action::Exec));
        assert!(profile.process_exec);
        assert_eq!(profile.network, NetworkAxis::Restricted);
    }

    #[test]
    fn process_exec_is_independent_of_write_glob() {
        let mut profile = PermissionProfile::from_workspace_profile(Profile::WorkspaceWrite);
        profile.process_exec = false;
        assert!(profile.allows(Action::Write));
        assert!(!profile.allows(Action::Exec));
    }

    #[test]
    fn network_axis_does_not_grant() {
        let mut profile = PermissionProfile::from_workspace_profile(Profile::ReadOnly);
        profile.network = NetworkAxis::Enabled;
        assert!(!profile.allows(Action::Write));
        assert!(!profile.allows(Action::Exec));
        assert!(profile.allows(Action::Read));
        profile.process_exec = true;
        assert!(profile.allows(Action::Exec));
        assert!(!profile.allows(Action::Write));
    }
}
