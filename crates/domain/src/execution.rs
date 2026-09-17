//! Client-facing execution contract. Effective semantics only; not a
//! Codex/Runner/UDS description.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::profile::Profile;

/// Advertised PTY size. Must match the isolated PTY adapter default.
pub const PTY_INITIAL_ROWS: u16 = 24;
pub const PTY_INITIAL_COLS: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ClientEnvironmentKind {
    Host,
    LinuxContainer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentExecutionInfo {
    pub kind: ClientEnvironmentKind,
    /// Operator/workspace registration selects the environment. Not a tool argument.
    pub client_selectable: bool,
    /// Backend currently supports exec for this environment.
    pub exec_supported: bool,
    /// Backend currently supports apply_patch for this environment.
    pub patch_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectivePermissionInfo {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

impl EffectivePermissionInfo {
    pub fn from_profile(profile: Profile) -> Self {
        match profile {
            Profile::ReadOnly => Self {
                read: true,
                write: false,
                exec: false,
            },
            Profile::WorkspaceWrite => Self {
                read: true,
                write: true,
                exec: true,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtyCapabilityInfo {
    pub supported: bool,
    pub default: bool,
    pub initial_rows: u16,
    pub initial_cols: u16,
    pub resize_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessCapabilityInfo {
    pub handles: bool,
    pub survives_request_end: bool,
    pub write_stdin: bool,
    pub incremental_read: bool,
    pub terminate: bool,
    pub output_combined: bool,
    pub tty: PtyCapabilityInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceSerializationInfo {
    pub live_process_holds_mutation_lease: bool,
    pub parallel_exec: bool,
    pub read_while_process_live: bool,
    pub find_while_process_live: bool,
    pub patch_while_process_live: bool,
    pub conflict_error: ErrorCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CommandSandboxState {
    None,
    LinuxSandbox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IsolationInfo {
    pub file_tools_workspace_scoped: bool,
    pub command_sandbox: CommandSandboxState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPolicyState {
    Restricted,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkEnforcementState {
    None,
    Enforced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NetworkInfo {
    pub policy: NetworkPolicyState,
    pub enforcement: NetworkEnforcementState,
    pub client_may_escalate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceExecutionInfo {
    pub environment: EnvironmentExecutionInfo,
    pub permissions: EffectivePermissionInfo,
    pub process: ProcessCapabilityInfo,
    pub serialization: WorkspaceSerializationInfo,
    pub isolation: IsolationInfo,
    pub network: NetworkInfo,
}

impl WorkspaceExecutionInfo {
    /// Effective contract for a registered workspace. `kind` is operator-selected.
    pub fn for_registered(profile: Profile, kind: ClientEnvironmentKind) -> Self {
        let supported = matches!(kind, ClientEnvironmentKind::Host);
        Self {
            environment: EnvironmentExecutionInfo {
                kind,
                client_selectable: false,
                exec_supported: supported,
                patch_supported: supported,
            },
            permissions: EffectivePermissionInfo::from_profile(profile),
            process: ProcessCapabilityInfo {
                handles: true,
                survives_request_end: true,
                write_stdin: true,
                incremental_read: true,
                terminate: true,
                output_combined: true,
                tty: PtyCapabilityInfo {
                    supported,
                    default: false,
                    initial_rows: PTY_INITIAL_ROWS,
                    initial_cols: PTY_INITIAL_COLS,
                    resize_supported: false,
                },
            },
            serialization: WorkspaceSerializationInfo {
                live_process_holds_mutation_lease: true,
                parallel_exec: false,
                read_while_process_live: true,
                find_while_process_live: true,
                patch_while_process_live: false,
                conflict_error: ErrorCode::WorkspaceBusy,
            },
            isolation: IsolationInfo {
                file_tools_workspace_scoped: true,
                command_sandbox: CommandSandboxState::None,
            },
            network: NetworkInfo {
                policy: NetworkPolicyState::Restricted,
                enforcement: NetworkEnforcementState::None,
                client_may_escalate: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_workspace_write_advertises_exec_and_fixed_pty() {
        let exec = WorkspaceExecutionInfo::for_registered(
            Profile::WorkspaceWrite,
            ClientEnvironmentKind::Host,
        );
        assert_eq!(exec.environment.kind, ClientEnvironmentKind::Host);
        assert!(!exec.environment.client_selectable);
        assert!(exec.environment.exec_supported);
        assert!(exec.environment.patch_supported);
        assert_eq!(
            exec.permissions,
            EffectivePermissionInfo {
                read: true,
                write: true,
                exec: true,
            }
        );
        assert!(exec.process.tty.supported);
        assert!(!exec.process.tty.default);
        assert_eq!(exec.process.tty.initial_rows, 24);
        assert_eq!(exec.process.tty.initial_cols, 80);
        assert!(!exec.process.tty.resize_supported);
        assert!(exec.process.output_combined);
        assert_eq!(exec.isolation.command_sandbox, CommandSandboxState::None);
        assert_eq!(exec.network.policy, NetworkPolicyState::Restricted);
        assert_eq!(exec.network.enforcement, NetworkEnforcementState::None);
        assert!(!exec.network.client_may_escalate);
        assert_eq!(exec.serialization.conflict_error, ErrorCode::WorkspaceBusy);
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["serialization"]["conflict_error"], "WORKSPACE_BUSY");
        assert!(json.get("environment_id").is_none());
        assert_eq!(json["environment"]["kind"], "host");
        assert_eq!(json["isolation"]["command_sandbox"], "none");
        assert_eq!(json["network"]["enforcement"], "none");
    }

    #[test]
    fn host_read_only_denies_write_and_exec() {
        let exec =
            WorkspaceExecutionInfo::for_registered(Profile::ReadOnly, ClientEnvironmentKind::Host);
        assert_eq!(
            exec.permissions,
            EffectivePermissionInfo {
                read: true,
                write: false,
                exec: false,
            }
        );
        assert!(exec.environment.exec_supported);
    }

    #[test]
    fn linux_container_write_permits_exec_policy_but_not_backend() {
        let exec = WorkspaceExecutionInfo::for_registered(
            Profile::WorkspaceWrite,
            ClientEnvironmentKind::LinuxContainer,
        );
        assert!(exec.permissions.exec);
        assert!(!exec.environment.exec_supported);
        assert!(!exec.environment.patch_supported);
        assert!(!exec.process.tty.supported);
        assert_eq!(exec.environment.kind, ClientEnvironmentKind::LinuxContainer);
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["environment"]["kind"], "linux-container");
        assert!(json.get("environment_id").is_none());
        assert!(!json.to_string().contains("\"environment_id\""));
    }
}
