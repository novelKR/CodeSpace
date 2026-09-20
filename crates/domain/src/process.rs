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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecDispatchStatus {
    /// Backend confirmed the spawn request result. The process may already have exited.
    Confirmed,
    /// Spawn may have occurred. Do not start a duplicate process. The
    /// process_id identifies the uncertain attempt; inspect or terminate
    /// only when the backend remains reachable.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecCommandResult {
    pub process_id: ProcessId,
    pub dispatch_status: ExecDispatchStatus,
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
    /// True when the retained window does not start at byte 0.
    pub output_lost: bool,
    /// First retained process-output offset (`dropped` in the runner).
    pub retained_from: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessTermination {
    Exited,
    Timeout,
    Terminated,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessStatusParams {
    pub process_id: ProcessId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessStatusResult {
    pub process_id: ProcessId,
    pub state: ProcessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination: Option<ProcessTermination>,
    pub output_total: u64,
    pub output_retained_from: u64,
    pub eof: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

impl ProcessStatusResult {
    pub fn invariants_hold(&self) -> bool {
        match self.state {
            ProcessState::Running => self.exit_code.is_none() && self.termination.is_none(),
            ProcessState::Exited => match self.termination {
                Some(ProcessTermination::Exited) => true,
                Some(_) => self.exit_code.is_none(),
                None => false,
            },
        }
    }
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
        assert!(
            !dumped.contains("tty_size"),
            "exec_command params must not include tty_size: {dumped}"
        );
    }

    #[test]
    fn exec_command_result_always_serializes_dispatch_status() {
        let json = serde_json::to_value(ExecCommandResult {
            process_id: ProcessId("proc-1".into()),
            dispatch_status: ExecDispatchStatus::Confirmed,
            coordination: None,
        })
        .unwrap();
        assert_eq!(json["dispatch_status"], "confirmed");
        let unknown = serde_json::to_value(ExecCommandResult {
            process_id: ProcessId("proc-1".into()),
            dispatch_status: ExecDispatchStatus::Unknown,
            coordination: None,
        })
        .unwrap();
        assert_eq!(unknown["dispatch_status"], "unknown");
        let schema = serde_json::to_value(schemars::schema_for!(ExecCommandResult)).unwrap();
        assert!(
            schema.to_string().contains("dispatch_status"),
            "result schema must include dispatch_status: {schema}"
        );
    }

    #[test]
    fn running_status_omits_exit_code() {
        let json = serde_json::to_value(ProcessStatusResult {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Running,
            exit_code: None,
            termination: None,
            output_total: 0,
            output_retained_from: 0,
            eof: false,
            coordination: None,
        })
        .unwrap();
        assert_eq!(json["state"], "running");
        assert!(json.get("exit_code").is_none());
        assert!(json.get("termination").is_none());
        assert!(ProcessStatusResult {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Running,
            exit_code: None,
            termination: None,
            output_total: 0,
            output_retained_from: 0,
            eof: false,
            coordination: None,
        }
        .invariants_hold());
        assert!(!ProcessStatusResult {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Running,
            exit_code: Some(0),
            termination: None,
            output_total: 0,
            output_retained_from: 0,
            eof: false,
            coordination: None,
        }
        .invariants_hold());
        let schema = serde_json::to_value(schemars::schema_for!(ProcessStatusResult)).unwrap();
        let dumped = schema.to_string();
        assert!(dumped.contains("output_total"), "{dumped}");
        assert!(dumped.contains("termination"), "{dumped}");
    }

    #[test]
    fn exited_success_may_include_exit_code() {
        let json = serde_json::to_value(ProcessStatusResult {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Exited,
            exit_code: Some(0),
            termination: Some(ProcessTermination::Exited),
            output_total: 12,
            output_retained_from: 0,
            eof: true,
            coordination: None,
        })
        .unwrap();
        assert_eq!(json["state"], "exited");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["termination"], "exited");
    }

    #[test]
    fn timeout_status_has_no_exit_code() {
        let json = serde_json::to_value(ProcessStatusResult {
            process_id: ProcessId("proc-1".into()),
            state: ProcessState::Exited,
            exit_code: None,
            termination: Some(ProcessTermination::Timeout),
            output_total: 0,
            output_retained_from: 0,
            eof: true,
            coordination: None,
        })
        .unwrap();
        assert_eq!(json["termination"], "timeout");
        assert!(json.get("exit_code").is_none());
    }

    #[test]
    fn read_process_result_exposes_output_loss() {
        let json = serde_json::to_value(ReadProcessResult {
            process_id: ProcessId("proc-1".into()),
            cursor: 10,
            chunk: String::new(),
            eof: false,
            output_lost: true,
            retained_from: 8,
            coordination: None,
        })
        .unwrap();
        assert_eq!(json["output_lost"], true);
        assert_eq!(json["retained_from"], 8);
        let schema = serde_json::to_value(schemars::schema_for!(ReadProcessResult)).unwrap();
        let dumped = schema.to_string();
        assert!(dumped.contains("output_lost"), "{dumped}");
        assert!(dumped.contains("retained_from"), "{dumped}");
    }

    #[test]
    fn process_status_params_are_process_id_only() {
        let schema = serde_json::to_value(schemars::schema_for!(ProcessStatusParams)).unwrap();
        let dumped = schema.to_string();
        assert!(dumped.contains("process_id"), "{dumped}");
        assert!(!dumped.contains("tty_size"), "{dumped}");
        assert!(!dumped.contains("signal"), "{dumped}");
    }
}
