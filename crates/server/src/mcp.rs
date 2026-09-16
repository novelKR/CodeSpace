use codespace_domain::{workspace_info, WorkspaceInfo, WorkspaceInfoParams};
use codespace_policy::Registry;
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
        match lookup(&self.registry, params.workspace_id) {
            Ok(info) => Ok(Json(info)),
            Err(err) => Err(serde_json::to_string(&err).unwrap_or(err.message)),
        }
    }
}

fn lookup(
    registry: &Registry,
    workspace_id: Option<String>,
) -> Result<WorkspaceInfo, codespace_domain::ErrorBody> {
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
                "CodeSpace execution-tools MCP. No internal model calls. workspace_id is a selector. Unknown ids are rejected. Client approved/user_id claims are ignored."
                    .to_string(),
            )
    }
}
