use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::profile::Profile;
use crate::tools::{
    SERVER_NAME, SERVER_VERSION, TRANSPORT_STDIO, TRANSPORT_STREAMABLE_HTTP, W03_EXPOSED_TOOLS,
};

/// Optional selector. Not a credential.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

pub fn workspace_info(workspace_id: Option<String>) -> WorkspaceInfo {
    WorkspaceInfo {
        server: SERVER_NAME.to_string(),
        version: SERVER_VERSION.to_string(),
        internal_model_calls: false,
        tools_exposed: W03_EXPOSED_TOOLS.iter().map(|s| (*s).to_string()).collect(),
        transports: vec![
            TRANSPORT_STDIO.to_string(),
            TRANSPORT_STREAMABLE_HTTP.to_string(),
        ],
        workspace_id,
        workspace_id_is_credential: false,
        note: "workspace_info, read, find, apply_patch, and operation_status are live. apply_patch records operations; the Codex engine lands in W06/W09. Exec is not exposed yet."
            .to_string(),
        profile: None,
        root: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TOOL_WORKSPACE_INFO;

    #[test]
    fn workspace_info_contract_fields() {
        let info = workspace_info(None);
        assert!(!info.internal_model_calls);
        assert!(!info.workspace_id_is_credential);
        assert!(info.tools_exposed.iter().any(|s| s == TOOL_WORKSPACE_INFO));
        assert_eq!(
            info.tools_exposed,
            crate::tools::LIVE_TOOLS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
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
