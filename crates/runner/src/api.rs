//! Execution-plane DTOs. Not MCP schemas: no `work_id`, coordination, or
//! operation persistence. JsonSchema/rmcp stay in `crates/domain`.

use std::collections::BTreeMap;
use std::path::Path;

use codespace_domain::{
    ErrorBody, ErrorCode, FileChange, PatchStatus, ProcessId, ProcessState, ProcessTermination,
    Profile,
};
use codespace_policy::NetworkAxis;
use serde::{Deserialize, Serialize};

pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Gateway-filled exec policy summary. Not an allow engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerExecPolicy {
    pub workspace_profile: Profile,
    pub network: NetworkAxis,
}

/// Working directory for exec. The runner resolves this against its local
/// workspace root. Host absolute paths are not part of the wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerCwd {
    WorkspaceRoot,
}

/// Exec environment. `PATH` / `HOME` / `LANG` come from the **runner
/// process** when `use_runner_defaults` is set. The gateway does not
/// serialize its own `PATH` or host absolute cwd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerExecEnv {
    pub use_runner_defaults: bool,
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerExecRequest {
    pub argv: Vec<String>,
    pub process_id: ProcessId,
    pub cwd: RunnerCwd,
    pub env: RunnerExecEnv,
    pub timeout_ms: u64,
    pub output_bytes_cap: u64,
    pub tty: bool,
    pub policy: RunnerExecPolicy,
}

impl RunnerExecRequest {
    pub fn for_host(argv: Vec<String>, process_id: ProcessId, profile: Profile) -> Self {
        Self {
            argv,
            process_id,
            cwd: RunnerCwd::WorkspaceRoot,
            env: RunnerExecEnv {
                use_runner_defaults: true,
                overrides: BTreeMap::new(),
            },
            timeout_ms: default_exec_timeout_ms(),
            output_bytes_cap: MAX_OUTPUT_BYTES as u64,
            tty: false,
            policy: RunnerExecPolicy {
                workspace_profile: profile,
                network: NetworkAxis::Restricted,
            },
        }
    }
}

/// Runner-local defaults applied after `env_clear`. Not a DTO field.
pub fn runner_local_exec_env(home: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
    );
    env.insert("HOME".into(), home.display().to_string());
    env.insert("LANG".into(), "C".into());
    env
}

pub fn default_exec_timeout_ms() -> u64 {
    std::env::var("CODESPACE_PROCESS_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(|secs| secs.saturating_mul(1000))
        .unwrap_or(DEFAULT_TIMEOUT_MS)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerExecResult {
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerWriteStdin {
    pub process_id: ProcessId,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerReadProcess {
    pub process_id: ProcessId,
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerReadResult {
    pub process_id: ProcessId,
    pub cursor: u64,
    pub chunk: String,
    pub eof: bool,
    #[serde(default)]
    pub output_lost: bool,
    #[serde(default)]
    pub retained_from: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerProcessStatus {
    pub process_id: ProcessId,
    pub state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<ProcessTermination>,
    pub output_total: u64,
    pub output_retained_from: u64,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerResizeResult {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerApplyPatchRequest {
    pub patch: String,
    pub expected_versions: BTreeMap<String, String>,
    pub check_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerApplyPatchResult {
    pub status: PatchStatus,
    pub files: Vec<String>,
    pub changes: Vec<FileChange>,
}

/// Runner-trait errors. Not MCP `ErrorBody` until the gateway maps them.
/// No new public `ErrorCode` is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// Disk/exec engine refused the request.
    Execution(ErrorBody),
    /// The request never left this process. Disk is unchanged.
    TransportBeforeDispatch { message: String },
    /// The request may have run. Result cannot be confirmed.
    TransportAmbiguous { message: String },
}

impl RunnerError {
    pub fn execution(body: ErrorBody) -> Self {
        Self::Execution(body)
    }

    pub fn before_dispatch(message: impl Into<String>) -> Self {
        Self::TransportBeforeDispatch {
            message: message.into(),
        }
    }

    pub fn ambiguous(message: impl Into<String>) -> Self {
        Self::TransportAmbiguous {
            message: message.into(),
        }
    }

    pub fn as_execution(&self) -> Option<&ErrorBody> {
        match self {
            Self::Execution(body) => Some(body),
            _ => None,
        }
    }

    /// Gateway mapping onto the frozen MCP error catalog.
    pub fn into_error_body(self) -> ErrorBody {
        match self {
            Self::Execution(body) => body,
            Self::TransportBeforeDispatch { message } => ErrorBody::new(
                ErrorCode::Timeout,
                format!("runner transport failed before dispatch: {message}"),
            ),
            Self::TransportAmbiguous { message } => ErrorBody::new(
                ErrorCode::Timeout,
                format!("runner transport result is ambiguous: {message}"),
            ),
        }
    }
}

impl From<ErrorBody> for RunnerError {
    fn from(body: ErrorBody) -> Self {
        Self::Execution(body)
    }
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Execution(body) => write!(f, "{}", body.message),
            Self::TransportBeforeDispatch { message } => {
                write!(f, "runner transport failed before dispatch: {message}")
            }
            Self::TransportAmbiguous { message } => {
                write!(f, "runner transport result is ambiguous: {message}")
            }
        }
    }
}

impl std::error::Error for RunnerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_request_has_no_work_id_field() {
        let req = RunnerExecRequest::for_host(
            vec!["/bin/echo".into()],
            ProcessId("proc-1".into()),
            Profile::WorkspaceWrite,
        );
        let RunnerExecRequest {
            argv,
            process_id,
            cwd,
            env,
            timeout_ms,
            output_bytes_cap,
            tty,
            policy,
        } = req;
        assert_eq!(argv, ["/bin/echo"]);
        assert_eq!(process_id.0, "proc-1");
        assert_eq!(cwd, RunnerCwd::WorkspaceRoot);
        assert!(env.use_runner_defaults);
        assert!(env.overrides.is_empty());
        assert!(timeout_ms > 0);
        assert_eq!(output_bytes_cap, MAX_OUTPUT_BYTES as u64);
        assert!(!tty);
        assert_eq!(policy.workspace_profile, Profile::WorkspaceWrite);
        assert_eq!(policy.network, NetworkAxis::Restricted);
        let json = serde_json::to_value(RunnerExecRequest::for_host(
            vec!["/bin/echo".into()],
            ProcessId("proc-1".into()),
            Profile::WorkspaceWrite,
        ))
        .unwrap();
        assert!(json.get("work_id").is_none());
        assert!(json.get("operation_id").is_none());
        assert_eq!(json["cwd"], "workspace_root");
        assert!(json["cwd"].as_str().is_some());
        assert!(json["env"].get("PATH").is_none());
        assert_eq!(json["env"]["use_runner_defaults"], true);
        assert_eq!(json["env"]["overrides"], serde_json::json!({}));
        let dumped = json.to_string();
        assert!(
            !dumped.contains("/tmp/") && !dumped.contains("/Users/"),
            "host absolute cwd must not be serialized: {dumped}"
        );
    }

    #[test]
    fn apply_result_has_no_operation_or_work_id() {
        let res = RunnerApplyPatchResult {
            status: PatchStatus::Applied,
            files: vec!["a.txt".into()],
            changes: vec![],
        };
        let RunnerApplyPatchResult {
            status,
            files,
            changes,
        } = res;
        assert_eq!(status, PatchStatus::Applied);
        assert_eq!(files, ["a.txt"]);
        assert!(changes.is_empty());
    }
}
