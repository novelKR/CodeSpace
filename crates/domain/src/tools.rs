pub const SERVER_NAME: &str = "codespace";
pub const SERVER_VERSION: &str = "0.3.0";

pub const TOOL_WORKSPACE_INFO: &str = "workspace_info";
pub const TOOL_READ: &str = "read";
pub const TOOL_FIND: &str = "find";
pub const TOOL_APPLY_PATCH: &str = "apply_patch";
pub const TOOL_EXEC_COMMAND: &str = "exec_command";
pub const TOOL_WRITE_STDIN: &str = "write_stdin";
pub const TOOL_READ_PROCESS: &str = "read_process";
pub const TOOL_TERMINATE_PROCESS: &str = "terminate_process";
pub const TOOL_OPERATION_STATUS: &str = "operation_status";

pub const TRANSPORT_STDIO: &str = "stdio";
pub const TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";

/// Tools registered in this release.
pub const LIVE_TOOLS: &[&str] = &[
    TOOL_WORKSPACE_INFO,
    TOOL_READ,
    TOOL_FIND,
    TOOL_APPLY_PATCH,
    TOOL_OPERATION_STATUS,
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
