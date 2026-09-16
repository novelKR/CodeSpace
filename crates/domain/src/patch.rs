use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::{OperationId, OperationKey, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatchStatus {
    Applied,
    Rejected,
    FailedRolledBack,
    FailedPartial,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyPatchParams {
    pub workspace_id: WorkspaceId,
    pub patch: String,
    #[serde(default)]
    pub expected_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub operation_key: Option<OperationKey>,
    #[serde(default)]
    pub check_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyPatchResult {
    pub status: PatchStatus,
    pub operation_id: OperationId,
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationStatusParams {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationStatusResult {
    pub operation_id: OperationId,
    pub status: PatchStatus,
    pub replayed: bool,
}
