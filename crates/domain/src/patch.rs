use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::{OperationId, OperationKey, WorkId, WorkspaceId};
use crate::work::CoordinationHint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatchStatus {
    Applied,
    Checked,
    Rejected,
    FailedRolledBack,
    FailedPartial,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileChange {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_version: Option<String>,
    pub kind: FileChangeKind,
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
    #[serde(default)]
    pub changes: Vec<FileChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

impl ApplyPatchResult {
    pub fn new(status: PatchStatus, operation_id: OperationId) -> Self {
        Self {
            status,
            operation_id,
            replayed: false,
            files: Vec::new(),
            changes: Vec::new(),
            work_id: None,
            coordination: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    #[default]
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationEventName {
    Minted,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationEvent {
    pub at: i64,
    pub name: OperationEventName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PatchStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl OperationEvent {
    pub fn minted(at: i64) -> Self {
        Self {
            at,
            name: OperationEventName::Minted,
            status: None,
            reason: None,
        }
    }

    pub fn finished(at: i64, status: PatchStatus) -> Self {
        Self {
            at,
            name: OperationEventName::Finished,
            status: Some(status),
            reason: match status {
                PatchStatus::Unknown => Some("unknown".into()),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationStatusParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_key: Option<OperationKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationStatusResult {
    pub operation_id: OperationId,
    pub status: PatchStatus,
    pub replayed: bool,
    #[serde(default)]
    pub kind: OperationKind,
    pub workspace_id: WorkspaceId,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FileChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<OperationEvent>,
}
