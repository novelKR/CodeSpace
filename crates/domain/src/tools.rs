pub const SERVER_NAME: &str = "codespace";
pub const SERVER_VERSION: &str = "0.6.0";

pub const TOOL_WORKSPACE_INFO: &str = "workspace_info";
pub const TOOL_READ: &str = "read";
pub const TOOL_FIND: &str = "find";
pub const TOOL_APPLY_PATCH: &str = "apply_patch";
pub const TOOL_EXEC_COMMAND: &str = "exec_command";
pub const TOOL_WRITE_STDIN: &str = "write_stdin";
pub const TOOL_READ_PROCESS: &str = "read_process";
pub const TOOL_PROCESS_STATUS: &str = "process_status";
pub const TOOL_PROCESS_RESIZE: &str = "process_resize";
pub const TOOL_TERMINATE_PROCESS: &str = "terminate_process";
pub const TOOL_OPERATION_STATUS: &str = "operation_status";
pub const TOOL_WORK_OPEN: &str = "work_open";
pub const TOOL_STEER_STATUS: &str = "steer_status";
pub const TOOL_STEER_CLAIM_NEXT: &str = "steer_claim_next";
pub const TOOL_STEER_COMPLETE: &str = "steer_complete";
pub const TOOL_WORK_FINISH: &str = "work_finish";
pub const TOOL_APPROVAL_CREATE: &str = "approval_create";
pub const TOOL_APPROVAL_RESOLVE: &str = "approval_resolve";
pub const TOOL_OPERATION_RESUME: &str = "operation_resume";

pub const TRANSPORT_STDIO: &str = "stdio";
pub const TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";

/// Tools registered in this release. Contract tests require `tools/list` to
/// match this list.
pub const LIVE_TOOLS: &[&str] = &[
    TOOL_WORKSPACE_INFO,
    TOOL_READ,
    TOOL_FIND,
    TOOL_APPLY_PATCH,
    TOOL_OPERATION_STATUS,
    TOOL_EXEC_COMMAND,
    TOOL_WRITE_STDIN,
    TOOL_READ_PROCESS,
    TOOL_PROCESS_STATUS,
    TOOL_PROCESS_RESIZE,
    TOOL_TERMINATE_PROCESS,
    TOOL_WORK_OPEN,
    TOOL_STEER_STATUS,
    TOOL_STEER_CLAIM_NEXT,
    TOOL_STEER_COMPLETE,
    TOOL_WORK_FINISH,
    TOOL_APPROVAL_CREATE,
    TOOL_APPROVAL_RESOLVE,
    TOOL_OPERATION_RESUME,
];
pub const W03_EXPOSED_TOOLS: &[&str] = LIVE_TOOLS;

pub const MVP_TOOL_CATALOG: &[&str] = &[
    TOOL_WORKSPACE_INFO,
    TOOL_READ,
    TOOL_FIND,
    TOOL_APPLY_PATCH,
    TOOL_EXEC_COMMAND,
    TOOL_WRITE_STDIN,
    TOOL_READ_PROCESS,
    TOOL_TERMINATE_PROCESS,
    TOOL_OPERATION_STATUS,
];
