//! Handshake between `codespace-runner` and the Linux sandbox helper
//! process. No Codex types. `WIRE_PROTOCOL` is unchanged; this is a
//! separate helper JSON version, not a Unix-socket protocol.

use serde::{Deserialize, Serialize};

/// JSON request/response version for helper `prepare`. Not UDS.
pub const SANDBOX_HELPER_PROTOCOL: u32 = 1;

/// Restricted is isolated netns + Restricted seccomp. Enabled is isolated
/// netns plus the helper-owned managed proxy (`--allow-network-for-proxy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxNetwork {
    Restricted,
    Enabled,
}

/// Stdin JSON for `codespace-linux-sandbox prepare`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPrepareRequest {
    pub protocol: u32,
    pub workspace_root: String,
    pub command_cwd: String,
    pub writable_workspace: bool,
    pub network: SandboxNetwork,
    pub argv: Vec<String>,
}

/// Stdout JSON from `prepare`. Success carries the plan **pathname**
/// only; Codex argv never crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SandboxPrepareResponse {
    Prepared { plan_path: String },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(SANDBOX_HELPER_PROTOCOL, 1);
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-linux-sandbox-protocol");
    }

    #[test]
    fn request_roundtrip() {
        let req = SandboxPrepareRequest {
            protocol: SANDBOX_HELPER_PROTOCOL,
            workspace_root: "/workspace".into(),
            command_cwd: "/workspace".into(),
            writable_workspace: true,
            network: SandboxNetwork::Restricted,
            argv: vec!["/bin/true".into()],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["network"], "restricted");
        assert_eq!(
            serde_json::from_value::<SandboxPrepareRequest>(json).unwrap(),
            req
        );

        let enabled = SandboxPrepareRequest {
            network: SandboxNetwork::Enabled,
            ..req.clone()
        };
        let json = serde_json::to_value(&enabled).unwrap();
        assert_eq!(json["network"], "enabled");
        assert_eq!(
            serde_json::from_value::<SandboxPrepareRequest>(json).unwrap(),
            enabled
        );
    }

    #[test]
    fn response_is_path_only() {
        let prepared = serde_json::to_value(SandboxPrepareResponse::Prepared {
            plan_path: "/tmp/plan".into(),
        })
        .unwrap();
        assert_eq!(prepared["type"], "prepared");
        assert_eq!(prepared["plan_path"], "/tmp/plan");
        assert!(prepared.get("argv").is_none());

        let err = serde_json::to_value(SandboxPrepareResponse::Error {
            message: "nope".into(),
        })
        .unwrap();
        assert_eq!(err["type"], "error");
        assert_eq!(err["message"], "nope");
    }

    #[test]
    fn unknown_network_is_rejected() {
        let json = serde_json::json!({
            "protocol": 1,
            "workspace_root": "/",
            "command_cwd": "/",
            "writable_workspace": false,
            "network": "open",
            "argv": ["/bin/true"]
        });
        assert!(serde_json::from_value::<SandboxPrepareRequest>(json).is_err());
    }
}
