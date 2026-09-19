use std::borrow::Cow;
use std::sync::Arc;

use codespace_domain::{
    workspace_info, ApplyPatchParams, ApplyPatchResult, ClientEnvironmentKind, CoordinationHint,
    EffectivePermissionInfo, EnvironmentExecutionInfo, ErrorBody, ErrorCode, ExecCommandParams,
    ExecCommandResult, ExecDispatchStatus, FindParams, FindResult, NetworkPolicyState,
    OperationStatusParams, OperationStatusResult, PatchStatus, ProcessId, ReadParams,
    ReadProcessParams, ReadProcessResult, ReadResult, SteerClaimNextResult, SteerCompleteParams,
    SteerStatusResult, TerminateProcessParams, WorkFinishResult, WorkId, WorkIdParams,
    WorkOpenParams, WorkOpenResult, WorkspaceExecutionInfo, WorkspaceInfo, WorkspaceInfoParams,
    WriteStdinParams,
};
use codespace_policy::{
    allow, Action, ClientClaims, EnvironmentKind, NetworkAxis, PermissionProfile, Registry,
    Workspace,
};
use codespace_runner::{
    linux_sandbox_available, Runner, RunnerApplyPatchRequest, RunnerError, RunnerExecRequest,
    RunnerReadProcess, RunnerWriteStdin, RuntimeBackend,
};
use codespace_store::{Begin, Store};
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
}

fn err_json(err: ErrorBody) -> String {
    serde_json::to_string(&err).unwrap_or(err.message)
}

const MCP_INSTRUCTIONS: &str = "\
CodeSpace is an execution-only MCP and never calls a model.

Use workspace-relative paths for file tools. workspace_id and work_id are \
selectors, not credentials.

exec_command accepts argv; there is no implicit shell. It runs in the \
workspace cwd and returns a server-minted process_id. Ending an MCP request \
does not terminate the process.

A live managed process holds the workspace mutation lease. read and find may \
continue, but apply_patch or another exec_command may return WORKSPACE_BUSY \
until the process exits or is terminated.

exec_command.tty is optional and defaults to false. tty=true attaches a \
fixed 24x80 PTY. PTY resize is not currently supported. Use tty=true only \
when the command requires terminal semantics or an interactive TUI.

Executable workspaces currently use host execution. When \
workspace_info.execution.isolation.command_sandbox is linux-sandbox, \
exec_command is wrapped by the Linux helper. When it is none, host \
execution is not an OS command sandbox. Network policy is reported by \
workspace_info. OS network enforcement follows \
execution.network.enforcement; absence of enforcement is not permission.

Treat apply_patch status=unknown as possibly executed. Do not blindly retry \
the mutation with a new operation_key.

If exec_command reports dispatch_status=unknown, the spawn may have occurred. \
Do not blindly start a duplicate process. The returned process_id identifies \
the uncertain attempt. Use read_process or terminate_process when the backend \
remains reachable; do not assume that unknown means the process did not start.

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
        }
    }

    #[tool(
        name = "workspace_info",
        description = "Return CodeSpace identity and, when workspace_id is set, the effective execution contract. files.*.available and process.available reflect permission and backend support only. They do not include transient workspace occupancy; exec_command or apply_patch may still return WORKSPACE_BUSY. Tool existence is reported separately by tools_exposed. Does not call a model. Does not read files. workspace_id is a selector, not a credential."
    )]
    async fn workspace_info(
        &self,
        Parameters(params): Parameters<WorkspaceInfoParams>,
    ) -> Result<Json<WorkspaceInfo>, String> {
        lookup(&self.registry, params.workspace_id)
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "read",
        description = "Read a relative workspace file and return content plus a sha256 version. Rejects symlinks, special files, and path escape."
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
            .read(ws, &params.path)
            .await
            .map(|mut result| {
                result.coordination = self.hint(&params.workspace_id.0, params.work_id.as_ref());
                Json(result)
            })
            .map_err(runner_err_json)
    }

    #[tool(
        name = "find",
        description = "List relative file paths in a workspace. Does not follow symlinks."
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
            .find(ws, params.glob.as_deref())
            .await
            .map(|mut result| {
                result.coordination = self.hint(&params.workspace_id.0, params.work_id.as_ref());
                Json(result)
            })
            .map_err(runner_err_json)
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex V4A patch. check_only verifies without writing and returns status checked. status applied means disk hashes match the helper claim. Never falls back to git apply. status=unknown means the mutation may have executed but its result could not be confirmed. Do not retry the same mutation under a new operation_key. operation_key provides replay/idempotency for the same logical mutation."
    )]
    async fn apply_patch(
        &self,
        Parameters(params): Parameters<ApplyPatchParams>,
    ) -> Result<Json<ApplyPatchResult>, String> {
        self.apply_patch_inner(params)
            .await
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "operation_status",
        description = "Look up an operation by exactly one of operation_id or operation_key. Does not re-run the operation. Distinct from HTTP request ids."
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
        description = "Start a managed argv in the workspace cwd. There is no implicit shell. Returns a server-minted process_id and a dispatch_status. Request end does not terminate the process. Omitted or false tty uses pipes. tty=true attaches a fixed 24x80 PTY; resize is not supported. Use tty only for commands requiring terminal semantics or an interactive TUI. A live process holds the workspace mutation lease, so another exec_command or apply_patch may return WORKSPACE_BUSY until it exits or is terminated. Use write_stdin, read_process, and terminate_process with the returned process_id. dispatch_status=unknown means the spawn may have occurred. Do not blindly start a duplicate process. The returned process_id identifies the uncertain attempt. Use read_process or terminate_process when the backend remains reachable; do not assume that unknown means the process did not start. PROCESS_SPAWN_FAILED means the backend confirmed that no managed process was started; it is distinct from dispatch_status=unknown."
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
            return Err(err_json(ErrorBody::new(
                ErrorCode::InvalidCommand,
                "command must be a non-empty argv (no shell)",
            )));
        }
        let process_id = ProcessId(format!("proc-{}", Uuid::new_v4()));
        self.store
            .mark_shell_busy(&params.workspace_id.0, &process_id.0)
            .map_err(err_json)?;
        let mut req = RunnerExecRequest::for_host(params.command, process_id.clone(), ws.profile);
        req.policy.network = PermissionProfile::from_workspace_profile(ws.profile).network;
        req.tty = params.tty;
        match self.runner.exec(ws, req).await {
            Ok(result) => Ok(Json(ExecCommandResult {
                process_id: result.process_id,
                dispatch_status: ExecDispatchStatus::Confirmed,
                coordination: self.hint(&params.workspace_id.0, params.work_id.as_ref()),
            })),
            Err(RunnerError::TransportAmbiguous { .. }) => Ok(Json(ExecCommandResult {
                process_id,
                dispatch_status: ExecDispatchStatus::Unknown,
                coordination: self.hint(&params.workspace_id.0, params.work_id.as_ref()),
            })),
            Err(err) => {
                self.store.release_process(&process_id.0);
                Err(runner_err_json(err))
            }
        }
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
        description = "Read output from a managed process starting at cursor. Output is bounded; process_id cannot be invented."
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
}

impl CodeSpace {
    async fn apply_patch_inner(
        &self,
        params: ApplyPatchParams,
    ) -> Result<ApplyPatchResult, ErrorBody> {
        let ws = self.registry.get(&params.workspace_id.0)?;
        codespace_policy::allow(ws, Action::Write, &ClientClaims::default())?;
        ws.require_file_write()?;
        let _lease = self.store.try_acquire_write(&params.workspace_id.0)?;
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
    let policy = PermissionProfile::from_workspace_profile(ws.profile);
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
    let info = WorkspaceExecutionInfo::from_effective(
        environment,
        permissions,
        client_network_policy(policy.network),
    );
    advertise_linux_sandbox(info, policy.network)
}

fn advertise_linux_sandbox(
    info: WorkspaceExecutionInfo,
    network: NetworkAxis,
) -> WorkspaceExecutionInfo {
    if linux_sandbox_available() {
        info.with_linux_command_sandbox(matches!(network, NetworkAxis::Restricted))
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
        let policy = PermissionProfile::from_workspace_profile(ws.profile);
        assert_eq!(exec.permissions.read, policy.allows(Action::Read));
        assert_eq!(exec.permissions.write, policy.allows(Action::Write));
        assert_eq!(exec.permissions.exec, policy.allows(Action::Exec));
        assert_eq!(exec.network.policy, client_network_policy(policy.network));
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
            if exec.network.policy == NetworkPolicyState::Restricted {
                assert_eq!(exec.network.enforcement, NetworkEnforcementState::Enforced);
            }
        } else {
            assert_eq!(exec.isolation.command_sandbox, CommandSandboxState::None);
            assert_eq!(exec.network.enforcement, NetworkEnforcementState::None);
        }
    }

    fn assert_instructions_cover_execution_contract(text: &str) {
        assert!(text.contains("never calls a model"), "{text}");
        assert!(
            text.contains("Ending an MCP request does not terminate the process"),
            "{text}"
        );
        assert!(text.contains("fixed 24x80 PTY"), "{text}");
        assert!(
            text.contains("PTY resize is not currently supported"),
            "{text}"
        );
        assert!(text.contains("WORKSPACE_BUSY"), "{text}");
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
        assert!(!text.contains("not granted by policy"), "{text}");
        assert!(
            text.contains("Treat apply_patch status=unknown as possibly executed"),
            "{text}"
        );
        assert!(text.contains("dispatch_status=unknown"), "{text}");
        assert!(text.contains("uncertain attempt"), "{text}");
        assert!(text.contains("backend remains reachable"), "{text}");
        assert!(
            !text.contains("inspect or terminate that handle rather than"),
            "{text}"
        );
        assert!(text.contains("major checkpoints"), "{text}");
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
        assert!(exec.process.available);
        let tty = &exec
            .process
            .capabilities
            .as_ref()
            .expect("capabilities")
            .tty;
        assert!(tty.supported);
        assert!(!tty.resize_supported);
        assert_eq!(exec.network.policy, NetworkPolicyState::Restricted);
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("environment_id").is_none());
        assert!(!json.to_string().contains("\"environment_id\""));
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
            .apply_patch_inner(ApplyPatchParams {
                workspace_id: WorkspaceId("demo".into()),
                patch: "*** Begin Patch\n*** Add File: lost.txt\n+x\n*** End Patch\n".into(),
                expected_versions: BTreeMap::new(),
                operation_key: Some(OperationKey("k-unknown".into())),
                check_only: false,
                work_id: None,
            })
            .await
            .unwrap();
        assert_eq!(result.status, PatchStatus::Unknown);
        let stored = store.get(&result.operation_id).unwrap();
        assert_eq!(stored.status, PatchStatus::Unknown);
        assert_ne!(stored.status, PatchStatus::Rejected);
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
            .apply_patch_inner(ApplyPatchParams {
                workspace_id: WorkspaceId("demo".into()),
                patch: "*** Begin Patch\n*** Add File: later.txt\n+x\n*** End Patch\n".into(),
                expected_versions: BTreeMap::new(),
                operation_key: None,
                check_only: false,
                work_id: None,
            })
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
            .apply_patch_inner(ApplyPatchParams {
                workspace_id: WorkspaceId("demo".into()),
                patch: "*** Begin Patch\n*** Add File: a.txt\n+x\n*** End Patch\n".into(),
                expected_versions: BTreeMap::new(),
                operation_key: Some(OperationKey("k-box".into())),
                check_only: false,
                work_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(err.operation_id.is_none());
    }
}
