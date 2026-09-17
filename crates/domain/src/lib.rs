//! Domain types for CodeSpace. No `rmcp` dependency.

pub mod error;
pub mod files;
pub mod ids;
pub mod info;
pub mod patch;
pub mod process;
pub mod profile;
pub mod tools;

pub use error::{
    classify_http_status, ErrorBody, ErrorCode, FailureClass, TRANSPORT_FAILURE_IS_NOT_OPERATION,
};
pub use files::{FindParams, FindResult, ReadParams, ReadResult};
pub use ids::{OperationId, OperationKey, ProcessId, WorkspaceId};
pub use info::{workspace_info, WorkspaceInfo, WorkspaceInfoParams};
pub use patch::{
    ApplyPatchParams, ApplyPatchResult, OperationStatusParams, OperationStatusResult, PatchStatus,
};
pub use process::{
    ExecCommandParams, ExecCommandResult, ReadProcessParams, ReadProcessResult,
    TerminateProcessParams, TerminateProcessResult, WriteStdinParams, WriteStdinResult,
};
pub use profile::Profile;
pub use tools::{
    LIVE_TOOLS, SERVER_NAME, SERVER_VERSION, TOOL_APPLY_PATCH, TOOL_EXEC_COMMAND, TOOL_FIND,
    TOOL_OPERATION_STATUS, TOOL_READ, TOOL_READ_PROCESS, TOOL_TERMINATE_PROCESS,
    TOOL_WORKSPACE_INFO, TOOL_WRITE_STDIN, TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP,
    W03_EXPOSED_TOOLS,
};
