use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{IntentId, WorkId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntentState {
    Draft,
    Queued,
    Claimed,
    Done,
    Blocked,
    Cancelled,
    Superseded,
}

impl IntentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "draft" => Some(Self::Draft),
            "queued" => Some(Self::Queued),
            "claimed" => Some(Self::Claimed),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }

    pub fn is_editable(self) -> bool {
        matches!(self, Self::Draft | Self::Queued)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPolicy {
    NextCheckpoint,
    #[default]
    AfterCurrentWork,
    NextWork,
}

impl DeliveryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NextCheckpoint => "next_checkpoint",
            Self::AfterCurrentWork => "after_current_work",
            Self::NextWork => "next_work",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "next_checkpoint" => Some(Self::NextCheckpoint),
            "after_current_work" => Some(Self::AfterCurrentWork),
            "next_work" => Some(Self::NextWork),
            _ => None,
        }
    }

    pub fn claimable_in(self, work_state: crate::work::WorkState) -> bool {
        use crate::work::WorkState;
        matches!(
            (self, work_state),
            (Self::NextCheckpoint, WorkState::Active | WorkState::Closing)
                | (Self::AfterCurrentWork, WorkState::Closing)
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    #[default]
    FollowUp,
    Note,
    StopNotice,
}

impl IntentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FollowUp => "follow_up",
            Self::Note => "note",
            Self::StopNotice => "stop_notice",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "follow_up" => Some(Self::FollowUp),
            "note" => Some(Self::Note),
            "stop_notice" => Some(Self::StopNotice),
            _ => None,
        }
    }
}

/// User-authored deferred input. Body is instruction, never a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UserIntent {
    pub intent_id: IntentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_id: Option<WorkId>,
    pub workspace_id: WorkspaceId,
    pub position: i64,
    pub revision: u64,
    pub kind: IntentKind,
    pub delivery: DeliveryPolicy,
    pub body: String,
    pub state: IntentState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::WorkState;

    #[test]
    fn default_delivery_is_after_current_work() {
        assert_eq!(DeliveryPolicy::default(), DeliveryPolicy::AfterCurrentWork);
        assert!(!DeliveryPolicy::AfterCurrentWork.claimable_in(WorkState::Active));
        assert!(DeliveryPolicy::AfterCurrentWork.claimable_in(WorkState::Closing));
        assert!(DeliveryPolicy::NextCheckpoint.claimable_in(WorkState::Active));
        assert!(!DeliveryPolicy::NextWork.claimable_in(WorkState::Active));
        assert!(!DeliveryPolicy::NextWork.claimable_in(WorkState::Closing));
    }

    #[test]
    fn draft_is_editable_claimed_is_not() {
        assert!(IntentState::Draft.is_editable());
        assert!(IntentState::Queued.is_editable());
        assert!(!IntentState::Claimed.is_editable());
    }
}
