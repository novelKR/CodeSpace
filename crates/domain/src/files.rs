use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::WorkspaceId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadParams {
    pub workspace_id: WorkspaceId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    pub version: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindParams {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub glob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindResult {
    pub paths: Vec<String>,
    pub truncated: bool,
}
