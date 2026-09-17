use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Product error codes returned in tool results. HTTP 401 from Bearer
/// middleware is **not** one of these: it never becomes an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    WorkspaceNotFound,
    WorkspaceBusy,
    InvalidPatch,
    InvalidCommand,
    ProcessSpawnFailed,
    PathEscape,
    SymlinkRejected,
    SpecialFileRejected,
    AddFileExists,
    MoveDestinationExists,
    VersionConflict,
    OperationKeyConflict,
    OperationNotFound,
    ProcessNotFound,
    OutputLimit,
    Timeout,
    WorkNotFound,
    WorkClosed,
    IntentNotFound,
    IntentAlreadyClaimed,
    IntentNotEditable,
    IntentRevisionConflict,
    QueueNotEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            operation_id: None,
        }
    }

    pub fn with_operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }
}

/// A lost HTTP response, TCP reset, or 401 at the Bearer layer is not an
/// execution failure. Clients must not treat it as `operation` state.
pub const TRANSPORT_FAILURE_IS_NOT_OPERATION: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Wire/auth before any tool handler. No `operation_id` is minted.
    Transport,
    /// Tool handler refused the request. May include `operation_id` if a
    /// mutating operation was already recorded (W08+).
    Execution,
}

pub fn classify_http_status(status: u16) -> FailureClass {
    match status {
        401 | 403 | 404 | 408 | 429 | 502 | 503 | 504 => FailureClass::Transport,
        _ => FailureClass::Execution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_screaming_snake() {
        let json = serde_json::to_string(&ErrorCode::WorkspaceNotFound).unwrap();
        assert_eq!(json, "\"WORKSPACE_NOT_FOUND\"");
        let json = serde_json::to_string(&ErrorCode::WorkspaceBusy).unwrap();
        assert_eq!(json, "\"WORKSPACE_BUSY\"");
        let json = serde_json::to_string(&ErrorCode::Unauthorized).unwrap();
        assert_eq!(json, "\"UNAUTHORIZED\"");
        let json = serde_json::to_string(&ErrorCode::InvalidPatch).unwrap();
        assert_eq!(json, "\"INVALID_PATCH\"");
        let json = serde_json::to_string(&ErrorCode::InvalidCommand).unwrap();
        assert_eq!(json, "\"INVALID_COMMAND\"");
        let json = serde_json::to_string(&ErrorCode::ProcessSpawnFailed).unwrap();
        assert_eq!(json, "\"PROCESS_SPAWN_FAILED\"");
        let json = serde_json::to_string(&ErrorCode::IntentAlreadyClaimed).unwrap();
        assert_eq!(json, "\"INTENT_ALREADY_CLAIMED\"");
        let json = serde_json::to_string(&ErrorCode::WorkClosed).unwrap();
        assert_eq!(json, "\"WORK_CLOSED\"");
    }

    #[test]
    fn http_401_is_transport_not_operation() {
        assert_eq!(classify_http_status(401), FailureClass::Transport);
        const {
            assert!(TRANSPORT_FAILURE_IS_NOT_OPERATION);
        }
        let body = ErrorBody::new(ErrorCode::Unauthorized, "not used for bearer 401");
        assert!(body.operation_id.is_none());
    }
}
