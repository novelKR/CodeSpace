use std::sync::Arc;

use codespace_domain::{
    workspace_info, ApplyPatchParams, ApplyPatchResult, ErrorBody, ErrorCode, FindParams,
    FindResult, OperationStatusParams, OperationStatusResult, PatchStatus, ReadParams, ReadResult,
    WorkspaceInfo, WorkspaceInfoParams,
};
use codespace_policy::{allow, Action, ClientClaims, Registry};
use codespace_runner::PathSandbox;
use codespace_store::{begin_or_replay, OperationStore, WorkspaceLocks};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, Json, ServerHandler,
};

#[derive(Clone)]
pub struct CodeSpace {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    registry: Registry,
    store: Arc<OperationStore>,
    locks: Arc<WorkspaceLocks>,
}

fn err_json(err: ErrorBody) -> String {
    serde_json::to_string(&err).unwrap_or(err.message)
}

#[tool_router]
impl CodeSpace {
    pub fn new(registry: Registry) -> Self {
        Self::new_with(registry, OperationStore::memory(), WorkspaceLocks::new())
    }

    pub fn new_with(
        registry: Registry,
        store: Arc<OperationStore>,
        locks: Arc<WorkspaceLocks>,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
            store,
            locks,
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
        description = "Idempotent mutating patch request. W08 records the operation; disk apply is W09. operation_key is not a credential."
    )]
    async fn apply_patch(
        &self,
        Parameters(params): Parameters<ApplyPatchParams>,
    ) -> Result<Json<ApplyPatchResult>, String> {
        self.run_apply_patch(params).map(Json).map_err(err_json)
    }

    #[tool(
        name = "operation_status",
        description = "Look up a server-minted operation_id. A lost HTTP response is not an execution failure."
    )]
    async fn operation_status(
        &self,
        Parameters(params): Parameters<OperationStatusParams>,
    ) -> Result<Json<OperationStatusResult>, String> {
        self.run_operation_status(params)
            .map(Json)
            .map_err(err_json)
    }
}

impl CodeSpace {
    fn run_apply_patch(&self, params: ApplyPatchParams) -> Result<ApplyPatchResult, ErrorBody> {
        let ws = self.registry.get(&params.workspace_id.0)?;
        allow(ws, Action::Write, &ClientClaims::default())?;
        begin_or_replay(&self.store, &self.locks, &params, |operation_id| {
            // W08 stores the key and result. W09 replaces this with Codex apply.
            Ok(ApplyPatchResult {
                status: PatchStatus::Applied,
                operation_id,
                replayed: false,
                files: vec![],
            })
        })
    }

    fn run_operation_status(
        &self,
        params: OperationStatusParams,
    ) -> Result<OperationStatusResult, ErrorBody> {
        let stored = self.store.get(&params.operation_id.0)?.ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::OperationNotFound,
                format!("unknown operation_id `{}`", params.operation_id.0),
            )
        })?;
        Ok(OperationStatusResult {
            operation_id: stored.result.operation_id,
            status: stored.result.status,
            replayed: stored.result.replayed,
        })
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
            .with_server_info(Implementation::new(
                codespace_domain::SERVER_NAME,
                codespace_domain::SERVER_VERSION,
            ))
            .with_instructions(
                "CodeSpace execution-tools MCP. No internal model calls. workspace_id is a selector. operation_key is idempotency, not a credential."
                    .to_string(),
            )
    }
}
