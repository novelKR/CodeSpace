use codespace_domain::{
    workspace_info, ErrorBody, FindParams, FindResult, ReadParams, ReadResult, WorkspaceInfo,
    WorkspaceInfoParams,
};
use codespace_policy::Registry;
use codespace_runner::PathSandbox;
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
}

fn err_json(err: ErrorBody) -> String {
    serde_json::to_string(&err).unwrap_or(err.message)
}

#[tool_router]
impl CodeSpace {
    pub fn new(registry: Registry) -> Self {
        Self {
            tool_router: Self::tool_router(),
            registry,
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
                "CodeSpace execution-tools MCP. No internal model calls. workspace_id is a selector. read/find stay inside the registered workspace."
                    .to_string(),
            )
    }
}
