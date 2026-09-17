use std::borrow::Cow;
use std::sync::Arc;

use codespace_domain::{
    workspace_info, ApplyPatchParams, ApplyPatchResult, CoordinationHint, ErrorBody, ErrorCode,
    ExecCommandParams, ExecCommandResult, FindParams, FindResult, OperationStatusParams,
    OperationStatusResult, PatchStatus, ProcessId, ReadParams, ReadProcessParams,
    ReadProcessResult, ReadResult, SteerClaimNextResult, SteerCompleteParams, SteerStatusResult,
    TerminateProcessParams, WorkFinishResult, WorkId, WorkIdParams, WorkOpenParams, WorkOpenResult,
    WorkspaceInfo, WorkspaceInfoParams, WriteStdinParams,
};
use codespace_policy::{allow, Action, ClientClaims, Registry};
use codespace_runner::{
    ContainerRunner, Runner, RunnerApplyPatchRequest, RunnerExecRequest, RunnerReadProcess,
    RunnerWriteStdin, RuntimeBackend,
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
        let on_release = Arc::new(move |workspace_id: &str| {
            store_for_lease.clear_shell(workspace_id);
        });
        let runner = match std::env::var("CODESPACE_RUNNER") {
            Ok(mode) if mode == "uds" => {
                let path = std::env::var("CODESPACE_RUNNER_SOCKET")
                    .expect("CODESPACE_RUNNER_SOCKET is required when CODESPACE_RUNNER=uds");
                RuntimeBackend::Container(connect_container_runner(&path))
            }
            _ => RuntimeBackend::in_process(on_release),
        };
        Self::with_store_and_runner(registry, store, runner)
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
        description = "Return CodeSpace identity. Does not call a model. Does not read files. workspace_id is a selector, not a credential."
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
            .map_err(err_json)
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
            .map_err(err_json)
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex V4A patch. check_only verifies without writing and returns status checked. status applied means disk hashes match the helper claim. Never falls back to git apply."
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
        description = "Start a managed argv in the workspace cwd. Returns a server-minted process_id. Does not use a login shell. Request end does not kill the process. Host process today."
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
        ws.require_host_execution().map_err(err_json)?;
        if params.command.is_empty() || params.command[0].is_empty() {
            return Err(err_json(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "command must be a non-empty argv (no shell)",
            )));
        }
        let process_id = ProcessId(format!("proc-{}", Uuid::new_v4()));
        self.store
            .mark_shell_busy(&params.workspace_id.0, &process_id.0)
            .map_err(err_json)?;
        match self
            .runner
            .exec(
                ws,
                RunnerExecRequest::for_host(
                    params.command,
                    process_id,
                    ws.root.clone(),
                    ws.profile,
                ),
            )
            .await
        {
            Ok(result) => {
                if matches!(self.runner, RuntimeBackend::Container(_)) {
                    reap_remote_shell(
                        self.store.clone(),
                        self.runner.clone(),
                        params.workspace_id.0.clone(),
                        result.process_id.clone(),
                    );
                }
                Ok(Json(ExecCommandResult {
                    process_id: result.process_id,
                    coordination: self.hint(&params.workspace_id.0, params.work_id.as_ref()),
                }))
            }
            Err(err) => {
                self.store.clear_shell(&params.workspace_id.0);
                Err(err_json(err))
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
            .map_err(err_json)?;
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
            .map_err(err_json)?;
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
            .map_err(err_json)?;
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
        ws.require_host_execution()?;
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
                    Err(err) => {
                        let failed =
                            ApplyPatchResult::new(PatchStatus::Rejected, operation_id.clone());
                        let _ = self.store.finish(&operation_id, &failed);
                        return Err(err.with_operation_id(operation_id.0));
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

fn connect_container_runner(path: &str) -> ContainerRunner {
    let std_stream = std::os::unix::net::UnixStream::connect(path).unwrap_or_else(|err| {
        panic!("connect runner socket {path}: {err}");
    });
    std_stream
        .set_nonblocking(true)
        .expect("runner socket nonblocking");
    let stream = tokio::net::UnixStream::from_std(std_stream).expect("tokio runner socket");
    ContainerRunner::from_stream(stream)
}

fn reap_remote_shell(
    store: Arc<Store>,
    runner: RuntimeBackend,
    workspace_id: String,
    process_id: ProcessId,
) {
    tokio::spawn(async move {
        for _ in 0..600 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            match runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
            {
                Ok(result) if result.eof => break,
                Err(_) => break,
                _ => {}
            }
        }
        store.clear_shell(&workspace_id);
    });
}

fn lookup(registry: &Registry, workspace_id: Option<String>) -> Result<WorkspaceInfo, ErrorBody> {
    let Some(id) = workspace_id.filter(|s| !s.is_empty()) else {
        return Ok(workspace_info(None));
    };
    let ws = registry.get(&id)?;
    let mut info = workspace_info(Some(id));
    info.profile = Some(ws.profile);
    info.root = Some(ws.root.display().to_string());
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
            .with_instructions(
                "CodeSpace execution-tools MCP. No internal model calls. workspace_id and work_id are selectors. process_id is server-minted. Request end does not kill a process. Claim user intents only at major checkpoints and before work_finish."
                    .to_string(),
            )
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
