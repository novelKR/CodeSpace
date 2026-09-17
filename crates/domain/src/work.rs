use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{IntentId, WorkId, WorkspaceId};
use crate::intent::{DeliveryPolicy, IntentKind};

/// Application work state. Independent of MCP session lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    Active,
    Closing,
    Closed,
    Cancelled,
}

impl WorkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "active" => Some(Self::Active),
            "closing" => Some(Self::Closing),
            "closed" => Some(Self::Closed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn accepts_claims(self) -> bool {
        matches!(self, Self::Active | Self::Closing)
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Closing)
    }
}

/// Counts-only hint. Never includes intent bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoordinationHint {
    pub work_id: WorkId,
    pub pending_user_items: u32,
    pub queue_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Work {
    pub work_id: WorkId,
    pub workspace_id: WorkspaceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: WorkState,
    pub queue_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkOpenParams {
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkOpenResult {
    pub work_id: WorkId,
    pub workspace_id: WorkspaceId,
    pub state: WorkState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkIdParams {
    pub work_id: WorkId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SteerStatusResult {
    pub work_id: WorkId,
    pub state: WorkState,
    pub queued: u32,
    pub claimed: u32,
    pub claimable_now: u32,
    pub queue_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimedIntent {
    pub intent_id: IntentId,
    pub revision: u64,
    pub kind: IntentKind,
    pub delivery: DeliveryPolicy,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SteerClaimNextResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ClaimedIntent>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SteerOutcome {
    #[default]
    Done,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SteerCompleteParams {
    pub work_id: WorkId,
    pub intent_id: IntentId,
    #[serde(default)]
    pub outcome: SteerOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkFinishResult {
    pub work_id: WorkId,
    pub closed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub queued: u32,
    pub state: WorkState,
}

pub const FINISH_REASON_PENDING: &str = "pending_user_input";
