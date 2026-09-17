use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{ProcessId, WorkId, WorkspaceId};
use crate::work::CoordinationHint;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecCommandParams {
    pub workspace_id: WorkspaceId,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecCommandResult {
    pub process_id: ProcessId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WriteStdinParams {
    pub process_id: ProcessId,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadProcessParams {
    pub process_id: ProcessId,
    #[serde(default)]
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadProcessResult {
    pub process_id: ProcessId,
    pub cursor: u64,
    pub chunk: String,
    pub eof: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminateProcessParams {
    pub process_id: ProcessId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_command_params_have_no_environment_id() {
        let json = serde_json::to_value(ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into()],
            work_id: None,
        })
        .unwrap();
        assert!(json.get("environment_id").is_none());
        assert!(json.get("cwd").is_none());
        assert!(json.get("tty").is_none());
    }
}
