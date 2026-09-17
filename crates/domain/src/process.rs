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
    /// Omit or false uses pipes. True attaches a PTY (default size 24x80).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[schemars(default)]
    pub tty: bool,
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
        let omitted = serde_json::from_value::<ExecCommandParams>(serde_json::json!({
            "workspace_id": "demo",
            "command": ["/bin/echo"]
        }))
        .unwrap();
        assert!(!omitted.tty);
        assert!(omitted.work_id.is_none());

        let json = serde_json::to_value(ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into()],
            work_id: None,
            tty: false,
        })
        .unwrap();
        assert!(json.get("environment_id").is_none());
        assert!(json.get("cwd").is_none());
        assert!(json.get("tty").is_none());

        let with_tty = serde_json::to_value(ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into()],
            work_id: None,
            tty: true,
        })
        .unwrap();
        assert_eq!(with_tty["tty"], true);
        assert!(with_tty.get("environment_id").is_none());
        assert!(with_tty.get("cwd").is_none());

        let schema = serde_json::to_value(schemars::schema_for!(ExecCommandParams)).unwrap();
        let dumped = schema.to_string();
        assert!(
            dumped.contains("\"tty\""),
            "exec_command schema must include tty: {dumped}"
        );
        assert!(
            !dumped.contains("environment_id"),
            "exec_command schema must not include environment_id: {dumped}"
        );
        assert!(
            !dumped.contains("\"cwd\""),
            "exec_command schema must not include cwd: {dumped}"
        );
    }
}
