use std::borrow::Cow;
use std::sync::Arc;

use codespace_domain::{
    workspace_info, ApplyPatchParams, ApplyPatchResult, ApprovalCreateParams, ApprovalCreateResult,
    ApprovalId, ApprovalResolveParams, ApprovalResolveResult, ApprovalState, ApprovalTargetTool,
    ClientEnvironmentKind, CoordinationHint, EffectivePermissionInfo, EnvironmentExecutionInfo,
    ErrorBody, ErrorCode, ExecCommandParams, ExecCommandResult, ExecDispatchStatus, FindParams,
    FindResult, NetworkPolicyState, OperationResumeParams, OperationResumeResult,
    OperationStatusParams, OperationStatusResult, PatchStatus, ProcessId, ProcessResizeParams,
    ProcessResizeResult, ProcessStatusParams, ProcessStatusResult, ReadParams, ReadProcessParams,
    ReadProcessResult, ReadResult, SteerClaimNextResult, SteerCompleteParams, SteerStatusResult,
    TerminateProcessParams, WorkFinishResult, WorkId, WorkIdParams, WorkOpenParams, WorkOpenResult,
    WorkspaceExecutionInfo, WorkspaceInfo, WorkspaceInfoParams, WriteStdinParams,
};
use codespace_policy::{
    allow, Action, ClientClaims, EnvironmentKind, NetworkAxis, Participation, PermissionProfile,
    Registry, Workspace,
};
use codespace_runner::{
    linux_sandbox_available, Runner, RunnerApplyPatchRequest, RunnerError, RunnerExecRequest,
    RunnerReadProcess, RunnerWriteStdin, RuntimeBackend,
};
use codespace_store::{Begin, ResumeClaim, Store, StoredOperation};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Implementation, InitializeRequestParams, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData as McpError, Json, RoleServer, ServerHandler,
};
use uuid::Uuid;

use crate::protocol::{NegotiatedFeatures, CORE_BASELINE, SUPPORTED_PROTOCOL_VERSIONS};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Clone)]
pub struct CodeSpace {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    pub(crate) registry: Registry,
    pub(crate) store: Arc<Store>,
    pub(crate) runner: RuntimeBackend,
    /// The resource authority whose status `workspace_info` reports (CSRG-U1).
    #[cfg(feature = "devguard")]
    pub(crate) resource_authority: Option<Arc<crate::devguard::ResourceAuthority>>,
    /// The execution owner's registration `workspace_info` reports (CSRG-U2).
    #[cfg(feature = "devguard")]
    pub(crate) registration: Option<Arc<crate::devguard::Registration>>,
}

fn err_json(err: ErrorBody) -> String {
    serde_json::to_string(&err).unwrap_or(err.message)
}

const MCP_INSTRUCTIONS: &str = "\
CodeSpace is an execution-only MCP and never calls a model.

Use workspace-relative paths for file tools. workspace_id and work_id are \
selectors, not credentials.

read and find accept optional offset and limit. Omitted arguments return \
the first window: 1 MiB for read, 10000 sorted paths for find. Per-call \
caps are the same. truncated means more observed content remains after \
this window. Continue only when next_offset is present. Do not compute \
the next window from content.len() or retry an empty page at the same \
offset. Byte windows may split UTF-8 sequences; content_lossy=true means \
content contains replacement decoding and must not be used for exact \
reconstruction. find.incomplete=true means the walk was capped so the \
matching set is not known to be complete. listing_version identifies \
this observation's sorted path set; if it changes between pages, restart \
from offset 0. version hashes the whole file, not the window. A limit of \
0 or above the cap returns OUTPUT_LIMIT.

exec_command accepts argv; there is no implicit shell. It runs in the \
workspace cwd and returns a server-minted process_id. Ending an MCP request \
does not terminate the process.

A live managed process holds the workspace mutation lease. read and find may \
continue, but apply_patch or another exec_command returns WORKSPACE_BUSY \
until the process exits or is terminated. Request-owned patch or exec work \
on the same workspace waits in FIFO order until acquire(); a live process \
fails already-queued waiters with WORKSPACE_BUSY. Queue saturation returns \
RESOURCE_QUEUE_FULL.

exec_command.tty is optional and defaults to false. tty=true attaches a \
fixed 24x80 PTY. Use process_resize on a running PTY to change rows and \
cols. tty_size is not an exec_command argument. Use tty=true only \
when the command requires terminal semantics or an interactive TUI.

Executable workspaces currently use host execution. When \
workspace_info.execution.isolation.command_sandbox is linux-sandbox, \
exec_command is wrapped by the Linux helper. When it is none, host \
execution is not an OS command sandbox. Network policy is reported by \
workspace_info. OS network enforcement follows \
execution.network.enforcement; absence of enforcement is not permission. \
Enabled network uses a managed proxy; a missing Linux helper is not \
permission.

Treat apply_patch status=unknown as possibly executed. Do not blindly retry \
the mutation with a new operation_key.

When a workspace is configured with approvals=confirm, allowed apply_patch \
and exec_command calls return APPROVAL_REQUIRED instead of executing. That \
hold is a workflow pause, not a privilege grant or isolation boundary. The \
same MCP caller can grant it with approval_resolve. Then call \
operation_resume; resume re-checks policy.

If exec_command reports dispatch_status=unknown, the spawn may have occurred. \
Do not blindly start a duplicate process. The returned process_id identifies \
the uncertain attempt. Use read_process, process_status, process_resize, or \
terminate_process when the backend remains reachable; do not assume that \
unknown means the process did not start.

process_id is a lifecycle handle after spawn. exec_command returns dispatch \
identity only. Use process_status to observe state running or exited. \
termination is present only after exit and is one of exited, signaled, \
timeout, terminated, or unknown. exit_code is present only when termination \
is exited. signaled means a signal CodeSpace did not send ended the process, \
and signal then gives its number and, when known, its name. A tty process \
reports such a death as exited with exit_code 1. EOF from read_process is not \
a successful exit. signaled, timeout, terminated, and unknown are not success \
even when eof is true.

read_process results include output_lost and retained_from. If output_lost is \
true, the retained window is not the complete log.

Managed process lifetime is owned by the runner instance, not the MCP \
session. Client or HTTP disconnect keeps the process running. Losing the \
UDS worker connection or shutting down the gateway terminates the owned \
subtree and drops handles. Restart does not recover process_id. If you lose \
the spawn response before receiving process_id, do not search for the lost \
handle and do not start a duplicate command.

process_resize requires a running PTY-backed process. Pipe processes return \
PROCESS_NOT_TTY. An exited handle returns PROCESS_NOT_RUNNING. A missing \
handle returns PROCESS_NOT_FOUND.

tty_size is not an exec_command argument.

Claim user intents only at major checkpoints and before work_finish.";

#[derive(Debug, Clone, Serialize, JsonSchema)]
struct OkBody {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    coordination: Option<CoordinationHint>,
}

#[tool_router]
impl CodeSpace {
    pub fn new(registry: Registry) -> Self {
        Self::with_store(
            registry,
            Arc::new(Store::memory().expect("in-memory store")),
        )
    }

    pub fn with_store(registry: Registry, store: Arc<Store>) -> Self {
        let store_for_lease = store.clone();
        let on_release = Arc::new(move |process_id: &str| {
            store_for_lease.release_process(process_id);
        });
        Self::with_store_and_runner(registry, store, RuntimeBackend::in_process(on_release))
    }

    pub fn with_store_and_runner(
        registry: Registry,
        store: Arc<Store>,
        runner: RuntimeBackend,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
            store,
            runner,
            #[cfg(feature = "devguard")]
            resource_authority: None,
            #[cfg(feature = "devguard")]
            registration: None,
        }
    }

    #[tool(
        name = "workspace_info",
        description = "Return CodeSpace identity and, when workspace_id is set, the effective execution contract. files.*.available and process.available reflect permission and backend support only. They do not include transient workspace occupancy; exec_command or apply_patch may still return WORKSPACE_BUSY or RESOURCE_QUEUE_FULL. serialization reports request_conflict wait-fifo, process_conflict reject, and max_waiters_per_resource. Tool existence is reported separately by tools_exposed. Does not call a model. Does not read files. workspace_id is a selector, not a credential."
    )]
    async fn workspace_info(
        &self,
        Parameters(params): Parameters<WorkspaceInfoParams>,
    ) -> Result<Json<WorkspaceInfo>, String> {
        let info = lookup(&self.registry, params.workspace_id).map_err(err_json)?;
        #[cfg(feature = "devguard")]
        let info = self.with_resource_authority_status(info).await;
        Ok(Json(info))
    }

    #[tool(
        name = "read",
        description = "Read a relative workspace file and return content plus a sha256 version of the whole file. Optional offset and limit select a byte window (default 0 and 1 MiB, max 1 MiB per call). truncated means more bytes remain after this window; continue only at next_offset. Byte windows may split UTF-8 sequences. content_lossy=true means content contains replacement decoding and must not be used for exact reconstruction. Rejects symlinks, special files, and path escape."
    )]
    async fn read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<Json<ReadResult>, String> {
        let ws = self
            .registry
            .get(&params.workspace_id.0)
            .map_err(err_json)?;
        self.runner
            .read(ws, &params.path, params.offset, params.limit)
            .await
            .map(|mut result| {
                result.coordination = self.hint(&params.workspace_id.0, params.work_id.as_ref());
                Json(result)
            })
            .map_err(runner_err_json)
    }

    #[tool(
        name = "find",
        description = "List relative file paths in a workspace. Does not follow symlinks. Optional offset and limit page the sorted path list (default 0 and 10000, max 10000 per call). truncated means more observed matching paths remain; continue only at next_offset. incomplete=true means the walk was capped so the matching set is not known to be complete. listing_version identifies this observation's sorted path set. Do not retry an empty page at the same offset."
    )]
    async fn find(
        &self,
        Parameters(params): Parameters<FindParams>,
    ) -> Result<Json<FindResult>, String> {
        let ws = self
            .registry
            .get(&params.workspace_id.0)
            .map_err(err_json)?;
        self.runner
            .find(ws, params.glob.as_deref(), params.offset, params.limit)
            .await
            .map(|mut result| {
                result.coordination = self.hint(&params.workspace_id.0, params.work_id.as_ref());
                Json(result)
            })
            .map_err(runner_err_json)
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex V4A patch. check_only verifies without writing and returns status checked. status applied means disk hashes match the helper claim. Never falls back to git apply. status=unknown means the mutation may have executed but its result could not be confirmed. Do not retry the same mutation under a new operation_key. operation_key provides replay/idempotency for the same logical mutation. When the workspace approvals mode is confirm, a policy-allowed request returns APPROVAL_REQUIRED before begin() and does not write. A live process returns WORKSPACE_BUSY and fails waiters that were already queued. Overlapping request-owned patch or exec work on the same workspace waits in FIFO order starting at acquire(). Queue saturation returns RESOURCE_QUEUE_FULL."
    )]
    async fn apply_patch(
        &self,
        Parameters(params): Parameters<ApplyPatchParams>,
    ) -> Result<Json<ApplyPatchResult>, String> {
        let ws = self
            .registry
            .get(&params.workspace_id.0)
            .map_err(err_json)?;
        allow(ws, Action::Write, &ClientClaims::default()).map_err(err_json)?;
        ws.require_file_write().map_err(err_json)?;
        if let Err(err) = self.maybe_hold_patch(ws, &params) {
            return Err(err_json(err));
        }
        self.apply_patch_inner(params, None)
            .await
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "operation_status",
        description = "Look up a recorded patch by exactly one of operation_id or operation_key. Returns kind=patch, files/changes hashes, and minted/finished events. Does not re-run the operation or track exec_command. Distinct from HTTP request ids and process_id."
    )]
    async fn operation_status(
        &self,
        Parameters(params): Parameters<OperationStatusParams>,
    ) -> Result<Json<OperationStatusResult>, String> {
        self.store
            .status_lookup(params.operation_id.as_ref(), params.operation_key.as_ref())
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "exec_command",
        description = "Start a managed argv in the workspace cwd. There is no implicit shell. Returns a server-minted process_id and a dispatch_status. Request end does not terminate the process. Omitted or false tty uses pipes. tty=true attaches a 24x80 PTY; use process_resize to change the size of a running PTY. tty_size is not an exec_command argument. Use tty only for commands requiring terminal semantics or an interactive TUI. A live process holds the workspace mutation lease, so another exec_command or apply_patch returns WORKSPACE_BUSY until it exits or is terminated, including waiters that were already queued. Overlapping request-owned patch and exec work on the same workspace waits in FIFO order starting at acquire(). Queue saturation returns RESOURCE_QUEUE_FULL. Use write_stdin, read_process, process_status, process_resize, and terminate_process with the returned process_id. dispatch_status=unknown means the spawn may have occurred. Do not blindly start a duplicate process. The returned process_id identifies the uncertain attempt. Use read_process, process_status, process_resize, or terminate_process when the backend remains reachable; do not assume that unknown means the process did not start. PROCESS_SPAWN_FAILED means the backend confirmed that no managed process was started; it is distinct from dispatch_status=unknown. When the workspace approvals mode is confirm, a policy-allowed request returns APPROVAL_REQUIRED before spawn."
    )]
    async fn exec_command(
        &self,
        Parameters(params): Parameters<ExecCommandParams>,
    ) -> Result<Json<ExecCommandResult>, String> {
        let ws = self
            .registry
            .get(&params.workspace_id.0)
            .map_err(err_json)?;
        allow(ws, Action::Exec, &ClientClaims::default()).map_err(err_json)?;
        ws.require_exec().map_err(err_json)?;
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(err_json(invalid_argv()));
        }
        // A workspace this gateway cannot govern is refused before any approval or lease.
        self.participation(ws).await.map_err(err_json)?;
        if let Err(err) = self.maybe_hold_exec(ws, &params) {
            return Err(err_json(err));
        }
        self.exec_command_inner(params, None)
            .await
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "write_stdin",
        description = "Write to a managed process stdin. Unknown process_id is rejected."
    )]
    async fn write_stdin(
        &self,
        Parameters(params): Parameters<WriteStdinParams>,
    ) -> Result<Json<OkBody>, String> {
        self.runner
            .write_stdin(RunnerWriteStdin {
                process_id: params.process_id.clone(),
                data: params.data,
            })
            .await
            .map_err(runner_err_json)?;
        Ok(Json(OkBody {
            ok: true,
            coordination: self.process_hint(&params.process_id.0).await,
        }))
    }

    #[tool(
        name = "read_process",
        description = "Read output from a managed process starting at cursor. Output is bounded; output_lost means the retained window is not the complete log. process_id cannot be invented."
    )]
    async fn read_process(
        &self,
        Parameters(params): Parameters<ReadProcessParams>,
    ) -> Result<Json<ReadProcessResult>, String> {
        let result = self
            .runner
            .read_process(RunnerReadProcess {
                process_id: params.process_id.clone(),
                cursor: params.cursor,
            })
            .await
            .map_err(runner_err_json)?;
        Ok(Json(ReadProcessResult {
            process_id: result.process_id,
            cursor: result.cursor,
            chunk: result.chunk,
            eof: result.eof,
            output_lost: result.output_lost,
            retained_from: result.retained_from,
            coordination: self.process_hint(&params.process_id.0).await,
        }))
    }

    #[tool(
        name = "process_status",
        description = "Observe a managed process lifecycle. state is running or exited. termination is present only after exit (exited, signaled, timeout, terminated, or unknown). exit_code is present only when termination is exited; signal only when it is signaled, a signal CodeSpace did not send. A tty process reports a signal death as exited with exit_code 1. EOF is not success. Unknown process_id is rejected."
    )]
    async fn process_status(
        &self,
        Parameters(params): Parameters<ProcessStatusParams>,
    ) -> Result<Json<ProcessStatusResult>, String> {
        let result = self
            .runner
            .process_status(&params.process_id)
            .await
            .map_err(runner_err_json)?;
        Ok(Json(ProcessStatusResult {
            process_id: result.process_id,
            state: result.state,
            exit_code: result.exit_code,
            termination: result.termination,
            signal: result.signal,
            output_total: result.output_total,
            output_retained_from: result.output_retained_from,
            eof: result.eof,
            coordination: self.process_hint(&params.process_id.0).await,
        }))
    }

    #[tool(
        name = "process_resize",
        description = "Change the PTY size of a running managed process. Requires tty=true spawn. rows and cols must be at least 1. Pipe-backed processes return PROCESS_NOT_TTY. An exited handle returns PROCESS_NOT_RUNNING. Unknown process_id is rejected."
    )]
    async fn process_resize(
        &self,
        Parameters(params): Parameters<ProcessResizeParams>,
    ) -> Result<Json<ProcessResizeResult>, String> {
        let result = self
            .runner
            .resize(&params.process_id, params.rows, params.cols)
            .await
            .map_err(runner_err_json)?;
        Ok(Json(ProcessResizeResult {
            ok: true,
            rows: result.rows,
            cols: result.cols,
            coordination: self.process_hint(&params.process_id.0).await,
        }))
    }

    #[tool(
        name = "terminate_process",
        description = "Terminate a managed process. Only server-minted process_id values are accepted."
    )]
    async fn terminate_process(
        &self,
        Parameters(params): Parameters<TerminateProcessParams>,
    ) -> Result<Json<OkBody>, String> {
        self.runner
            .terminate(&params.process_id)
            .await
            .map_err(runner_err_json)?;
        Ok(Json(OkBody {
            ok: true,
            coordination: self.process_hint(&params.process_id.0).await,
        }))
    }

    #[tool(
        name = "work_open",
        description = "Open a server-minted work_id for deferred user intent. Does not call a model. work_id is a selector, not a credential. Ordinary tools/call; does not require MCP 2026-07-28."
    )]
    async fn work_open(
        &self,
        Parameters(params): Parameters<WorkOpenParams>,
    ) -> Result<Json<WorkOpenResult>, String> {
        self.registry
            .get(&params.workspace_id.0)
            .map_err(err_json)?;
        self.store
            .open_work(&params.workspace_id, params.title)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "steer_status",
        description = "Return queued/claimed counts for a work_id. Never returns intent bodies. Check at major checkpoints (after a patch, after a long process, before work_finish), not after every read."
    )]
    async fn steer_status(
        &self,
        Parameters(params): Parameters<WorkIdParams>,
    ) -> Result<Json<SteerStatusResult>, String> {
        self.store
            .steer_status(&params.work_id)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "steer_claim_next",
        description = "Atomically claim the next eligible queued user intent (one item). Drafts are never returned. Claimed content is frozen. Do not call after every tool; use after a major checkpoint and before work_finish."
    )]
    async fn steer_claim_next(
        &self,
        Parameters(params): Parameters<WorkIdParams>,
    ) -> Result<Json<SteerClaimNextResult>, String> {
        self.store
            .claim_next(&params.work_id)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "steer_complete",
        description = "Mark a claimed intent done or blocked. Does not grant permissions. Required before work_finish if any claimed items remain."
    )]
    async fn steer_complete(
        &self,
        Parameters(params): Parameters<SteerCompleteParams>,
    ) -> Result<Json<codespace_domain::UserIntent>, String> {
        self.store
            .complete_intent(&params.work_id, &params.intent_id, params.outcome)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "work_finish",
        description = "Attempt to close a work. If pending user input remains, closed is false and you must steer_claim_next. Call this before a final user-visible answer. Ordinary tools/call."
    )]
    async fn work_finish(
        &self,
        Parameters(params): Parameters<WorkIdParams>,
    ) -> Result<Json<WorkFinishResult>, String> {
        self.store
            .finish_work(&params.work_id)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "approval_create",
        description = "Create a pending confirmation hold for an already-allowed apply_patch or exec_command. Policy denial returns UNAUTHORIZED and creates no row. This does not grant write/exec rights and does not change the profile. The same MCP caller can later grant the hold."
    )]
    async fn approval_create(
        &self,
        Parameters(params): Parameters<ApprovalCreateParams>,
    ) -> Result<Json<ApprovalCreateResult>, String> {
        self.approval_create_inner(params)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "approval_resolve",
        description = "Grant or deny a pending confirmation hold. grant does not change the permission profile and is not an isolation boundary; the same MCP caller can grant. deny is terminal. Resume a granted hold with operation_resume."
    )]
    async fn approval_resolve(
        &self,
        Parameters(params): Parameters<ApprovalResolveParams>,
    ) -> Result<Json<ApprovalResolveResult>, String> {
        self.store
            .resolve_approval(&params.approval_id, params.decision)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "operation_resume",
        description = "Run a granted confirmation hold once after re-checking allow(). Repeating the same approval_id returns the stored terminal result, recovers a recorded patch from the operations ledger, or returns APPROVAL_AMBIGUOUS. This is not a privilege escalation."
    )]
    async fn operation_resume(
        &self,
        Parameters(params): Parameters<OperationResumeParams>,
    ) -> Result<Json<OperationResumeResult>, String> {
        self.operation_resume_inner(params)
            .await
            .map(Json)
            .map_err(err_json)
    }
}

impl CodeSpace {
    async fn apply_patch_inner(
        &self,
        params: ApplyPatchParams,
        resume: Option<&ApprovalId>,
    ) -> Result<ApplyPatchResult, ErrorBody> {
        let ws = self.registry.get(&params.workspace_id.0)?;
        codespace_policy::allow(ws, Action::Write, &ClientClaims::default())?;
        ws.require_file_write()?;
        let _lease = self.store.acquire_write(&params.workspace_id.0).await?;
        if let Some(id) = resume {
            self.store.mark_resuming(id)?;
        }
        let fingerprint = Store::fingerprint(&params);
        match self.store.begin(
            params.operation_key.as_ref(),
            &params.workspace_id.0,
            &fingerprint,
        )? {
            Begin::Replayed(stored) => Ok(self.with_hint(
                stored.result,
                &params.workspace_id.0,
                params.work_id.as_ref(),
            )),
            Begin::Fresh(operation_id) => {
                let result = match self
                    .runner
                    .apply_patch(
                        ws,
                        RunnerApplyPatchRequest {
                            patch: params.patch,
                            expected_versions: params.expected_versions,
                            check_only: params.check_only,
                        },
                    )
                    .await
                {
                    Ok(applied) => ApplyPatchResult {
                        status: applied.status,
                        operation_id: operation_id.clone(),
                        replayed: false,
                        files: applied.files,
                        changes: applied.changes,
                        work_id: None,
                        coordination: None,
                    },
                    Err(RunnerError::Execution(err)) => {
                        let failed =
                            ApplyPatchResult::new(PatchStatus::Rejected, operation_id.clone());
                        let _ = self.store.finish(&operation_id, &failed);
                        return Err(err.with_operation_id(operation_id.0));
                    }
                    Err(RunnerError::TransportBeforeDispatch { .. })
                    | Err(RunnerError::TransportAmbiguous { .. }) => {
                        let unknown =
                            ApplyPatchResult::new(PatchStatus::Unknown, operation_id.clone());
                        self.store.finish(&operation_id, &unknown)?;
                        return Ok(self.with_hint(
                            unknown,
                            &params.workspace_id.0,
                            params.work_id.as_ref(),
                        ));
                    }
                };
                self.store.finish(&operation_id, &result)?;
                Ok(self.with_hint(result, &params.workspace_id.0, params.work_id.as_ref()))
            }
        }
    }

    fn hint(&self, workspace_id: &str, work_id: Option<&WorkId>) -> Option<CoordinationHint> {
        self.store
            .coordination_hint(workspace_id, work_id)
            .ok()
            .flatten()
    }

    async fn process_hint(&self, process_id: &str) -> Option<CoordinationHint> {
        let ws = self.runner.workspace_of(process_id).await?;
        self.hint(&ws, None)
    }

    fn with_hint(
        &self,
        mut result: ApplyPatchResult,
        workspace_id: &str,
        work_id: Option<&WorkId>,
    ) -> ApplyPatchResult {
        result.work_id = work_id.cloned();
        result.coordination = self.hint(workspace_id, work_id);
        result
    }

    fn maybe_hold_patch(&self, ws: &Workspace, params: &ApplyPatchParams) -> Result<(), ErrorBody> {
        if !ws.approvals.holds_mutations() {
            return Ok(());
        }
        let snapshot = serde_json::to_value(params)
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        let created = self.store.create_approval(
            &ws.id.0,
            ApprovalTargetTool::ApplyPatch,
            &Store::fingerprint(params),
            snapshot,
        )?;
        Err(approval_required(&created.approval_id))
    }

    fn maybe_hold_exec(&self, ws: &Workspace, params: &ExecCommandParams) -> Result<(), ErrorBody> {
        if !ws.approvals.holds_mutations() {
            return Ok(());
        }
        let snapshot = serde_json::to_value(params)
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidCommand, err.to_string()))?;
        let created = self.store.create_approval(
            &ws.id.0,
            ApprovalTargetTool::ExecCommand,
            &Store::exec_fingerprint(params),
            snapshot,
        )?;
        Err(approval_required(&created.approval_id))
    }

    fn approval_create_inner(
        &self,
        params: ApprovalCreateParams,
    ) -> Result<ApprovalCreateResult, ErrorBody> {
        match params.tool {
            ApprovalTargetTool::ApplyPatch => {
                let args: ApplyPatchParams = serde_json::from_value(params.arguments)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
                let snapshot = serde_json::to_value(&args)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
                let ws = self.registry.get(&args.workspace_id.0)?;
                allow(ws, Action::Write, &ClientClaims::default())?;
                ws.require_file_write()?;
                let created = self.store.create_approval(
                    &ws.id.0,
                    ApprovalTargetTool::ApplyPatch,
                    &Store::fingerprint(&args),
                    snapshot,
                )?;
                Ok(ApprovalCreateResult {
                    approval_id: created.approval_id,
                    state: created.state,
                })
            }
            ApprovalTargetTool::ExecCommand => {
                let args: ExecCommandParams = serde_json::from_value(params.arguments)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidCommand, err.to_string()))?;
                if args.command.is_empty() || args.command[0].is_empty() {
                    return Err(invalid_argv());
                }
                let snapshot = serde_json::to_value(&args)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidCommand, err.to_string()))?;
                let ws = self.registry.get(&args.workspace_id.0)?;
                allow(ws, Action::Exec, &ClientClaims::default())?;
                ws.require_exec()?;
                let created = self.store.create_approval(
                    &ws.id.0,
                    ApprovalTargetTool::ExecCommand,
                    &Store::exec_fingerprint(&args),
                    snapshot,
                )?;
                Ok(ApprovalCreateResult {
                    approval_id: created.approval_id,
                    state: created.state,
                })
            }
        }
    }

    async fn operation_resume_inner(
        &self,
        params: OperationResumeParams,
    ) -> Result<OperationResumeResult, ErrorBody> {
        match self.store.claim_resume(&params.approval_id)? {
            ResumeClaim::ReplaySuccess(result) => Ok(result),
            ResumeClaim::ReplayError(err) => Err(err),
            ResumeClaim::Execute(record, _guard) => {
                let outcome = self.execute_approved(record.clone()).await;
                self.commit_resume(&record.approval_id, outcome)
            }
            ResumeClaim::Reconcile(record, _guard) => self.reconcile_resume(record).await,
        }
    }

    fn commit_resume(
        &self,
        approval_id: &ApprovalId,
        outcome: Result<OperationResumeResult, ErrorBody>,
    ) -> Result<OperationResumeResult, ErrorBody> {
        match &outcome {
            Ok(success) => match self.store.finish_resume(approval_id, Ok(success)) {
                Ok(()) => Ok(success.clone()),
                Err(mut err) => {
                    if err.approval_id.is_none() {
                        err = err.with_approval_id(approval_id.0.as_str());
                    }
                    if err.operation_id.is_none() {
                        if let Some(op) = success.apply_patch.as_ref() {
                            err = err.with_operation_id(op.operation_id.0.clone());
                        }
                    }
                    if err.code != ErrorCode::ApprovalAmbiguous {
                        err.code = ErrorCode::ApprovalAmbiguous;
                        err.message =
                            "mutation may have completed; resume result was not persisted".into();
                    }
                    Err(err)
                }
            },
            Err(err) => match self.store.finish_resume(approval_id, Err(err)) {
                Ok(()) => outcome,
                Err(_) => outcome,
            },
        }
    }

    async fn reconcile_resume(
        &self,
        record: codespace_store::ApprovalRecord,
    ) -> Result<OperationResumeResult, ErrorBody> {
        match record.tool {
            ApprovalTargetTool::ApplyPatch => {
                if let Some(stored) = self.lookup_patch_ledger(&record)? {
                    let result = OperationResumeResult {
                        approval_id: record.approval_id.clone(),
                        state: ApprovalState::Consumed,
                        apply_patch: Some(self.with_hint(
                            stored.result,
                            &record.workspace_id,
                            None,
                        )),
                        exec_command: None,
                    };
                    return self.commit_resume(&record.approval_id, Ok(result));
                }
                if params_scrubbed(&record.params_json) {
                    let err = ErrorBody::new(
                        ErrorCode::ApprovalAmbiguous,
                        "resume was interrupted and the snapshot is no longer available",
                    )
                    .with_approval_id(record.approval_id.0.as_str());
                    return self.commit_resume(&record.approval_id, Err(err));
                }
                let outcome = self.execute_approved(record.clone()).await;
                self.commit_resume(&record.approval_id, outcome)
            }
            ApprovalTargetTool::ExecCommand => {
                let err = ErrorBody::new(
                    ErrorCode::ApprovalAmbiguous,
                    "exec resume is ambiguous after interruption; the process was not restarted",
                )
                .with_approval_id(record.approval_id.0.as_str());
                self.commit_resume(&record.approval_id, Err(err))
            }
        }
    }

    fn lookup_patch_ledger(
        &self,
        record: &codespace_store::ApprovalRecord,
    ) -> Result<Option<StoredOperation>, ErrorBody> {
        if let Ok(params) = serde_json::from_str::<ApplyPatchParams>(&record.params_json) {
            if let Some(key) = params.operation_key.as_ref() {
                match self.store.status_lookup(None, Some(key)) {
                    Ok(status) => return Ok(Some(self.store.get(&status.operation_id)?)),
                    Err(err) if err.code == ErrorCode::OperationNotFound => {}
                    Err(err) => return Err(err),
                }
            }
            return self.store.find_operation_by_fingerprint(
                &record.workspace_id,
                &Store::fingerprint(&params),
                record.resolved_at,
            );
        }
        self.store.find_operation_by_fingerprint(
            &record.workspace_id,
            &record.fingerprint,
            record.resolved_at,
        )
    }

    async fn execute_approved(
        &self,
        record: codespace_store::ApprovalRecord,
    ) -> Result<OperationResumeResult, ErrorBody> {
        match record.tool {
            ApprovalTargetTool::ApplyPatch => {
                let params: ApplyPatchParams = serde_json::from_str(&record.params_json)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
                let result = self
                    .apply_patch_inner(params, Some(&record.approval_id))
                    .await?;
                Ok(OperationResumeResult {
                    approval_id: record.approval_id,
                    state: ApprovalState::Consumed,
                    apply_patch: Some(result),
                    exec_command: None,
                })
            }
            ApprovalTargetTool::ExecCommand => {
                let params: ExecCommandParams = serde_json::from_str(&record.params_json)
                    .map_err(|err| ErrorBody::new(ErrorCode::InvalidCommand, err.to_string()))?;
                let result = self
                    .exec_command_inner(params, Some(&record.approval_id))
                    .await?;
                Ok(OperationResumeResult {
                    approval_id: record.approval_id,
                    state: ApprovalState::Consumed,
                    apply_patch: None,
                    exec_command: Some(result),
                })
            }
        }
    }

    async fn exec_command_inner(
        &self,
        params: ExecCommandParams,
        resume: Option<&ApprovalId>,
    ) -> Result<ExecCommandResult, ErrorBody> {
        let ws = self.registry.get(&params.workspace_id.0)?;
        allow(ws, Action::Exec, &ClientClaims::default())?;
        ws.require_exec()?;
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(invalid_argv());
        }
        // Also here, so an approval resumed after the registry changed is governed as it is now.
        let participation = self.participation(ws).await?;
        let process_id = ProcessId(format!("proc-{}", Uuid::new_v4()));
        let mut reservation = self
            .store
            .acquire_shell_busy(&params.workspace_id.0, &process_id.0)
            .await?;
        if let Some(id) = resume {
            self.store.mark_resuming(id)?;
        }
        let mut req = RunnerExecRequest::for_host(params.command, process_id.clone(), ws.profile);
        req.policy.network = ws.network;
        req.tty = params.tty;
        match participation {
            Participating::Off => {}
            #[cfg(feature = "devguard")]
            Participating::Managed => {
                // The workspace lease is held while the owner prepares; nothing is started, so
                // it is released with the answer whatever the attempt's own state.
                let refused = self.prepare_governed(ws, req).await;
                reservation.abort();
                return Err(refused);
            }
        }
        reservation.arm_dispatch();
        match self.runner.exec(ws, req).await {
            Ok(result) => {
                reservation.confirm();
                Ok(ExecCommandResult {
                    process_id: result.process_id,
                    dispatch_status: ExecDispatchStatus::Confirmed,
                    coordination: self.hint(&params.workspace_id.0, params.work_id.as_ref()),
                })
            }
            Err(RunnerError::TransportAmbiguous { .. }) => {
                reservation.confirm();
                Ok(ExecCommandResult {
                    process_id,
                    dispatch_status: ExecDispatchStatus::Unknown,
                    coordination: self.hint(&params.workspace_id.0, params.work_id.as_ref()),
                })
            }
            Err(err) => {
                reservation.abort();
                Err(err.into_error_body())
            }
        }
    }
}

/// How a new execution in a workspace takes part in the resource authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Participating {
    /// Run as always, without the authority.
    Off,
    /// Prepare it through the execution owner's admission (CSRG-U3). It never runs outside it.
    #[cfg(feature = "devguard")]
    Managed,
}

impl CodeSpace {
    /// How a new execution in `ws` takes part, or why this gateway refuses it. A workspace that
    /// requires resource participation is prepared through the execution owner's admission
    /// when the gateway was started with `--devguard register` and its owner can prepare;
    /// otherwise it is refused. Nothing ever falls back to running it without the authority.
    async fn participation(&self, ws: &Workspace) -> Result<Participating, ErrorBody> {
        if ws.resources.participation == Participation::Off {
            return Ok(Participating::Off);
        }
        #[cfg(feature = "devguard")]
        if let Some(registration) = &self.registration {
            if registration.prepares(&self.runner) {
                return Ok(Participating::Managed);
            }
        }
        Err(ErrorBody::new(
            ErrorCode::ResourcePolicyUnsupported,
            format!(
                "workspace `{}` requires resource participation: {}; this gateway cannot \
                 admit executions through the resource authority, so nothing was started",
                ws.id.0,
                self.participation_readiness().await
            ),
        ))
    }

    /// Have the execution owner prepare `req` (CSRG-U3). Managed launch is not available yet,
    /// so every preparation ends in a refusal and nothing is started.
    #[cfg(feature = "devguard")]
    async fn prepare_governed(&self, ws: &Workspace, req: RunnerExecRequest) -> ErrorBody {
        let Some(registration) = &self.registration else {
            return ErrorBody::new(ErrorCode::Internal, "no execution owner registration");
        };
        let attempt_id = format!("cs-attempt-{}", Uuid::new_v4().simple());
        match registration
            .prepare(&self.runner, ws, req, attempt_id)
            .await
        {
            Ok(report) => report.into_error_body(&ws.id.0),
            Err(err) => err,
        }
    }

    #[cfg(not(feature = "devguard"))]
    async fn participation_readiness(&self) -> String {
        "this CodeSpace was built without DevGuard".into()
    }

    /// Whether the execution owner is registered, from a session it started for this call.
    #[cfg(feature = "devguard")]
    async fn participation_readiness(&self) -> String {
        use codespace_domain::ResourceRegistrationState;
        let Some(registration) = &self.registration else {
            return "the gateway was not started with --devguard register".into();
        };
        let state = registration
            .report(&self.runner)
            .await
            .registration
            .map(|registration| registration.state)
            .unwrap_or(ResourceRegistrationState::Unavailable);
        match state {
            ResourceRegistrationState::Registered => {
                "the execution owner is registered with DevGuard".into()
            }
            state => format!(
                "the execution owner is not registered with DevGuard (registration: {})",
                serde_json::to_value(state)
                    .ok()
                    .and_then(|name| name.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
        }
    }
}

fn invalid_argv() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidCommand,
        "command must be a non-empty argv (no shell)",
    )
}

fn approval_required(id: &ApprovalId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ApprovalRequired,
        "confirmation is required before this allowed mutation can run",
    )
    .with_approval_id(id.0.as_str())
}

fn params_scrubbed(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.get("scrubbed").and_then(|flag| flag.as_bool()))
        .unwrap_or(false)
}

fn runner_err_json(err: RunnerError) -> String {
    err_json(err.into_error_body())
}

fn client_environment_kind(kind: EnvironmentKind) -> ClientEnvironmentKind {
    match kind {
        EnvironmentKind::Host => ClientEnvironmentKind::Host,
        EnvironmentKind::LinuxContainer => ClientEnvironmentKind::LinuxContainer,
    }
}

fn client_network_policy(axis: NetworkAxis) -> NetworkPolicyState {
    match axis {
        NetworkAxis::Restricted => NetworkPolicyState::Restricted,
        NetworkAxis::Enabled => NetworkPolicyState::Enabled,
    }
}

fn workspace_execution_info(ws: &Workspace) -> WorkspaceExecutionInfo {
    let mut policy = PermissionProfile::from_workspace_profile(ws.profile);
    policy.network = ws.network;
    let permissions = EffectivePermissionInfo {
        read: policy.allows(Action::Read),
        write: policy.allows(Action::Write),
        exec: policy.allows(Action::Exec),
    };
    let environment = EnvironmentExecutionInfo {
        kind: client_environment_kind(ws.environment_kind),
        client_selectable: false,
        exec_supported: ws.environment_kind.exec_supported(),
        file_read_supported: ws.environment_kind.file_read_supported(),
        file_write_supported: ws.environment_kind.file_write_supported(),
    };
    let mut info = WorkspaceExecutionInfo::from_effective(
        environment,
        permissions,
        client_network_policy(policy.network),
    );
    info.approvals = ws.approvals;
    advertise_linux_sandbox(info)
}

fn advertise_linux_sandbox(info: WorkspaceExecutionInfo) -> WorkspaceExecutionInfo {
    if linux_sandbox_available() {
        info.with_linux_command_sandbox()
    } else {
        info
    }
}

fn lookup(registry: &Registry, workspace_id: Option<String>) -> Result<WorkspaceInfo, ErrorBody> {
    let Some(id) = workspace_id.filter(|s| !s.is_empty()) else {
        return Ok(workspace_info(None));
    };
    let ws = registry.get(&id)?;
    let mut info = workspace_info(Some(id));
    info.profile = Some(ws.profile);
    info.root = Some(ws.root.display().to_string());
    info.execution = Some(workspace_execution_info(ws));
    info.note = format!(
        "workspace_id is a selector, not a credential. profile={:?}",
        ws.profile
    );
    Ok(info)
}

impl CodeSpace {
    /// The gateway's handler for its settings. With the `devguard` feature and
    /// `--devguard status`, `workspace_info` also reports DevGuard's status; otherwise no
    /// DevGuard session is ever opened.
    pub fn from_cli(
        cli: &crate::config::Cli,
        registry: Registry,
        store: Arc<Store>,
        runner: RuntimeBackend,
    ) -> Self {
        let handler = Self::with_store_and_runner(registry, store, runner);
        #[cfg(feature = "devguard")]
        if let Some(settings) = cli.devguard.settings() {
            if cli.devguard.mode == crate::devguard::DevGuardMode::Register {
                let registration = crate::devguard::Registration::new(
                    settings,
                    cli.runner,
                    cli.runtime_bin.is_some(),
                );
                tracing::info!(
                    participation = "registration",
                    owner = ?registration.owner(),
                    "DevGuard registration of the execution owner enabled; it governs no execution"
                );
                return handler.with_registration(registration);
            }
            tracing::info!(
                participation = "status",
                "DevGuard status connection enabled; it governs no execution"
            );
            return handler
                .with_resource_authority(crate::devguard::ResourceAuthority::new(settings));
        }
        #[cfg(not(feature = "devguard"))]
        let _ = cli;
        handler
    }
}

#[cfg(feature = "devguard")]
impl CodeSpace {
    /// Report this authority's status in `workspace_info`. It governs no execution.
    pub fn with_resource_authority(
        mut self,
        authority: Arc<crate::devguard::ResourceAuthority>,
    ) -> Self {
        self.resource_authority = Some(authority);
        self
    }

    /// Report the execution owner's registration in `workspace_info` (CSRG-U2). It governs no
    /// execution.
    pub fn with_registration(mut self, registration: Arc<crate::devguard::Registration>) -> Self {
        self.registration = Some(registration);
        self
    }

    /// Have the execution owner register now, rather than at the first call that asks.
    pub fn register_owner_now(&self) {
        if let Some(registration) = self.registration.clone() {
            let runner = self.runner.clone();
            tokio::spawn(async move { registration.report(&runner).await });
        }
    }

    async fn with_resource_authority_status(&self, mut info: WorkspaceInfo) -> WorkspaceInfo {
        if let Some(registration) = &self.registration {
            info.resource_authority = Some(registration.report(&self.runner).await);
        } else if let Some(authority) = &self.resource_authority {
            info.resource_authority = Some(authority.status().await);
        }
        info
    }
}

impl Default for CodeSpace {
    fn default() -> Self {
        Self::new(Registry::new())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodeSpace {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(CORE_BASELINE)
            .with_server_info(Implementation::new(
                codespace_domain::SERVER_NAME,
                codespace_domain::SERVER_VERSION,
            ))
            .with_instructions(MCP_INSTRUCTIONS.to_string())
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request.clone());
        let result = self.negotiate_initialize(&request)?;
        let features = NegotiatedFeatures::from_protocol(result.protocol_version.clone());
        // Enhancement flags may be true for 2026-07-28. This PR does not take
        // MRTR / Tasks / subscriptions / SEP-2243 / stateless-HTTP handler paths.
        tracing::debug!(
            protocol = %features.protocol,
            mrtr = features.mrtr,
            tasks = features.tasks,
            subscriptions = features.subscriptions,
            standard_http_headers = features.standard_http_headers,
            stateless_http = features.stateless_http,
            "negotiated mcp protocol; handlers stay on tools/call"
        );
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{
        ClientEnvironmentKind, CommandSandboxState, ExecDispatchStatus, NetworkEnforcementState,
        NetworkPolicyState, OperationKey, Profile, WorkspaceId,
    };
    use codespace_policy::{EnvironmentKind, Workspace};
    use codespace_runner::{host_worker, serve_runner_connection, UdsRunner};
    use std::collections::BTreeMap;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    fn write_registry(root: std::path::PathBuf) -> Registry {
        let mut registry = Registry::new();
        registry.insert(Workspace::new(
            WorkspaceId("demo".into()),
            root,
            codespace_domain::Profile::WorkspaceWrite,
        ));
        registry
    }

    fn assert_advertised_matches_policy(ws: &Workspace, exec: &WorkspaceExecutionInfo) {
        let mut policy = PermissionProfile::from_workspace_profile(ws.profile);
        policy.network = ws.network;
        assert_eq!(exec.permissions.read, policy.allows(Action::Read));
        assert_eq!(exec.permissions.write, policy.allows(Action::Write));
        assert_eq!(exec.permissions.exec, policy.allows(Action::Exec));
        assert_eq!(exec.network.policy, client_network_policy(policy.network));
        assert_eq!(exec.approvals, ws.approvals);
        assert!(!exec.network.client_may_escalate);
        assert_eq!(
            exec.environment.exec_supported,
            ws.environment_kind.exec_supported()
        );
        assert_eq!(
            exec.environment.file_read_supported,
            ws.environment_kind.file_read_supported()
        );
        assert_eq!(
            exec.environment.file_write_supported,
            ws.environment_kind.file_write_supported()
        );
        assert_eq!(
            exec.files.read.available,
            exec.permissions.read && exec.environment.file_read_supported
        );
        assert_eq!(
            exec.files.find.available,
            exec.permissions.read && exec.environment.file_read_supported
        );
        assert_eq!(
            exec.files.patch.available,
            exec.permissions.write && exec.environment.file_write_supported
        );
        assert_eq!(
            exec.process.available,
            exec.permissions.exec && exec.environment.exec_supported
        );
        if linux_sandbox_available() {
            assert_eq!(
                exec.isolation.command_sandbox,
                CommandSandboxState::LinuxSandbox
            );
            assert_eq!(exec.network.enforcement, NetworkEnforcementState::Enforced);
        } else {
            assert_eq!(exec.isolation.command_sandbox, CommandSandboxState::None);
            assert_eq!(exec.network.enforcement, NetworkEnforcementState::None);
        }
    }

    fn assert_instructions_cover_execution_contract(text: &str) {
        assert!(text.contains("optional offset and limit"), "{text}");
        assert!(text.contains("next_offset"), "{text}");
        assert!(text.contains("content_lossy"), "{text}");
        assert!(text.contains("incomplete"), "{text}");
        assert!(text.contains("listing_version"), "{text}");
        assert!(text.contains("OUTPUT_LIMIT"), "{text}");
        assert!(
            text.contains("Ending an MCP request does not terminate the process"),
            "{text}"
        );
        assert!(text.contains("fixed 24x80 PTY"), "{text}");
        assert!(text.contains("process_resize"), "{text}");
        assert!(text.contains("owned by the runner instance"), "{text}");
        assert!(
            text.contains("Client or HTTP disconnect keeps the process running"),
            "{text}"
        );
        assert!(text.contains("PROCESS_NOT_TTY"), "{text}");
        assert!(text.contains("PROCESS_NOT_RUNNING"), "{text}");
        assert!(text.contains("WORKSPACE_BUSY"), "{text}");
        assert!(text.contains("RESOURCE_QUEUE_FULL"), "{text}");
        assert!(text.contains("command_sandbox is linux-sandbox"), "{text}");
        assert!(
            text.contains("host execution is not an OS command sandbox"),
            "{text}"
        );
        assert!(
            text.contains("Network policy is reported by workspace_info"),
            "{text}"
        );
        assert!(text.contains("execution.network.enforcement"), "{text}");
        assert!(
            text.contains("absence of enforcement is not permission"),
            "{text}"
        );
        assert!(
            text.contains("Enabled network uses a managed proxy"),
            "{text}"
        );
        assert!(
            text.contains("missing Linux helper is not permission"),
            "{text}"
        );
        assert!(!text.contains("not granted by policy"), "{text}");
        assert!(
            text.contains("Treat apply_patch status=unknown as possibly executed"),
            "{text}"
        );
        assert!(text.contains("dispatch_status=unknown"), "{text}");
        assert!(text.contains("uncertain attempt"), "{text}");
        assert!(text.contains("backend remains reachable"), "{text}");
        assert!(text.contains("process_status"), "{text}");
        assert!(
            text.contains("signaled means a signal CodeSpace did not send"),
            "{text}"
        );
        assert!(text.contains("output_lost"), "{text}");
        assert!(
            text.contains("EOF from read_process is not a successful exit"),
            "{text}"
        );
        assert!(
            text.contains("tty_size is not an exec_command argument"),
            "{text}"
        );
        assert!(
            !text.contains("inspect or terminate that handle rather than"),
            "{text}"
        );
        assert!(text.contains("major checkpoints"), "{text}");
        assert!(text.contains("approvals=confirm"), "{text}");
        assert!(text.contains("APPROVAL_REQUIRED"), "{text}");
        assert!(text.contains("not a privilege grant"), "{text}");
        assert!(text.contains("same MCP caller can grant"), "{text}");
        assert!(text.contains("isolation boundary"), "{text}");
    }

    #[test]
    fn initialize_instructions_cover_execution_contract() {
        let cfg = CodeSpace::default().get_info();
        let text = cfg.instructions.expect("instructions");
        assert_instructions_cover_execution_contract(&text);
    }

    #[test]
    fn workspace_info_without_id_omits_execution() {
        let info = lookup(&Registry::new(), None).unwrap();
        assert!(info.execution.is_none());
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("execution").is_none());
    }

    #[test]
    fn workspace_info_host_write_exposes_execution() {
        let dir = tempfile::tempdir().unwrap();
        let registry = write_registry(dir.path().to_path_buf());
        let info = lookup(&registry, Some("demo".into())).unwrap();
        let exec = info.execution.as_ref().expect("execution");
        assert_advertised_matches_policy(registry.get("demo").unwrap(), exec);
        assert_eq!(exec.environment.kind, ClientEnvironmentKind::Host);
        assert!(!exec.environment.client_selectable);
        assert!(exec.environment.exec_supported);
        assert!(exec.environment.file_read_supported);
        assert!(exec.environment.file_write_supported);
        assert!(exec.permissions.read && exec.permissions.write && exec.permissions.exec);
        assert!(
            exec.files.read.available && exec.files.find.available && exec.files.patch.available
        );
        assert!(exec.files.capabilities.read_range);
        assert!(exec.files.capabilities.find_pagination);
        assert!(exec.process.available);
        let tty = &exec
            .process
            .capabilities
            .as_ref()
            .expect("capabilities")
            .tty;
        assert!(tty.supported);
        assert!(tty.resize_supported);
        assert_eq!(exec.network.policy, NetworkPolicyState::Restricted);
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            json["execution"]["process"]["capabilities"]["lifetime"]["owner"],
            "runner"
        );
        assert_eq!(
            json["execution"]["process"]["capabilities"]["lifetime"]["client_disconnect"],
            "keep_running"
        );
        assert_eq!(
            json["execution"]["process"]["capabilities"]["lifetime"]["runner_disconnect"],
            "terminate"
        );
        assert_eq!(
            json["execution"]["process"]["capabilities"]["lifetime"]["restart_recovery"],
            "none"
        );
        assert_eq!(
            json["execution"]["files"]["capabilities"]["read_range"],
            true
        );
        assert_eq!(
            json["execution"]["files"]["capabilities"]["find_pagination"],
            true
        );
        assert_eq!(
            json["execution"]["files"]["capabilities"]["read_max_bytes"],
            1048576
        );
        assert_eq!(
            json["execution"]["files"]["capabilities"]["find_max_paths"],
            10000
        );
        assert_eq!(json["execution"]["serialization"]["scope"], "workspace");
        assert_eq!(
            json["execution"]["serialization"]["request_conflict"],
            "wait-fifo"
        );
        assert_eq!(
            json["execution"]["serialization"]["process_conflict"],
            "reject"
        );
        assert_eq!(json["execution"]["serialization"]["queue_durable"], false);
        assert_eq!(
            json["execution"]["serialization"]["max_waiters_per_resource"],
            64
        );
        assert_eq!(
            json["execution"]["serialization"]["conflict_error"],
            "WORKSPACE_BUSY"
        );
        assert_eq!(
            json["execution"]["serialization"]["queue_full_error"],
            "RESOURCE_QUEUE_FULL"
        );
        assert!(json.get("environment_id").is_none());
        assert!(!json.to_string().contains("\"environment_id\""));
    }

    #[test]
    fn workspace_info_operator_enabled_network() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::json!({
            "workspaces": {
                "demo": {
                    "root": dir.path(),
                    "profile": "workspace-write",
                    "network": "enabled"
                }
            }
        });
        let registry = Registry::load_json(&json.to_string()).unwrap();
        let info = lookup(&registry, Some("demo".into())).unwrap();
        let exec = info.execution.as_ref().expect("execution");
        assert_advertised_matches_policy(registry.get("demo").unwrap(), exec);
        assert_eq!(exec.network.policy, NetworkPolicyState::Enabled);
        assert!(!exec.network.client_may_escalate);
        assert!(exec.permissions.write && exec.permissions.exec);
        if linux_sandbox_available() {
            assert_eq!(exec.network.enforcement, NetworkEnforcementState::Enforced);
            assert_eq!(
                exec.isolation.command_sandbox,
                CommandSandboxState::LinuxSandbox
            );
        } else {
            assert_eq!(exec.network.enforcement, NetworkEnforcementState::None);
        }
    }

    #[test]
    fn workspace_info_host_read_only_denies_write_and_exec() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::new();
        registry.insert(Workspace::new(
            WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            Profile::ReadOnly,
        ));
        let exec = lookup(&registry, Some("demo".into()))
            .unwrap()
            .execution
            .expect("execution");
        assert_advertised_matches_policy(registry.get("demo").unwrap(), &exec);
        assert!(exec.permissions.read);
        assert!(!exec.permissions.write);
        assert!(!exec.permissions.exec);
        assert!(exec.environment.exec_supported);
        assert!(exec.environment.file_read_supported);
        assert!(exec.environment.file_write_supported);
        assert!(exec.files.read.available && exec.files.find.available);
        assert!(!exec.files.patch.available);
        assert!(!exec.process.available);
        assert!(exec.process.capabilities.is_none());
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["process"]["available"], false);
        assert!(json["process"].get("capabilities").is_none());
    }

    #[test]
    fn workspace_info_linux_container_write_is_policy_true_backend_false() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::new();
        let mut ws = Workspace::new(
            WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            Profile::WorkspaceWrite,
        );
        ws.environment_kind = EnvironmentKind::LinuxContainer;
        registry.insert(ws);
        let exec = lookup(&registry, Some("demo".into()))
            .unwrap()
            .execution
            .expect("execution");
        assert_advertised_matches_policy(registry.get("demo").unwrap(), &exec);
        assert!(exec.permissions.exec);
        assert!(exec.permissions.read && exec.permissions.write);
        assert!(!exec.environment.exec_supported);
        assert!(!exec.environment.file_read_supported);
        assert!(!exec.environment.file_write_supported);
        assert!(!exec.files.read.available);
        assert!(!exec.files.find.available);
        assert!(!exec.files.patch.available);
        assert!(!exec.process.available);
        assert!(exec.process.capabilities.is_none());
        let json = serde_json::to_value(&exec).unwrap();
        assert_eq!(json["process"]["available"], false);
        assert!(json["process"].get("capabilities").is_none());
    }

    #[test]
    fn workspace_info_linux_container_read_only_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::new();
        let mut ws = Workspace::new(
            WorkspaceId("demo".into()),
            dir.path().to_path_buf(),
            codespace_domain::Profile::ReadOnly,
        );
        ws.environment_kind = EnvironmentKind::LinuxContainer;
        registry.insert(ws);
        let exec = lookup(&registry, Some("demo".into()))
            .unwrap()
            .execution
            .expect("execution");
        assert_advertised_matches_policy(registry.get("demo").unwrap(), &exec);
        assert!(!exec.permissions.exec);
        assert!(exec.permissions.read);
        assert!(!exec.environment.exec_supported);
        assert!(!exec.environment.file_read_supported);
        assert!(!exec.environment.file_write_supported);
        assert!(!exec.files.read.available);
        assert!(!exec.files.find.available);
        assert!(!exec.files.patch.available);
        assert!(!exec.process.available);
        assert!(exec.process.capabilities.is_none());
    }

    async fn drop_after_one_frame(stream: UnixStream) {
        let (mut read, _write) = stream.into_split();
        let mut len_buf = [0u8; 4];
        if read.read_exact(&mut len_buf).await.is_err() {
            return;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        let _ = read.read_exact(&mut payload).await;
    }

    #[tokio::test]
    async fn apply_patch_uds_loss_records_unknown_not_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        tokio::spawn(async move {
            drop_after_one_frame(server).await;
        });
        let store = Arc::new(Store::memory().unwrap());
        let runner = RuntimeBackend::Uds(UdsRunner::from_stream(client, Arc::new(|_| {})));
        let cs = CodeSpace::with_store_and_runner(write_registry(ws_root), store.clone(), runner);
        let result = cs
            .apply_patch_inner(
                ApplyPatchParams {
                    workspace_id: WorkspaceId("demo".into()),
                    patch: "*** Begin Patch\n*** Add File: lost.txt\n+x\n*** End Patch\n".into(),
                    expected_versions: BTreeMap::new(),
                    operation_key: Some(OperationKey("k-unknown".into())),
                    check_only: false,
                    work_id: None,
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.status, PatchStatus::Unknown);
        let stored = store.get(&result.operation_id).unwrap();
        assert_eq!(stored.status, PatchStatus::Unknown);
        assert_ne!(stored.status, PatchStatus::Rejected);
        assert!(stored.finished_at.is_some());
        assert!(stored.events.iter().any(|event| {
            event.name == codespace_domain::OperationEventName::Finished
                && event.reason.as_deref() == Some("unknown")
        }));
        assert!(stored.result.changes.is_empty());
        assert!(!dir.path().join("ws/lost.txt").exists());
    }

    #[tokio::test]
    async fn exec_ambiguous_keeps_workspace_busy() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        tokio::spawn(async move {
            drop_after_one_frame(server).await;
        });
        let store = Arc::new(Store::memory().unwrap());
        let runner = RuntimeBackend::Uds(UdsRunner::from_stream(client, Arc::new(|_| {})));
        let cs = CodeSpace::with_store_and_runner(write_registry(ws_root), store.clone(), runner);
        let started = cs
            .exec_command(Parameters(ExecCommandParams {
                workspace_id: WorkspaceId("demo".into()),
                command: vec!["/bin/echo".into(), "x".into()],
                work_id: None,
                tty: false,
            }))
            .await
            .unwrap();
        assert!(started.0.process_id.0.starts_with("proc-"));
        assert_eq!(started.0.dispatch_status, ExecDispatchStatus::Unknown);
        let busy = cs
            .apply_patch_inner(
                ApplyPatchParams {
                    workspace_id: WorkspaceId("demo".into()),
                    patch: "*** Begin Patch\n*** Add File: later.txt\n+x\n*** End Patch\n".into(),
                    expected_versions: BTreeMap::new(),
                    operation_key: None,
                    check_only: false,
                    work_id: None,
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(busy.code, ErrorCode::WorkspaceBusy);
    }

    #[tokio::test]
    async fn process_exited_releases_exclusive_lease() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let store = Arc::new(Store::memory().unwrap());
        let store_for_lease = store.clone();
        let runner = RuntimeBackend::Uds(UdsRunner::from_stream(
            client,
            Arc::new(move |process_id: &str| {
                store_for_lease.release_process(process_id);
            }),
        ));
        let cs = CodeSpace::with_store_and_runner(write_registry(ws_root), store.clone(), runner);
        cs.exec_command(Parameters(ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into(), "done".into()],
            work_id: None,
            tty: false,
        }))
        .await
        .unwrap();
        for _ in 0..50 {
            if store.try_acquire_write("demo").is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _lease = store.try_acquire_write("demo").expect("lease released");
    }

    #[tokio::test]
    async fn exec_host_reports_confirmed_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let cs = CodeSpace::new(write_registry(ws_root));
        let started = cs
            .exec_command(Parameters(ExecCommandParams {
                workspace_id: WorkspaceId("demo".into()),
                command: vec!["/bin/echo".into(), "ok".into()],
                work_id: None,
                tty: false,
            }))
            .await
            .unwrap();
        assert_eq!(started.0.dispatch_status, ExecDispatchStatus::Confirmed);
        assert!(started.0.process_id.0.starts_with("proc-"));
    }

    #[tokio::test]
    async fn aborted_apply_waiter_releases_queue_slot() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let cs = CodeSpace::with_store(write_registry(ws_root), store.clone());
        let lease = store.acquire_write("demo").await.unwrap();
        let cs_wait = cs.clone();
        let waiting = tokio::spawn(async move {
            cs_wait
                .apply_patch_inner(
                    ApplyPatchParams {
                        workspace_id: WorkspaceId("demo".into()),
                        patch: "*** Begin Patch\n*** Add File: wait.txt\n+x\n*** End Patch\n"
                            .into(),
                        expected_versions: BTreeMap::new(),
                        operation_key: Some(OperationKey("wait-cancel".into())),
                        check_only: false,
                        work_id: None,
                    },
                    None,
                )
                .await
        });
        for _ in 0..30 {
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        waiting.abort();
        let _ = waiting.await;
        drop(lease);
        let _next = store
            .acquire_write("demo")
            .await
            .expect("queue released after cancelled waiter");
    }

    fn parse_exec_err(result: Result<Json<ExecCommandResult>, String>) -> ErrorBody {
        match result {
            Err(err) => serde_json::from_str(&err)
                .unwrap_or_else(|parse| panic!("exec err json ({parse}): {err}")),
            Ok(ok) => panic!("expected exec error, got process_id={}", ok.0.process_id.0),
        }
    }

    async fn exec_echo(cs: &CodeSpace) -> ExecCommandResult {
        cs.exec_command(Parameters(ExecCommandParams {
            workspace_id: WorkspaceId("demo".into()),
            command: vec!["/bin/echo".into(), "ok".into()],
            work_id: None,
            tty: false,
        }))
        .await
        .unwrap()
        .0
    }

    async fn assert_missing_exec_boundary_and_release(cs: &CodeSpace, store: &Store, tty: bool) {
        let result = cs
            .exec_command(Parameters(ExecCommandParams {
                workspace_id: WorkspaceId("demo".into()),
                command: vec!["/no/such/codespace-exec".into()],
                work_id: None,
                tty,
            }))
            .await;

        if linux_sandbox_available() {
            let started = result.expect("sandbox helper should spawn successfully").0;
            assert_eq!(started.dispatch_status, ExecDispatchStatus::Confirmed);
            for _ in 0..50 {
                if store.try_acquire_write("demo").is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        } else {
            let err = parse_exec_err(result);
            assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
        }

        let _lease = store.try_acquire_write("demo").expect("lease released");
    }

    #[tokio::test]
    async fn invalid_command_does_not_hold_lease() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let cs = CodeSpace::new(write_registry(ws_root));
        let err = parse_exec_err(
            cs.exec_command(Parameters(ExecCommandParams {
                workspace_id: WorkspaceId("demo".into()),
                command: vec![],
                work_id: None,
                tty: false,
            }))
            .await,
        );
        assert_eq!(err.code, ErrorCode::InvalidCommand);
        let started = exec_echo(&cs).await;
        assert_eq!(started.dispatch_status, ExecDispatchStatus::Confirmed);
    }

    #[tokio::test]
    async fn missing_executable_releases_lease_across_spawn_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let cs = CodeSpace::with_store(write_registry(ws_root), store.clone());
        assert_missing_exec_boundary_and_release(&cs, &store, false).await;
    }

    #[tokio::test]
    async fn tty_missing_executable_releases_lease_across_spawn_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let cs = CodeSpace::with_store(write_registry(ws_root), store.clone());
        assert_missing_exec_boundary_and_release(&cs, &store, true).await;
    }

    #[tokio::test]
    async fn uds_missing_executable_releases_lease_across_spawn_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let (worker, events) = host_worker();
        tokio::spawn(async move {
            serve_runner_connection(server, worker, events)
                .await
                .expect("serve");
        });
        let store = Arc::new(Store::memory().unwrap());
        let store_for_lease = store.clone();
        let runner = RuntimeBackend::Uds(UdsRunner::from_stream(
            client,
            Arc::new(move |process_id: &str| {
                store_for_lease.release_process(process_id);
            }),
        ));
        let cs = CodeSpace::with_store_and_runner(write_registry(ws_root), store.clone(), runner);
        assert_missing_exec_boundary_and_release(&cs, &store, false).await;
    }

    #[tokio::test]
    async fn linux_container_apply_patch_is_unauthorized_without_operation() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("ws");
        std::fs::create_dir(&ws_root).unwrap();
        let mut registry = Registry::new();
        let mut ws = Workspace::new(
            WorkspaceId("demo".into()),
            ws_root,
            codespace_domain::Profile::WorkspaceWrite,
        );
        ws.environment_kind = EnvironmentKind::LinuxContainer;
        registry.insert(ws);
        let cs = CodeSpace::new(registry);
        let err = cs
            .apply_patch_inner(
                ApplyPatchParams {
                    workspace_id: WorkspaceId("demo".into()),
                    patch: "*** Begin Patch\n*** Add File: a.txt\n+x\n*** End Patch\n".into(),
                    expected_versions: BTreeMap::new(),
                    operation_key: Some(OperationKey("k-box".into())),
                    check_only: false,
                    work_id: None,
                },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(err.operation_id.is_none());
    }

    /// A registry whose `gov` workspaces require resource participation, or set it to
    /// `participation`, beside an ordinary `demo` workspace, all on `root` (CSRG-U2).
    pub(crate) fn participation_registry(root: &std::path::Path, participation: &str) -> Registry {
        let workspace = |approvals: &str| {
            serde_json::json!({
                "root": root,
                "profile": "workspace-write",
                "approvals": approvals,
                "resources": {"participation": participation},
            })
        };
        let config = serde_json::json!({"workspaces": {
            "demo": {"root": root, "profile": "workspace-write"},
            "gov": workspace("off"),
            "gov-confirm": workspace("confirm"),
        }});
        Registry::load_json(&config.to_string()).unwrap()
    }

    pub(crate) fn exec_params(workspace: &str, command: &[&str], tty: bool) -> ExecCommandParams {
        ExecCommandParams {
            workspace_id: WorkspaceId(workspace.into()),
            command: command.iter().map(|part| part.to_string()).collect(),
            work_id: None,
            tty,
        }
    }

    /// Every new execution in a `gov` workspace is refused, before any approval or lease,
    /// stating `readiness`; nothing starts and the other tools keep working.
    pub(crate) async fn assert_required_starts_nothing(
        cs: &CodeSpace,
        store: &Store,
        root: &std::path::Path,
        readiness: &str,
    ) {
        for (workspace, tty) in [("gov", false), ("gov", true), ("gov-confirm", false)] {
            let touch = format!("{}/started", root.display());
            // A refusal names the registration from a session the owner opened for it.
            // DevGuard reads the consumer secret within 250 ms of wall time, so a session whose
            // thread the scheduler held that long reports `credential_unavailable` although the
            // file is intact; it opens nothing, and the refusal is asked again.
            let mut attempts = 0;
            let refused = loop {
                let refused = cs
                    .exec_command(Parameters(exec_params(
                        workspace,
                        &["/usr/bin/touch", &touch],
                        tty,
                    )))
                    .await
                    .err()
                    .expect("refused");
                attempts += 1;
                if attempts == 5
                    || readiness.contains("credential_unavailable")
                    || !refused.contains("(registration: credential_unavailable)")
                {
                    break refused;
                }
            };
            let body: ErrorBody = serde_json::from_str(&refused).unwrap();
            assert_eq!(body.code, ErrorCode::ResourcePolicyUnsupported, "{refused}");
            assert!(
                body.message.contains(&format!(
                    "workspace `{workspace}` requires resource participation: {readiness}"
                )),
                "{refused}"
            );
            assert!(
                body.message.ends_with(
                    "this gateway cannot admit executions through the resource authority, so \
                     nothing was started"
                ),
                "{refused}"
            );
            assert!(body.approval_id.is_none() && body.operation_id.is_none());
        }
        assert!(!root.join("started").exists());
        for workspace in ["gov", "gov-confirm"] {
            drop(
                store
                    .try_acquire_write(workspace)
                    .expect("no lease is held"),
            );
        }
        let read = cs
            .read(Parameters(ReadParams {
                workspace_id: WorkspaceId("gov".into()),
                path: "a.txt".into(),
                offset: None,
                limit: None,
                work_id: None,
            }))
            .await
            .unwrap();
        assert_eq!(read.0.content, "kept");
    }

    #[tokio::test]
    async fn a_workspace_that_requires_resource_participation_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let cs = CodeSpace::with_store(
            participation_registry(dir.path(), "required"),
            store.clone(),
        );
        #[cfg(not(feature = "devguard"))]
        let readiness = "this CodeSpace was built without DevGuard";
        #[cfg(feature = "devguard")]
        let readiness = "the gateway was not started with --devguard register";
        assert_required_starts_nothing(&cs, &store, dir.path(), readiness).await;
        // A workspace without it, and one that sets it off, run as before.
        let off = CodeSpace::with_store(participation_registry(dir.path(), "off"), store.clone());
        for (handler, workspace) in [(&cs, "demo"), (&off, "gov")] {
            let started = handler
                .exec_command(Parameters(exec_params(
                    workspace,
                    &["/bin/echo", "ok"],
                    false,
                )))
                .await
                .unwrap();
            assert_eq!(started.0.dispatch_status, ExecDispatchStatus::Confirmed);
        }
    }

    /// An approval granted while a workspace ran without resource participation starts
    /// nothing once the operator requires it.
    #[tokio::test]
    async fn a_resumed_approval_starts_nothing_once_participation_is_required() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let touch = format!("{}/started", dir.path().display());
        let before =
            CodeSpace::with_store(participation_registry(dir.path(), "off"), store.clone());
        let held = before
            .exec_command(Parameters(exec_params(
                "gov-confirm",
                &["/usr/bin/touch", &touch],
                false,
            )))
            .await
            .err()
            .expect("refused");
        let body: ErrorBody = serde_json::from_str(&held).unwrap();
        assert_eq!(body.code, ErrorCode::ApprovalRequired, "{held}");
        let approval_id = ApprovalId(body.approval_id.unwrap());
        before
            .approval_resolve(Parameters(ApprovalResolveParams {
                approval_id: approval_id.clone(),
                decision: codespace_domain::ApprovalDecision::Grant,
            }))
            .await
            .unwrap();
        // The same operations store under a gateway whose registry now requires it.
        let after = CodeSpace::with_store(
            participation_registry(dir.path(), "required"),
            store.clone(),
        );
        let refused = after
            .operation_resume(Parameters(OperationResumeParams { approval_id }))
            .await
            .err()
            .expect("refused");
        let body: ErrorBody = serde_json::from_str(&refused).unwrap();
        assert_eq!(body.code, ErrorCode::ResourcePolicyUnsupported, "{refused}");
        assert!(!dir.path().join("started").exists());
        drop(
            store
                .try_acquire_write("gov-confirm")
                .expect("no lease is held"),
        );
    }
}

#[cfg(all(test, feature = "devguard"))]
mod devguard_tests {
    use super::tests::{assert_required_starts_nothing, exec_params, participation_registry};
    use super::*;
    use crate::devguard::tests::{
        devguard_worker, execute_report, parse, past_the_credential, settings, short_directory,
        Endpoint, FixtureAuthority, SECRET,
    };
    use crate::devguard::{ResourceAuthority, WorkerSettings};
    use crate::runtime::RuntimeProcess;
    use clap::Parser;
    use codespace_devguard::Settings;
    use codespace_domain::{
        ProcessState, ProcessStatusParams, ResourceOwner, ResourceRegistrationInfo,
        ResourceRegistrationState, TerminateProcessParams, WorkspaceId,
    };
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn reported(handler: &CodeSpace) -> serde_json::Value {
        let Json(info) = handler
            .workspace_info(Parameters(WorkspaceInfoParams { workspace_id: None }))
            .await
            .unwrap();
        serde_json::to_value(info).unwrap()
    }

    /// `reported`, again while the authority's state is a stop at the credential (see
    /// `past_the_credential`).
    async fn reported_past_the_credential(handler: &CodeSpace) -> serde_json::Value {
        past_the_credential(
            |report: &serde_json::Value| {
                report["resource_authority"]["state"] == "credential_unavailable"
            },
            || reported(handler),
        )
        .await
    }

    #[tokio::test]
    async fn workspace_info_reports_the_authority_only_when_enabled() {
        let off = reported(&CodeSpace::new(Registry::new())).await;
        assert!(off.get("resource_authority").is_none());
        let plain = serde_json::to_value(lookup(&Registry::new(), None).unwrap()).unwrap();
        assert_eq!(off, plain);

        let dir = short_directory();
        let authority =
            ResourceAuthority::new(settings(dir.path(), dir.path().join("absent.sock")));
        let handler = CodeSpace::new(Registry::new()).with_resource_authority(authority);
        let on = reported_past_the_credential(&handler).await;
        assert_eq!(
            on["resource_authority"],
            serde_json::json!({
                "provider": "devguard",
                "participation": "status",
                "governs_execution": false,
                "state": "unavailable",
                "error_code": "resource_control_unavailable",
            })
        );
        assert!(!on.to_string().contains(SECRET));
    }

    /// Through the same construction as `main`: with every setting present, `off` opens no
    /// session and `status` opens one per call.
    #[tokio::test]
    async fn runtime_off_opens_no_session_through_the_gateway_handler() {
        let dir = short_directory();
        let socket = dir.path().join("watched.sock");
        let endpoint = Endpoint::start(&socket, drop);
        let credential = settings(dir.path(), socket.clone()).credential_file;
        for (mode, sessions) in [("off", 0), ("status", 1)] {
            let args: Vec<std::ffi::OsString> = vec![
                "codespace-mcp".into(),
                "--devguard".into(),
                mode.into(),
                "--devguard-socket".into(),
                socket.clone().into(),
                "--devguard-consumer".into(),
                "codespace".into(),
                "--devguard-generation".into(),
                "g1".into(),
                "--devguard-credential-file".into(),
                credential.clone().into(),
            ];
            let cli = crate::config::Cli::try_parse_from(args).unwrap();
            let store = Arc::new(Store::memory().unwrap());
            let handler = CodeSpace::from_cli(
                &cli,
                Registry::new(),
                store,
                RuntimeBackend::in_process(Arc::new(|_| {})),
            );
            let reported = reported_past_the_credential(&handler).await;
            assert_eq!(
                reported.get("resource_authority").is_some(),
                mode == "status"
            );
            // Let an attempted connection reach the endpoint before counting.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(endpoint.accepted(), sessions, "{mode}");
        }
    }

    #[tokio::test]
    async fn an_unknown_workspace_is_refused_before_any_session() {
        let dir = short_directory();
        let socket = dir.path().join("watched.sock");
        let endpoint = Endpoint::start(&socket, drop);
        let handler = CodeSpace::new(Registry::new())
            .with_resource_authority(ResourceAuthority::new(settings(dir.path(), socket)));
        let refused = handler
            .workspace_info(Parameters(WorkspaceInfoParams {
                workspace_id: Some("unknown".into()),
            }))
            .await;
        assert!(refused.is_err());
        assert_eq!(endpoint.accepted(), 0);
    }
    /// The gateway's settings for `--devguard register` with `settings`, then `runner`.
    fn register_cli(settings: &Settings, runner: &[&str]) -> crate::config::Cli {
        let mut args: Vec<OsString> = vec![
            "codespace-mcp".into(),
            "--devguard".into(),
            "register".into(),
            "--devguard-socket".into(),
            settings.socket.clone().into(),
            "--devguard-consumer".into(),
            settings.consumer.clone().into(),
            "--devguard-generation".into(),
            settings.generation.clone().into(),
            "--devguard-credential-file".into(),
            settings.credential_file.clone().into(),
        ];
        args.extend(runner.iter().map(OsString::from));
        crate::config::Cli::try_parse_from(args).unwrap()
    }

    fn release(store: &Arc<Store>) -> codespace_runner::ShellRelease {
        let store = store.clone();
        Arc::new(move |process_id: &str| store.release_process(process_id))
    }

    /// The registration reported by `n` calls that arrive together.
    async fn reported_together(handler: &CodeSpace, n: usize) -> Vec<serde_json::Value> {
        let calls: Vec<_> = (0..n)
            .map(|_| {
                let handler = handler.clone();
                tokio::spawn(async move { reported(&handler).await })
            })
            .collect();
        let mut reports = Vec::new();
        for call in calls {
            let report = call.await.unwrap();
            assert_eq!(report["resource_authority"]["governs_execution"], false);
            assert_eq!(
                report["resource_authority"]["participation"],
                "registration"
            );
            reports.push(report["resource_authority"]["registration"].clone());
        }
        reports
    }

    #[tokio::test]
    async fn status_participation_is_unchanged_and_never_starts_a_required_workspace() {
        let dir = short_directory();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let socket = dir.path().join("watched.sock");
        let endpoint = Endpoint::start(&socket, drop);
        let credential = settings(dir.path(), socket.clone()).credential_file;
        let args: Vec<OsString> = vec![
            "codespace-mcp".into(),
            "--devguard".into(),
            "status".into(),
            "--devguard-socket".into(),
            socket.into(),
            "--devguard-consumer".into(),
            "codespace".into(),
            "--devguard-generation".into(),
            "g1".into(),
            "--devguard-credential-file".into(),
            credential.into(),
        ];
        let cli = crate::config::Cli::try_parse_from(args).unwrap();
        let store = Arc::new(Store::memory().unwrap());
        let handler = CodeSpace::from_cli(
            &cli,
            participation_registry(dir.path(), "required"),
            store.clone(),
            RuntimeBackend::in_process(release(&store)),
        );
        let report = reported_past_the_credential(&handler).await;
        assert_eq!(report["resource_authority"]["participation"], "status");
        assert!(report["resource_authority"].get("registration").is_none());
        assert_required_starts_nothing(
            &handler,
            &store,
            dir.path(),
            "the gateway was not started with --devguard register",
        )
        .await;
        // Status participation opens one session per `workspace_info` and none per refusal.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(endpoint.accepted(), 1);
    }

    #[tokio::test]
    async fn a_required_workspace_is_prepared_by_the_owner_and_starts_nothing() {
        let dir = short_directory();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let socket = dir.path().join("authority.sock");
        // The endpoint closes each session at once: the owner is not registered.
        let endpoint = Endpoint::start(&socket, drop);
        let cli = register_cli(&settings(dir.path(), socket), &["--runner", "in-process"]);
        let store = Arc::new(Store::memory().unwrap());
        let handler = CodeSpace::from_cli(
            &cli,
            participation_registry(dir.path(), "required"),
            store.clone(),
            RuntimeBackend::in_process(release(&store)),
        );
        assert_governed_starts_nothing(
            &handler,
            &store,
            dir.path(),
            ErrorCode::ResourceAuthorityUnavailable,
            &[
                "the execution owner could not ask the resource authority (registration: unavailable",
                ": not_asked; nothing was started",
            ],
        )
        .await;
        // Each preparation came from a session the owner opened for it.
        assert_eq!(endpoint.accepted(), 3);
        // Existing process control never asks DevGuard.
        let started = handler
            .exec_command(Parameters(exec_params(
                "demo",
                &["/bin/sleep", "30"],
                false,
            )))
            .await
            .unwrap()
            .0;
        assert_process_control_needs_no_registration(&handler, started.process_id).await;
        assert_eq!(endpoint.accepted(), 3);
    }

    /// Status, termination and the outcome of a running process, without a registration.
    async fn assert_process_control_needs_no_registration(
        handler: &CodeSpace,
        process_id: ProcessId,
    ) {
        let status = handler
            .process_status(Parameters(ProcessStatusParams {
                process_id: process_id.clone(),
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(status.state, ProcessState::Running);
        handler
            .terminate_process(Parameters(TerminateProcessParams {
                process_id: process_id.clone(),
            }))
            .await
            .unwrap();
        for _ in 0..400 {
            let status = handler
                .process_status(Parameters(ProcessStatusParams {
                    process_id: process_id.clone(),
                }))
                .await
                .unwrap()
                .0;
            if status.state == ProcessState::Exited {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("{process_id:?} did not exit after termination");
    }

    /// Every new execution in a `gov` workspace goes to the execution owner's preparation and
    /// ends in a refusal with `code` whose message contains each of `says`: a pipe and a PTY command,
    /// and one held for approval, granted and resumed. Nothing starts, the workspace lease is
    /// released each time and the other tools keep working. Resuming the same approval again
    /// returns the stored refusal, so its admission is not asked again. Returns the attempt IDs.
    pub(crate) async fn assert_governed_starts_nothing(
        cs: &CodeSpace,
        store: &Store,
        root: &std::path::Path,
        code: ErrorCode,
        says: &[&str],
    ) -> Vec<String> {
        // DevGuard reads the consumer secret within 250 ms (see `past_the_credential`): such a
        // session opens nothing, and the call is made again.
        let stopped = |body: &ErrorBody| {
            code != ErrorCode::ResourceAuthorityUnavailable
                && body
                    .message
                    .contains("(registration: credential_unavailable")
        };
        let touch = format!("{}/started", root.display());
        let mut attempts = Vec::new();
        let mut check = |body: &ErrorBody, raw: &str| {
            assert_eq!(body.code, code, "{raw}");
            for said in says {
                assert!(body.message.contains(said), "{raw}");
            }
            assert!(body.message.ends_with("; nothing was started"), "{raw}");
            let attempt = body
                .message
                .split("attempt `")
                .nth(1)
                .and_then(|rest| rest.split('`').next())
                .unwrap_or_else(|| panic!("no attempt in {raw}"))
                .to_owned();
            assert!(attempt.starts_with("cs-attempt-"), "{raw}");
            assert!(!attempts.contains(&attempt), "attempt reused: {raw}");
            attempts.push(attempt);
        };
        for tty in [false, true] {
            let mut tries = 0;
            let (body, raw) = loop {
                let raw = cs
                    .exec_command(Parameters(exec_params(
                        "gov",
                        &["/usr/bin/touch", &touch],
                        tty,
                    )))
                    .await
                    .err()
                    .expect("refused");
                let body: ErrorBody = serde_json::from_str(&raw).unwrap();
                tries += 1;
                if tries == 5 || !stopped(&body) {
                    break (body, raw);
                }
            };
            check(&body, &raw);
            drop(
                store
                    .try_acquire_write("gov")
                    .expect("the lease is released"),
            );
        }
        let mut tries = 0;
        let (body, raw, approval_id) = loop {
            let held = cs
                .exec_command(Parameters(exec_params(
                    "gov-confirm",
                    &["/usr/bin/touch", &touch],
                    false,
                )))
                .await
                .err()
                .expect("held");
            let held: ErrorBody = serde_json::from_str(&held).unwrap();
            assert_eq!(held.code, ErrorCode::ApprovalRequired);
            let approval_id = ApprovalId(held.approval_id.unwrap());
            cs.approval_resolve(Parameters(codespace_domain::ApprovalResolveParams {
                approval_id: approval_id.clone(),
                decision: codespace_domain::ApprovalDecision::Grant,
            }))
            .await
            .unwrap();
            let raw = cs
                .operation_resume(Parameters(OperationResumeParams {
                    approval_id: approval_id.clone(),
                }))
                .await
                .err()
                .expect("refused");
            let body: ErrorBody = serde_json::from_str(&raw).unwrap();
            tries += 1;
            if tries == 5 || !stopped(&body) {
                break (body, raw, approval_id);
            }
        };
        check(&body, &raw);
        let replayed = cs
            .operation_resume(Parameters(OperationResumeParams { approval_id }))
            .await
            .err()
            .expect("refused");
        let replayed: ErrorBody = serde_json::from_str(&replayed).unwrap();
        assert_eq!(
            (replayed.code, replayed.message.as_str()),
            (body.code, body.message.as_str())
        );
        drop(
            store
                .try_acquire_write("gov-confirm")
                .expect("the lease is released"),
        );
        assert!(!root.join("started").exists(), "a governed command ran");
        let read = cs
            .read(Parameters(ReadParams {
                workspace_id: WorkspaceId("gov".into()),
                path: "a.txt".into(),
                offset: None,
                limit: None,
                work_id: None,
            }))
            .await
            .unwrap();
        assert_eq!(read.0.content, "kept");
        attempts
    }

    /// The gateway registers itself when it runs the executions (InProcess), against
    /// DevGuard's fixture authority in its own process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_in_process_gateway_registers_itself_as_the_execution_owner() {
        let Some(mut fixture) = FixtureAuthority::start(2) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let cli = register_cli(&fixture.settings, &["--runner", "in-process"]);
        let store = Arc::new(Store::memory().unwrap());
        let handler = CodeSpace::from_cli(
            &cli,
            participation_registry(dir.path(), "required"),
            store.clone(),
            RuntimeBackend::in_process(release(&store)),
        );
        // Calls that arrive together while the startup registration runs.
        handler.register_owner_now();
        let me = std::process::id();
        let together = past_the_credential(
            |registrations: &Vec<serde_json::Value>| {
                registrations
                    .iter()
                    .any(|registration| registration["state"] == "credential_unavailable")
            },
            || reported_together(&handler, 8),
        )
        .await;
        let expected = if fixture.native {
            serde_json::json!({"owner": "in_process", "state": "registered", "pid": me})
        } else {
            serde_json::json!({
                "owner": "in_process",
                "state": "incompatible",
                "error_code": "resource_policy_unsupported",
            })
        };
        for registration in together {
            assert_eq!(registration, expected);
        }
        if fixture.native {
            // One instance across every session, and it is this process.
            let instances = fixture.instances();
            assert_eq!(instances.len(), 1, "{instances:?}");
            assert_eq!(instances[0].1, me);
        }
        assert_governed(&mut fixture, &handler, &store, dir.path()).await;
        if fixture.native {
            assert_eq!(fixture.instances().len(), 1);
        }
    }

    /// With native evidence DevGuard admits each governed execution and CodeSpace cancels it,
    /// as managed launch is not available; without it the owner cannot register.
    async fn assert_governed(
        fixture: &mut FixtureAuthority,
        handler: &CodeSpace,
        store: &Store,
        root: &std::path::Path,
    ) {
        let (code, says): (_, &[&str]) = if fixture.native {
            (
                ErrorCode::ManagedLaunchUnavailable,
                &[
                    "the resource authority admitted it, but managed launch is not available yet",
                    ": cancelled; nothing was started",
                ],
            )
        } else {
            (
                ErrorCode::ResourceAuthorityUnavailable,
                &[
                    "(registration: incompatible, resource_policy_unsupported)",
                    ": not_asked; nothing was started",
                ],
            )
        };
        assert_governed_starts_nothing(handler, store, root, code, says).await;
        // Nothing the authority admitted is still charged.
        assert_eq!(fixture.charged(), (Vec::new(), 0));
    }

    /// The operator's request decides what DevGuard is asked for: more memory than it can
    /// give is a shortage, kernel control it cannot give is unsupported. Neither reserves or
    /// starts anything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shortage_or_an_unsupported_minimum_is_refused_and_starts_nothing() {
        let Some(mut fixture) = FixtureAuthority::start(2) else {
            return;
        };
        if !fixture.native {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let touch = format!("{}/started", dir.path().display());
        let request = |memory: u64, minimum: &str| {
            serde_json::json!({
                "root": dir.path(),
                "profile": "workspace-write",
                "resources": {"participation": "required", "request": {
                    "cpu_milli": 100, "memory_bytes": memory, "tasks": 4,
                    "minimum": {"cpu": "accounted", "memory": minimum, "pids": "accounted"},
                }},
            })
        };
        let config = serde_json::json!({"workspaces": {
            "short": request(1 << 60, "accounted"),
            "kernel": request(1 << 20, "kernel"),
        }});
        let cli = register_cli(&fixture.settings, &["--runner", "in-process"]);
        let store = Arc::new(Store::memory().unwrap());
        let handler = CodeSpace::from_cli(
            &cli,
            Registry::load_json(&config.to_string()).unwrap(),
            store.clone(),
            RuntimeBackend::in_process(release(&store)),
        );
        for (workspace, code, denial) in [
            (
                "short",
                ErrorCode::ResourceUnavailable,
                "resource_unavailable",
            ),
            (
                "kernel",
                ErrorCode::ResourcePolicyUnsupported,
                "resource_policy_unsupported",
            ),
        ] {
            let body = past_the_credential(
                |body: &ErrorBody| body.message.contains("credential_unavailable"),
                || async {
                    let raw = handler
                        .exec_command(Parameters(exec_params(
                            workspace,
                            &["/usr/bin/touch", &touch],
                            false,
                        )))
                        .await
                        .err()
                        .expect("refused");
                    serde_json::from_str::<ErrorBody>(&raw).unwrap()
                },
            )
            .await;
            assert_eq!(body.code, code, "{}", body.message);
            assert!(
                body.message
                    .contains(&format!("the resource authority denied it ({denial})")),
                "{}",
                body.message
            );
            assert!(body.message.ends_with(": denied; nothing was started"));
            drop(
                store
                    .try_acquire_write(workspace)
                    .expect("the lease is released"),
            );
        }
        assert!(!dir.path().join("started").exists());
        assert_eq!(fixture.charged(), (Vec::new(), 0));
    }

    /// A worker that takes a preparation and is lost before answering may have asked DevGuard
    /// to admit it: the gateway reports the admission unknown, not refused, starts nothing and
    /// releases the workspace.
    #[tokio::test]
    async fn a_preparation_lost_with_the_worker_is_unknown() {
        use codespace_runner::{
            read_frame, write_frame, RunnerOp, RunnerOpResult, UdsRunner, WireEnvelope,
            WIRE_PROTOCOL,
        };
        let dir = short_directory();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let (client, mut worker) = tokio::net::UnixStream::pair().unwrap();
        let prepared = Arc::new(AtomicBool::new(false));
        let seen = prepared.clone();
        tokio::spawn(async move {
            while let Ok(Some(frame)) = read_frame(&mut worker).await {
                let request: WireEnvelope = serde_json::from_slice(&frame).unwrap();
                match request.op {
                    Some(RunnerOp::Hello) => {
                        let hello = RunnerOpResult::Hello {
                            protocol: WIRE_PROTOCOL,
                            registration: true,
                            admission: true,
                        };
                        let reply = WireEnvelope::response(request.request_id.unwrap(), Ok(hello));
                        write_frame(&mut worker, &reply).await.unwrap();
                    }
                    // Lost with the worker before it answers.
                    Some(RunnerOp::Prepare { .. }) => {
                        seen.store(true, Ordering::SeqCst);
                        return;
                    }
                    _ => return,
                }
            }
        });
        let store = Arc::new(Store::memory().unwrap());
        let runner = UdsRunner::from_stream(client, release(&store));
        runner.handshake().await.unwrap();
        let handler = CodeSpace::with_store_and_runner(
            participation_registry(dir.path(), "required"),
            store.clone(),
            RuntimeBackend::Uds(runner),
        )
        .with_registration(crate::devguard::Registration::new(
            settings(dir.path(), dir.path().join("absent.sock")),
            crate::config::RunnerMode::Uds,
            true,
        ));
        let touch = format!("{}/started", dir.path().display());
        let raw = handler
            .exec_command(Parameters(exec_params(
                "gov",
                &["/usr/bin/touch", &touch],
                false,
            )))
            .await
            .err()
            .expect("refused");
        let body: ErrorBody = serde_json::from_str(&raw).unwrap();
        assert!(prepared.load(Ordering::SeqCst));
        assert_eq!(body.code, ErrorCode::AdmissionUnknown, "{raw}");
        assert!(
            body.message.ends_with(": unknown; nothing was started"),
            "{raw}"
        );
        drop(
            store
                .try_acquire_write("gov")
                .expect("the lease is released"),
        );
        assert!(!dir.path().join("started").exists());
        // The connection is gone, so the next request was never sent: nothing was asked.
        let raw = handler
            .exec_command(Parameters(exec_params(
                "gov",
                &["/usr/bin/touch", &touch],
                false,
            )))
            .await
            .err()
            .expect("refused");
        let body: ErrorBody = serde_json::from_str(&raw).unwrap();
        assert_eq!(body.code, ErrorCode::ResourceAuthorityUnavailable, "{raw}");
        assert!(
            body.message.contains("(registration: owner_unreachable)"),
            "{raw}"
        );
        assert!(
            body.message.ends_with(": not_asked; nothing was started"),
            "{raw}"
        );
    }

    /// The UDS worker registers itself and the gateway does not, against DevGuard's fixture
    /// authority in its own process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_uds_worker_registers_itself_and_the_gateway_does_not() {
        let Some(mut fixture) = FixtureAuthority::start(2) else {
            return;
        };
        let Some(bin) = devguard_worker() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "kept").unwrap();
        let cli = register_cli(
            &fixture.settings,
            &["--runner", "uds", "--runtime-bin", bin.to_str().unwrap()],
        );
        let store = Arc::new(Store::memory().unwrap());
        let worker = cli.devguard.worker().unwrap();
        let (mut process, runner) =
            RuntimeProcess::spawn_registered(&bin, None, release(&store), store.clone(), &worker)
                .await
                .unwrap();
        let worker_pid = process.worker_pid().unwrap();
        assert_ne!(worker_pid, std::process::id());
        let handler = CodeSpace::from_cli(
            &cli,
            participation_registry(dir.path(), "required"),
            store.clone(),
            RuntimeBackend::Uds(runner),
        );
        let expected = if fixture.native {
            serde_json::json!({"owner": "worker", "state": "registered", "pid": worker_pid})
        } else {
            serde_json::json!({
                "owner": "worker",
                "state": "incompatible",
                "error_code": "resource_policy_unsupported",
            })
        };
        // Calls that arrive together while the worker's startup registration runs.
        for registration in reported_together(&handler, 8).await {
            assert_eq!(registration, expected);
        }
        if fixture.native {
            // One instance, the worker's; the gateway is not registered.
            let instances = fixture.instances();
            assert_eq!(instances.len(), 1, "{instances:?}");
            assert_eq!(instances[0].1, worker_pid);
        }
        // The worker runs, observes and terminates processes without asking DevGuard.
        let started = handler
            .exec_command(Parameters(exec_params(
                "demo",
                &["/bin/sleep", "30"],
                false,
            )))
            .await
            .unwrap()
            .0;
        assert_process_control_needs_no_registration(&handler, started.process_id).await;
        assert_governed(&mut fixture, &handler, &store, dir.path()).await;
        if fixture.native {
            assert_eq!(fixture.instances().len(), 1);
        }
        process.wait_exit().await;
    }

    #[tokio::test]
    async fn a_worker_without_a_usable_credential_reports_it_and_opens_no_session() {
        let Some(bin) = devguard_worker() else {
            return;
        };
        let dir = short_directory();
        let socket = dir.path().join("authority.sock");
        let endpoint = Endpoint::start(&socket, drop);
        let mut unusable = settings(dir.path(), socket);
        unusable.credential_file = dir.path().join("missing.secret");
        let store = Arc::new(Store::memory().unwrap());
        let (mut process, runner) = RuntimeProcess::spawn_registered(
            &bin,
            None,
            release(&store),
            store.clone(),
            &WorkerSettings::from_settings(unusable),
        )
        .await
        .unwrap();
        let info = runner.registration().await.unwrap();
        assert_eq!(
            info.registration,
            Some(ResourceRegistrationInfo {
                owner: ResourceOwner::Worker,
                state: ResourceRegistrationState::CredentialUnavailable,
                error_code: None,
                pid: None,
            })
        );
        assert_eq!(endpoint.accepted(), 0);
        process.wait_exit().await;
    }

    /// The worker's registration sessions, opened while it starts children (#79's inheritance
    /// case against the worker). `CODESPACE_D6_CHILDREN` sets how many children each mode
    /// starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_worker_registration_sessions_reach_none_of_its_children() {
        let Some(bin) = devguard_worker() else {
            return;
        };
        let per_mode: usize = std::env::var("CODESPACE_D6_CHILDREN")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        let dir = short_directory();
        let socket = dir.path().join("authority.sock");
        // Each session is closed at once, so the worker keeps creating session sockets.
        let endpoint = Endpoint::start(&socket, drop);
        let store = Arc::new(Store::memory().unwrap());
        let (mut process, runner) = RuntimeProcess::spawn_registered(
            &bin,
            None,
            release(&store),
            store.clone(),
            &WorkerSettings::from_settings(settings(dir.path(), socket)),
        )
        .await
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(
            WorkspaceId("demo".into()),
            root.path().to_path_buf(),
            codespace_domain::Profile::WorkspaceWrite,
        );
        let baseline = [
            parse(&execute_report(&runner, &workspace, 0, false).await).descriptors,
            parse(&execute_report(&runner, &workspace, 0, true).await).descriptors,
        ];
        let stop = Arc::new(AtomicBool::new(false));
        let registering = {
            let (runner, stop) = (runner.clone(), stop.clone());
            tokio::spawn(async move {
                let mut sessions = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let info = runner.registration().await.unwrap();
                    assert_eq!(
                        info.registration.unwrap().state,
                        ResourceRegistrationState::Unavailable
                    );
                    sessions += 1;
                }
                sessions
            })
        };
        let mut holders = [0usize; 2];
        for n in 1..=per_mode {
            for (mode, tty) in [false, true].into_iter().enumerate() {
                let report = parse(&execute_report(&runner, &workspace, n, tty).await);
                assert!(!report.environment.contains(SECRET), "{report:?}");
                assert_eq!(report.descriptors, baseline[mode], "tty {tty}: {report:?}");
                holders[mode] += usize::from(!report.sockets.is_empty());
            }
        }
        stop.store(true, Ordering::Relaxed);
        let sessions = registering.await.unwrap();
        assert!(sessions > 0 && endpoint.accepted() > 0);
        assert_eq!(holders, [0, 0]);
        eprintln!(
            "worker d6: {sessions} registrations, {} sessions; pipe 0/{per_mode} and pty 0/{per_mode} held a socket",
            endpoint.accepted()
        );
        process.wait_exit().await;
    }
}
