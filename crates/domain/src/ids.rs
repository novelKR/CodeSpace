use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Selector only. Knowing this value is not authorization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceId(pub String);

/// Server-minted id for a mutating operation. Distinct from the HTTP/JSON-RPC
/// request id and from [`ProcessId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct OperationId(pub String);

/// Client-supplied idempotency key. Not a capability token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct OperationKey(pub String);

/// Server-minted handle for a managed process. Clients cannot invent these.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ProcessId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_types() {
        let op = OperationId("op-1".into());
        let proc = ProcessId("proc-1".into());
        assert_ne!(op.0, proc.0);
    }
}
