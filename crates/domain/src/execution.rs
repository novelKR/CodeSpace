//! Client-facing execution contract. Effective semantics only; not a
//! Codex/Runner/UDS description.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;

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
    /// Backend currently supports workspace file reads.
    pub file_read_supported: bool,
    /// Backend currently supports workspace file writes / apply_patch.
    pub file_write_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectivePermissionInfo {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
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
    /// `read_process` exposes one output stream.
    ///
    /// stdout/stderr source identity is not preserved. For pipe-backed
    /// processes stdout and stderr are pumped independently, so their
    /// relative ordering is not guaranteed. PTY output is the terminal
    /// master stream.
    pub output_combined: bool,
    pub tty: PtyCapabilityInfo,
}

/// Static eligibility of a file operation in this workspace.
///
/// Availability combines effective permission with backend support.
/// It does not include transient workspace occupancy; `apply_patch`
/// may still return `WORKSPACE_BUSY`. Occupancy rules are in
/// `serialization`. Tool existence is reported separately by
/// `tools_exposed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileOperationInfo {
    /// Permission and backend support only. Not current occupancy.
    pub available: bool,
}

/// Effective file-tool eligibility. Nested so later capability fields
/// can be additive without renaming `available`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileExecutionInfo {
    pub read: FileOperationInfo,
    pub find: FileOperationInfo,
    pub patch: FileOperationInfo,
}

/// Static eligibility of a managed process in this workspace.
///
/// `available` is `permissions.exec && environment.exec_supported`.
/// It does not include transient workspace occupancy; `exec_command` may
/// still return `WORKSPACE_BUSY`. Occupancy rules are in `serialization`.
/// Tool existence is reported separately by `tools_exposed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessExecutionInfo {
    /// Permission and backend support only. Not current occupancy.
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ProcessCapabilityInfo>,
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
    pub files: FileExecutionInfo,
    pub process: ProcessExecutionInfo,
    pub serialization: WorkspaceSerializationInfo,
    pub isolation: IsolationInfo,
    pub network: NetworkInfo,
}

impl WorkspaceExecutionInfo {
    /// Assemble the advertised contract from already-evaluated axes.
    /// `files.*.available` and `process.available` are permission and
    /// backend support, not occupancy.
    pub fn from_effective(
        environment: EnvironmentExecutionInfo,
        permissions: EffectivePermissionInfo,
        network_policy: NetworkPolicyState,
    ) -> Self {
        let files = FileExecutionInfo {
            read: FileOperationInfo {
                available: permissions.read && environment.file_read_supported,
            },
            find: FileOperationInfo {
                available: permissions.read && environment.file_read_supported,
            },
            patch: FileOperationInfo {
                available: permissions.write && environment.file_write_supported,
            },
        };
        let process_available = permissions.exec && environment.exec_supported;
        let process = ProcessExecutionInfo {
            available: process_available,
            capabilities: process_available.then_some(ProcessCapabilityInfo {
                handles: true,
                survives_request_end: true,
                write_stdin: true,
                incremental_read: true,
                terminate: true,
                output_combined: true,
                tty: PtyCapabilityInfo {
                    supported: true,
                    default: false,
                    initial_rows: PTY_INITIAL_ROWS,
                    initial_cols: PTY_INITIAL_COLS,
                    resize_supported: false,
                },
            }),
        };
        Self {
            environment,
            permissions,
            files,
            process,
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
                policy: network_policy,
                enforcement: NetworkEnforcementState::None,
                client_may_escalate: false,
            },
        }
    }

    /// Overlay after a successful Linux helper probe. Restricted network
    /// advertises OS enforcement; Enabled stays unenforced in this WP.
    pub fn with_linux_command_sandbox(mut self, network_restricted: bool) -> Self {
        self.isolation.command_sandbox = CommandSandboxState::LinuxSandbox;
        if network_restricted {
            self.network.enforcement = NetworkEnforcementState::Enforced;
            self.network.client_may_escalate = false;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(
        kind: ClientEnvironmentKind,
        exec: bool,
        file_read: bool,
        file_write: bool,
    ) -> EnvironmentExecutionInfo {
        EnvironmentExecutionInfo {
            kind,
            client_selectable: false,
            exec_supported: exec,
            file_read_supported: file_read,
            file_write_supported: file_write,
        }
    }

    fn compose(
        kind: ClientEnvironmentKind,
        backend: bool,
        permissions: EffectivePermissionInfo,
    ) -> WorkspaceExecutionInfo {
        WorkspaceExecutionInfo::from_effective(
            environment(kind, backend, backend, backend),
            permissions,
            NetworkPolicyState::Restricted,
        )
    }

    fn assert_process_unavailable(exec: &WorkspaceExecutionInfo) {
        assert!(!exec.process.available);
        assert!(exec.process.capabilities.is_none());
        let json = serde_json::to_value(exec).unwrap();
        assert_eq!(json["process"]["available"], false);
        assert!(json["process"].get("capabilities").is_none());
    }

    fn assert_files(exec: &WorkspaceExecutionInfo, read: bool, find: bool, patch: bool) {
        assert_eq!(exec.files.read.available, read);
        assert_eq!(exec.files.find.available, find);
        assert_eq!(exec.files.patch.available, patch);
        let json = serde_json::to_value(exec).unwrap();
        assert_eq!(json["files"]["read"]["available"], read);
        assert_eq!(json["files"]["find"]["available"], find);
        assert_eq!(json["files"]["patch"]["available"], patch);
    }

    #[test]
    fn host_workspace_write_advertises_exec_and_fixed_pty() {
        let exec = compose(
            ClientEnvironmentKind::Host,
            true,
            EffectivePermissionInfo {
                read: true,
                write: true,
                exec: true,
            },
        );
        assert_eq!(exec.environment.kind, ClientEnvironmentKind::Host);
        assert!(!exec.environment.client_selectable);
        assert!(exec.environment.exec_supported);
        assert!(exec.environment.file_read_supported);
        assert!(exec.environment.file_write_supported);
        assert_files(&exec, true, true, true);
        assert!(exec.process.available);
        let caps = exec.process.capabilities.as_ref().expect("capabilities");
        assert!(caps.tty.supported);
        assert!(!caps.tty.default);
        assert_eq!(caps.tty.initial_rows, 24);
        assert_eq!(caps.tty.initial_cols, 80);
        assert!(!caps.tty.resize_supported);
        assert!(caps.output_combined);
        assert_eq!(exec.isolation.command_sandbox, CommandSandboxState::None);
        assert_eq!(exec.network.policy, NetworkPolicyState::Restricted);
        assert_eq!(exec.network.enforcement, NetworkEnforcementState::None);
        assert!(!exec.network.client_may_escalate);
        assert_eq!(exec.serialization.conflict_error, ErrorCode::WorkspaceBusy);
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["serialization"]["conflict_error"], "WORKSPACE_BUSY");
        assert!(json.get("environment_id").is_none());
        assert_eq!(json["environment"]["kind"], "host");
        assert_eq!(json["environment"]["file_read_supported"], true);
        assert_eq!(json["environment"]["file_write_supported"], true);
        assert!(json["environment"].get("patch_supported").is_none());
        assert_eq!(json["files"]["read"]["available"], true);
        assert_eq!(json["files"]["patch"]["available"], true);
        assert_eq!(json["isolation"]["command_sandbox"], "none");
        assert_eq!(json["network"]["enforcement"], "none");
        assert_eq!(json["process"]["available"], true);
        assert_eq!(json["process"]["capabilities"]["tty"]["supported"], true);
    }

    #[test]
    fn linux_sandbox_overlay_enforces_restricted_network() {
        let exec = compose(
            ClientEnvironmentKind::Host,
            true,
            EffectivePermissionInfo {
                read: true,
                write: true,
                exec: true,
            },
        )
        .with_linux_command_sandbox(true);
        assert_eq!(
            exec.isolation.command_sandbox,
            CommandSandboxState::LinuxSandbox
        );
        assert_eq!(exec.network.enforcement, NetworkEnforcementState::Enforced);
        assert!(!exec.network.client_may_escalate);
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["isolation"]["command_sandbox"], "linux-sandbox");
        assert_eq!(json["network"]["enforcement"], "enforced");
    }

    #[test]
    fn host_read_only_denies_write_and_exec() {
        let exec = compose(
            ClientEnvironmentKind::Host,
            true,
            EffectivePermissionInfo {
                read: true,
                write: false,
                exec: false,
            },
        );
        assert!(exec.environment.exec_supported);
        assert!(exec.environment.file_read_supported);
        assert!(exec.environment.file_write_supported);
        assert_files(&exec, true, true, false);
        assert_process_unavailable(&exec);
    }

    #[test]
    fn linux_container_write_permits_exec_policy_but_not_backend() {
        let exec = compose(
            ClientEnvironmentKind::LinuxContainer,
            false,
            EffectivePermissionInfo {
                read: true,
                write: true,
                exec: true,
            },
        );
        assert!(exec.permissions.exec);
        assert!(!exec.environment.exec_supported);
        assert!(!exec.environment.file_read_supported);
        assert!(!exec.environment.file_write_supported);
        assert_eq!(exec.environment.kind, ClientEnvironmentKind::LinuxContainer);
        assert_files(&exec, false, false, false);
        assert_process_unavailable(&exec);
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["environment"]["kind"], "linux-container");
        assert!(json.get("environment_id").is_none());
        assert!(!json.to_string().contains("\"environment_id\""));
    }

    #[test]
    fn linux_container_read_only_is_unavailable() {
        let exec = compose(
            ClientEnvironmentKind::LinuxContainer,
            false,
            EffectivePermissionInfo {
                read: true,
                write: false,
                exec: false,
            },
        );
        assert!(!exec.permissions.exec);
        assert!(exec.permissions.read);
        assert!(!exec.environment.exec_supported);
        assert!(!exec.environment.file_read_supported);
        assert!(!exec.environment.file_write_supported);
        assert_files(&exec, false, false, false);
        assert_process_unavailable(&exec);
    }
}
