//! Confirmation holds for already-allowed mutations. Not a privilege grant.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::ApprovalId;
use crate::patch::ApplyPatchResult;
use crate::process::ExecCommandResult;

/// Operator workspace setting. Not an MCP argument that grants rights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalsMode {
    /// Mutations run immediately after policy allows them.
    #[default]
    Off,
    /// Policy-allowed `apply_patch` / `exec_command` wait on a confirmation hold.
    Confirm,
}

impl ApprovalsMode {
    pub fn holds_mutations(self) -> bool {
        matches!(self, Self::Confirm)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ApprovalTargetTool {
    #[serde(rename = "apply_patch")]
    ApplyPatch,
    #[serde(rename = "exec_command")]
    ExecCommand,
}

impl ApprovalTargetTool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApplyPatch => "apply_patch",
            Self::ExecCommand => "exec_command",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "apply_patch" => Ok(Self::ApplyPatch),
            "exec_command" => Ok(Self::ExecCommand),
            other => Err(format!("unsupported approval tool `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Granted,
    /// Resume has been claimed. Execution may be in flight or interrupted.
    Resuming,
    Denied,
    Consumed,
}

impl ApprovalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Granted => "granted",
            Self::Resuming => "resuming",
            Self::Denied => "denied",
            Self::Consumed => "consumed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "granted" => Ok(Self::Granted),
            "resuming" => Ok(Self::Resuming),
            "denied" => Ok(Self::Denied),
            "consumed" => Ok(Self::Consumed),
            other => Err(format!("unknown approval state `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Grant,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalCreateParams {
    pub tool: ApprovalTargetTool,
    /// Same argument object as the named tool.
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalCreateResult {
    pub approval_id: ApprovalId,
    pub state: ApprovalState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalResolveParams {
    pub approval_id: ApprovalId,
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalResolveResult {
    pub approval_id: ApprovalId,
    pub state: ApprovalState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationResumeParams {
    pub approval_id: ApprovalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationResumeResult {
    pub approval_id: ApprovalId,
    pub state: ApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_patch: Option<ApplyPatchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_command: Option<ExecCommandResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_mode_defaults_to_off() {
        assert_eq!(ApprovalsMode::default(), ApprovalsMode::Off);
        assert!(!ApprovalsMode::Off.holds_mutations());
        assert!(ApprovalsMode::Confirm.holds_mutations());
        assert_eq!(
            serde_json::to_value(ApprovalsMode::Confirm).unwrap(),
            "confirm"
        );
        assert_eq!(ApprovalState::Resuming.as_str(), "resuming");
        assert_eq!(
            ApprovalState::parse("resuming").unwrap(),
            ApprovalState::Resuming
        );
    }
}
