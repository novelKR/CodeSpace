//! Execution-plane DTOs. Not MCP schemas: no `work_id`, coordination, or
//! operation persistence. JsonSchema/rmcp stay in `crates/domain`.

use std::collections::BTreeMap;

use codespace_domain::{FileChange, PatchStatus, ProcessId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerExecRequest {
    pub argv: Vec<String>,
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerExecResult {
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerWriteStdin {
    pub process_id: ProcessId,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerReadProcess {
    pub process_id: ProcessId,
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerReadResult {
    pub process_id: ProcessId,
    pub cursor: u64,
    pub chunk: String,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerApplyPatchRequest {
    pub patch: String,
    pub expected_versions: BTreeMap<String, String>,
    pub check_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerApplyPatchResult {
    pub status: PatchStatus,
    pub files: Vec<String>,
    pub changes: Vec<FileChange>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_request_has_no_work_id_field() {
        let req = RunnerExecRequest {
            argv: vec!["/bin/echo".into()],
            process_id: ProcessId("proc-1".into()),
        };
        let RunnerExecRequest { argv, process_id } = req;
        assert_eq!(argv, ["/bin/echo"]);
        assert_eq!(process_id.0, "proc-1");
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
