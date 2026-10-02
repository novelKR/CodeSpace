//! The resource authority's status in `workspace_info` (CSRG-U1). It exists only with the
//! `devguard` feature and is reported only when the gateway was started with
//! `--devguard status`. These are CodeSpace's types; no DevGuard type reaches MCP. Every value
//! is an enumeration, a flag or a protocol number: no free text from the authority does.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceAuthorityInfo {
    pub provider: ResourceAuthorityProvider,
    pub participation: ResourceParticipation,
    /// Whether the authority admits or governs CodeSpace executions. Always false with
    /// `status` participation.
    pub governs_execution: bool,
    pub state: ResourceAuthorityState,
    /// The authority's code for the step that failed. Its message is not reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ResourceAuthorityErrorCode>,
    /// The authority's own report, when `available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ResourceAuthorityReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAuthorityProvider {
    Devguard,
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

/// The authority's error codes, named as on DevGuard's wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAuthorityErrorCode {
    Unauthorized,
    InvalidRequest,
    AttemptConflict,
    ResourceUnavailable,
    ResourceControlUnavailable,
    ResourcePolicyUnsupported,
    InvalidTransition,
    NotFound,
    ReconciliationRequired,
    JournalInvalid,
}

/// The capabilities the authority states, named as on DevGuard's wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAuthorityCapability {
    DurableAdmission,
    FencedLaunch,
    PerResourceEvidence,
    StaticControlReservations,
    MacosCooperative,
    LinuxCgroupV2,
    ParentLease,
    UpgradeDrain,
}

/// The session role the authority granted CodeSpace's consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAuthorityRole {
    Workload,
    ControlService,
    Administrator,
}

/// The authority's own report. Its free-text readiness reason is not reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceAuthorityReport {
    pub protocol: u32,
    pub capabilities: Vec<ResourceAuthorityCapability>,
    pub role: ResourceAuthorityRole,
    pub storage_validated: bool,
    pub registration_ready: bool,
    pub execution_ready: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::info::{workspace_info, WorkspaceInfo};
    use serde_json::Value;

    #[test]
    fn workspace_info_reports_the_authority_only_when_set() {
        let info = workspace_info(None);
        assert!(info.resource_authority.is_none());
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("resource_authority").is_none());

        let mut info = workspace_info(None);
        info.resource_authority = Some(ResourceAuthorityInfo {
            provider: ResourceAuthorityProvider::Devguard,
            participation: ResourceParticipation::Status,
            governs_execution: false,
            state: ResourceAuthorityState::Available,
            error_code: None,
            report: Some(ResourceAuthorityReport {
                protocol: 1,
                capabilities: vec![
                    ResourceAuthorityCapability::DurableAdmission,
                    ResourceAuthorityCapability::LinuxCgroupV2,
                ],
                role: ResourceAuthorityRole::ControlService,
                storage_validated: true,
                registration_ready: false,
                execution_ready: false,
            }),
        });
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            json["resource_authority"],
            serde_json::json!({
                "provider": "devguard",
                "participation": "status",
                "governs_execution": false,
                "state": "available",
                "report": {
                    "protocol": 1,
                    "capabilities": ["durable_admission", "linux_cgroup_v2"],
                    "role": "control_service",
                    "storage_validated": true,
                    "registration_ready": false,
                    "execution_ready": false,
                },
            })
        );
        let back: WorkspaceInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);

        info.resource_authority = Some(ResourceAuthorityInfo {
            state: ResourceAuthorityState::CredentialRefused,
            error_code: Some(ResourceAuthorityErrorCode::Unauthorized),
            report: None,
            ..info.resource_authority.unwrap()
        });
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["resource_authority"]["error_code"], "unauthorized");
        assert!(json["resource_authority"].get("report").is_none());
    }

    #[test]
    fn the_schema_states_the_field_as_optional() {
        let schema = serde_json::to_value(schemars::schema_for!(WorkspaceInfo)).unwrap();
        assert!(schema["properties"].get("resource_authority").is_some());
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|name| name == "resource_authority"));
    }

    /// Where a JSON schema admits a string that is not one of a fixed set.
    fn free_strings(node: &Value, definitions: &Value, at: &str, found: &mut Vec<String>) {
        let Value::Object(map) = node else { return };
        if let Some(Value::String(reference)) = map.get("$ref") {
            let name = reference.trim_start_matches("#/$defs/");
            free_strings(
                &definitions[name],
                definitions,
                &format!("{at}>{name}"),
                found,
            );
        }
        let string = match map.get("type") {
            Some(Value::String(kind)) => kind == "string",
            Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "string"),
            _ => false,
        };
        if string && !map.contains_key("enum") && !map.contains_key("const") {
            found.push(at.to_owned());
        }
        if let Some(Value::Object(properties)) = map.get("properties") {
            for (name, property) in properties {
                free_strings(property, definitions, &format!("{at}.{name}"), found);
            }
        }
        for key in ["items", "additionalProperties"] {
            if let Some(inner) = map.get(key) {
                free_strings(inner, definitions, &format!("{at}[]"), found);
            }
        }
        for key in ["oneOf", "anyOf", "allOf"] {
            if let Some(Value::Array(alternatives)) = map.get(key) {
                for alternative in alternatives {
                    free_strings(alternative, definitions, at, found);
                }
            }
        }
    }

    #[test]
    fn no_value_from_the_authority_is_free_text() {
        let schema = serde_json::to_value(schemars::schema_for!(ResourceAuthorityInfo)).unwrap();
        let mut found = Vec::new();
        free_strings(&schema, &schema["$defs"], "resource_authority", &mut found);
        assert_eq!(found, Vec::<String>::new());

        // The check sees a free string where one exists.
        let schema = serde_json::to_value(schemars::schema_for!(WorkspaceInfo)).unwrap();
        let mut found = Vec::new();
        free_strings(&schema, &schema["$defs"], "workspace_info", &mut found);
        assert!(
            found.contains(&"workspace_info.note".to_owned()),
            "{found:?}"
        );
    }
}
