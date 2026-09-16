//! Domain types for CodeSpace. No `rmcp` dependency.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SERVER_NAME: &str = "codespace";
pub const SERVER_VERSION: &str = "0.2.0";

pub const TOOL_WORKSPACE_INFO: &str = "workspace_info";

pub const TRANSPORT_STDIO: &str = "stdio";
pub const TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";

/// Optional selector. Not a credential. Unknown until the W04 registry.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceInfoParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceInfo {
    pub server: String,
    pub version: String,
    pub internal_model_calls: bool,
    pub tools_exposed: Vec<String>,
    pub transports: Vec<String>,
    pub workspace_id: Option<String>,
    pub workspace_id_is_credential: bool,
    pub note: String,
}

pub fn workspace_info(workspace_id: Option<String>) -> WorkspaceInfo {
    WorkspaceInfo {
        server: SERVER_NAME.to_string(),
        version: SERVER_VERSION.to_string(),
        internal_model_calls: false,
        tools_exposed: vec![TOOL_WORKSPACE_INFO.to_string()],
        transports: vec![
            TRANSPORT_STDIO.to_string(),
            TRANSPORT_STREAMABLE_HTTP.to_string(),
        ],
        workspace_id,
        workspace_id_is_credential: false,
        note: "Harmless probe. Exec and apply_patch are not exposed in W02.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_info_contract_fields() {
        let info = workspace_info(None);
        assert!(!info.internal_model_calls);
        assert!(!info.workspace_id_is_credential);
        assert_eq!(info.tools_exposed, vec![TOOL_WORKSPACE_INFO]);
        assert_eq!(
            info.transports,
            vec![TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP]
        );
        assert_eq!(info.workspace_id, None);
        assert_eq!(info.server, SERVER_NAME);
        assert_eq!(info.version, SERVER_VERSION);
    }

    #[test]
    fn workspace_id_is_echoed_not_trusted() {
        let info = workspace_info(Some("demo".into()));
        assert_eq!(info.workspace_id.as_deref(), Some("demo"));
        assert!(!info.workspace_id_is_credential);
    }
}
