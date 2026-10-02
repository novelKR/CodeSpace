//! The resource authority's status in `workspace_info` (CSRG-U1). It exists only with the
//! `devguard` feature and is reported only when the gateway was started with
//! `--devguard status`. These are CodeSpace's types; no DevGuard type reaches MCP.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceAuthorityInfo {
    /// The authority, `devguard`.
    pub provider: String,
    pub participation: ResourceParticipation,
    /// Whether the authority admits or governs CodeSpace executions. Always false with
    /// `status` participation.
    pub governs_execution: bool,
    pub state: ResourceAuthorityState,
    /// The authority's code for the step that failed, as on its wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// The authority's own report, when `available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ResourceAuthorityReport>,
}

/// How CodeSpace takes part in the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceParticipation {
    /// CodeSpace reads the authority's status and nothing more: no registration, admission or
    /// launch.
    Status,
}

/// What the latest status session found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAuthorityState {
    /// The session authenticated and the authority reported its status.
    Available,
    /// No authority answered, or a response was missing or invalid.
    Unavailable,
    /// The socket's peer, or the identity it declared, is not the expected authority.
    UntrustedAuthority,
    /// The authority offers no protocol or capability set CodeSpace accepts.
    Incompatible,
    /// The authority refused CodeSpace's consumer credential.
    CredentialRefused,
    /// The consumer settings or secret could not be used, so no session was opened.
    CredentialUnavailable,
}

/// The authority's own report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceAuthorityReport {
    pub protocol: u32,
    pub capabilities: Vec<String>,
    /// The session role the authority granted CodeSpace's consumer.
    pub role: String,
    pub storage_validated: bool,
    pub registration_ready: bool,
    pub execution_ready: bool,
    /// The authority's own explanation of its readiness.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::{workspace_info, WorkspaceInfo};

    #[test]
    fn workspace_info_reports_the_authority_only_when_set() {
        let info = workspace_info(None);
        assert!(info.resource_authority.is_none());
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("resource_authority").is_none());

        let mut info = workspace_info(None);
        info.resource_authority = Some(ResourceAuthorityInfo {
            provider: "devguard".into(),
            participation: ResourceParticipation::Status,
            governs_execution: false,
            state: ResourceAuthorityState::CredentialRefused,
            error_code: Some("unauthorized".into()),
            report: None,
        });
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            json["resource_authority"],
            serde_json::json!({
                "provider": "devguard",
                "participation": "status",
                "governs_execution": false,
                "state": "credential_refused",
                "error_code": "unauthorized",
            })
        );
        let back: WorkspaceInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn the_schema_states_the_field_as_optional() {
        let schema = serde_json::to_value(schemars::schema_for!(WorkspaceInfo)).unwrap();
        assert!(schema["properties"].get("resource_authority").is_some());
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|name| name == "resource_authority"));
    }
}
