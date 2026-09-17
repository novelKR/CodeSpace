//! Execution-plane DTOs. Not MCP schemas: no `work_id`, coordination, or
//! operation persistence. JsonSchema/rmcp stay in `crates/domain`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use codespace_domain::{FileChange, PatchStatus, ProcessId, Profile};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerExecRequest {
    pub argv: Vec<String>,
    pub process_id: ProcessId,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub output_bytes_cap: u64,
    pub tty: bool,
    pub policy: RunnerExecPolicy,
}

impl RunnerExecRequest {
    pub fn for_host(
        argv: Vec<String>,
        process_id: ProcessId,
        cwd: PathBuf,
        profile: Profile,
    ) -> Self {
        Self {
            argv,
            process_id,
            env: default_host_exec_env(&cwd),
            cwd,
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

pub fn default_host_exec_env(cwd: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
    );
    env.insert("HOME".into(), cwd.display().to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn exec_request_has_no_work_id_field() {
        let req = RunnerExecRequest::for_host(
            vec!["/bin/echo".into()],
            ProcessId("proc-1".into()),
            PathBuf::from("/tmp/ws"),
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
        assert_eq!(cwd, PathBuf::from("/tmp/ws"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("C"));
        assert!(timeout_ms > 0);
        assert_eq!(output_bytes_cap, MAX_OUTPUT_BYTES as u64);
        assert!(!tty);
        assert_eq!(policy.workspace_profile, Profile::WorkspaceWrite);
        assert_eq!(policy.network, NetworkAxis::Restricted);
        let json = serde_json::to_value(RunnerExecRequest::for_host(
            vec!["/bin/echo".into()],
            ProcessId("proc-1".into()),
            PathBuf::from("/tmp/ws"),
            Profile::WorkspaceWrite,
        ))
        .unwrap();
        assert!(json.get("work_id").is_none());
        assert!(json.get("operation_id").is_none());
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
