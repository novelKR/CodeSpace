use std::borrow::Cow;
use std::sync::Arc;

use codespace_domain::{
    workspace_info, ApplyPatchParams, ApplyPatchResult, ErrorBody, ErrorCode, FindParams,
    FindResult, OperationStatusParams, OperationStatusResult, PatchStatus, ReadParams, ReadResult,
    WorkspaceInfo, WorkspaceInfoParams,
};
use codespace_policy::{Action, ClientClaims, Registry};
use codespace_runner::PathSandbox;
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

use crate::protocol::{NegotiatedFeatures, CORE_BASELINE, SUPPORTED_PROTOCOL_VERSIONS};

#[derive(Clone)]
pub struct CodeSpace {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    registry: Registry,
    store: Arc<Store>,
}

fn err_json(err: ErrorBody) -> String {
    serde_json::to_string(&err).unwrap_or(err.message)
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
        Self {
            tool_router: Self::tool_router(),
            registry,
            store,
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
        PathSandbox::new(ws.clone())
            .read_file(&params.path)
            .map(Json)
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
        PathSandbox::new(ws.clone())
            .find(params.glob.as_deref())
            .map(Json)
            .map_err(err_json)
    }

    #[tool(
        name = "apply_patch",
        description = "Apply a Codex V4A patch. check_only verifies without writing. status applied means disk matches. Never falls back to git apply."
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
        description = "Look up a server-minted operation_id. Does not re-run the operation. Distinct from HTTP request ids."
    )]
    async fn operation_status(
        &self,
        Parameters(params): Parameters<OperationStatusParams>,
    ) -> Result<Json<OperationStatusResult>, String> {
        self.store
            .status(&params.operation_id)
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
        let _lease = self.store.try_acquire_write(&params.workspace_id.0)?;
        let fingerprint = Store::fingerprint(&params);
        match self.store.begin(
            params.operation_key.as_ref(),
            &params.workspace_id.0,
            &fingerprint,
        )? {
            Begin::Replayed(stored) => Ok(stored.result),
            Begin::Fresh(operation_id) => {
                let result = match self.execute_patch(ws, &params, operation_id.clone()) {
                    Ok(result) => result,
                    Err(err) => {
                        let failed = ApplyPatchResult {
                            status: PatchStatus::Rejected,
                            operation_id: operation_id.clone(),
                            replayed: false,
                            files: Vec::new(),
                        };
                        let _ = self.store.finish(&operation_id, &failed);
                        return Err(err);
                    }
                };
                self.store.finish(&operation_id, &result)?;
                Ok(result)
            }
        }
    }

    fn execute_patch(
        &self,
        ws: &codespace_policy::Workspace,
        params: &ApplyPatchParams,
        operation_id: codespace_domain::OperationId,
    ) -> Result<ApplyPatchResult, ErrorBody> {
        let sandbox = PathSandbox::new(ws.clone());
        for (path, expected) in &params.expected_versions {
            let actual = sandbox.version(path)?;
            if &actual != expected {
                return Err(ErrorBody::new(
                    ErrorCode::VersionConflict,
                    format!("version conflict for {path}"),
                ));
            }
        }
        if params.check_only {
            let files = crate::patch_helper::preflight(&ws.root, &params.patch)?;
            return Ok(ApplyPatchResult {
                status: PatchStatus::Rejected,
                operation_id,
                replayed: false,
                files,
            });
        }
        let planned = crate::patch_helper::preflight(&ws.root, &params.patch)?;
        let snaps = crate::rollback::snapshot(&sandbox, &planned)?;
        match crate::patch_helper::apply(&ws.root, &params.patch, false) {
            Ok(files) => {
                for path in &files {
                    let after = sandbox.read_file(path)?;
                    if after.path != *path {
                        return Err(ErrorBody::new(
                            ErrorCode::InvalidPatch,
                            format!("apply listed {path} but read returned {}", after.path),
                        ));
                    }
                }
                Ok(ApplyPatchResult {
                    status: PatchStatus::Applied,
                    operation_id,
                    replayed: false,
                    files,
                })
            }
            Err(_) => {
                let complete = crate::rollback::restore(&sandbox, &snaps);
                Ok(ApplyPatchResult {
                    status: if complete {
                        PatchStatus::FailedRolledBack
                    } else {
                        PatchStatus::FailedPartial
                    },
                    operation_id,
                    replayed: false,
                    files: planned,
                })
            }
        }
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
                "CodeSpace execution-tools MCP. No internal model calls. workspace_id is a selector. apply_patch never returns applied after a failed rollback."
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
