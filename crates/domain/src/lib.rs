//! Domain types for CodeSpace. No `rmcp` dependency.

pub mod approval;
pub mod error;
pub mod execution;
pub mod files;
pub mod ids;
pub mod info;
pub mod intent;
pub mod patch;
pub mod process;
pub mod profile;
pub mod tools;
pub mod work;

pub use approval::{
    ApprovalCreateParams, ApprovalCreateResult, ApprovalDecision, ApprovalResolveParams,
    ApprovalResolveResult, ApprovalState, ApprovalTargetTool, ApprovalsMode, OperationResumeParams,
    OperationResumeResult,
};
pub use error::{
    classify_http_status, ErrorBody, ErrorCode, FailureClass, TRANSPORT_FAILURE_IS_NOT_OPERATION,
};
pub use execution::{
    ClientEnvironmentKind, CommandSandboxState, EffectivePermissionInfo, EnvironmentExecutionInfo,
    FileExecutionInfo, FileOperationInfo, IsolationInfo, NetworkEnforcementState, NetworkInfo,
    NetworkPolicyState, ProcessCapabilityInfo, ProcessExecutionInfo, PtyCapabilityInfo,
    WorkspaceExecutionInfo, WorkspaceSerializationInfo, PTY_INITIAL_COLS, PTY_INITIAL_ROWS,
};
pub use files::{FindParams, FindResult, ReadParams, ReadResult};
pub use ids::{ApprovalId, IntentId, OperationId, OperationKey, ProcessId, WorkId, WorkspaceId};
pub use info::{workspace_info, WorkspaceInfo, WorkspaceInfoParams};
pub use intent::{DeliveryPolicy, IntentKind, IntentState, UserIntent};
pub use patch::{
    ApplyPatchParams, ApplyPatchResult, FileChange, FileChangeKind, OperationEvent,
    OperationEventName, OperationKind, OperationStatusParams, OperationStatusResult, PatchStatus,
};
pub use process::{
    ExecCommandParams, ExecCommandResult, ExecDispatchStatus, ProcessState, ProcessStatusParams,
    ProcessStatusResult, ProcessTermination, ReadProcessParams, ReadProcessResult,
    TerminateProcessParams, WriteStdinParams,
};
pub use profile::Profile;
pub use tools::{
    LIVE_TOOLS, SERVER_NAME, SERVER_VERSION, TOOL_APPLY_PATCH, TOOL_APPROVAL_CREATE,
    TOOL_APPROVAL_RESOLVE, TOOL_EXEC_COMMAND, TOOL_FIND, TOOL_OPERATION_RESUME,
    TOOL_OPERATION_STATUS, TOOL_PROCESS_STATUS, TOOL_READ, TOOL_READ_PROCESS,
    TOOL_STEER_CLAIM_NEXT, TOOL_STEER_COMPLETE, TOOL_STEER_STATUS, TOOL_TERMINATE_PROCESS,
    TOOL_WORKSPACE_INFO, TOOL_WORK_FINISH, TOOL_WORK_OPEN, TOOL_WRITE_STDIN, TRANSPORT_STDIO,
    TRANSPORT_STREAMABLE_HTTP, W03_EXPOSED_TOOLS,
};
pub use work::{
    ClaimedIntent, CoordinationHint, SteerClaimNextResult, SteerCompleteParams, SteerOutcome,
    SteerStatusResult, Work, WorkFinishResult, WorkIdParams, WorkOpenParams, WorkOpenResult,
    WorkState, FINISH_REASON_PENDING,
};
