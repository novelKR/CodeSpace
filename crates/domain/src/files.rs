use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{WorkId, WorkspaceId};
use crate::work::CoordinationHint;

/// Per-call byte cap for `read`. Omitted `limit` uses this value.
pub const DEFAULT_READ_LIMIT: u32 = 1024 * 1024;
/// Per-call path cap for `find`. Omitted `limit` uses this value.
pub const DEFAULT_FIND_LIMIT: u32 = 10_000;

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadParams {
    pub workspace_id: WorkspaceId,
    pub path: String,
    /// Byte offset into the file. Omitted is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Byte window size. Omitted is [`DEFAULT_READ_LIMIT`]. Max is the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadResult {
    pub path: String,
    pub content: String,
    pub version: String,
    pub truncated: bool,
    /// Start of this window in file bytes. Omitted from JSON when 0.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub offset: u64,
    /// File bytes included in this window. Not `content.len()` after UTF-8 lossy.
    pub byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindParams {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub glob: Option<String>,
    /// Index into the sorted matching path list. Omitted is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Path window size. Omitted is [`DEFAULT_FIND_LIMIT`]. Max is the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindResult {
    pub paths: Vec<String>,
    pub truncated: bool,
    /// Start of this window in the sorted path list. Omitted from JSON when 0.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<CoordinationHint>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_read_params_default_to_first_window() {
        let params: ReadParams = serde_json::from_value(json!({
            "workspace_id": "demo",
            "path": "a.txt"
        }))
        .unwrap();
        assert!(params.offset.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn zero_offset_is_omitted_from_read_json() {
        let result = ReadResult {
            path: "a.txt".into(),
            content: "hi".into(),
            version: "sha256:x".into(),
            truncated: false,
            offset: 0,
            byte_count: 2,
            coordination: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("offset").is_none());
        assert_eq!(json["byte_count"], 2);
    }
}
