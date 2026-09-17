use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::{OperationId, OperationKey, WorkId, WorkspaceId};
use crate::work::CoordinationHint;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyPatchResult {
    pub status: PatchStatus,
    pub operation_id: OperationId,
    #[serde(default)]
    pub replayed: bool,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
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
