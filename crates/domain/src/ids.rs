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

/// Selector for one logical job the user handed the outer agent.
/// Knowing this value is not authorization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct WorkId(pub String);

/// Server-minted id for one deferred user-intent item.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct IntentId(pub String);

/// Server-minted id for a confirmation hold. Distinct from [`OperationId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalId(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct_types() {
        let op = OperationId("op-1".into());
        let proc = ProcessId("proc-1".into());
        assert_ne!(op.0, proc.0);
        let work = WorkId("work-1".into());
        let intent = IntentId("fb-1".into());
        assert_ne!(work.0, intent.0);
    }
}
