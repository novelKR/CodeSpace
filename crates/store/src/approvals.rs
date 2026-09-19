//! Confirmation holds. Separate from the patch operations ledger.

use codespace_domain::{
    ApprovalDecision, ApprovalId, ApprovalResolveResult, ApprovalState, ApprovalTargetTool,
    ErrorBody, ErrorCode, OperationResumeResult,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{now_secs, Store};

#[derive(Debug, Clone)]
pub struct ApprovalRecord {
    pub approval_id: ApprovalId,
    pub workspace_id: String,
    pub tool: ApprovalTargetTool,
    pub params_json: String,
    pub state: ApprovalState,
}

#[derive(Debug)]
pub enum ResumeClaim {
    ReplaySuccess(OperationResumeResult),
    ReplayError(ErrorBody),
    Execute(ApprovalRecord),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum PersistedResume {
    Success(OperationResumeResult),
    Failed(ErrorBody),
}

struct Row {
    approval_id: String,
    workspace_id: String,
    tool: String,
    params_json: String,
    state: String,
    result_json: Option<String>,
}

impl Row {
    fn into_record(self) -> Result<ApprovalRecord, ErrorBody> {
        Ok(ApprovalRecord {
            approval_id: ApprovalId(self.approval_id),
            workspace_id: self.workspace_id,
            tool: ApprovalTargetTool::parse(&self.tool).map_err(bad_row)?,
            params_json: self.params_json,
            state: ApprovalState::parse(&self.state).map_err(bad_row)?,
        })
    }
}

impl Store {
    pub fn create_approval(
        &self,
        workspace_id: &str,
        tool: ApprovalTargetTool,
        params_json: serde_json::Value,
    ) -> Result<ApprovalRecord, ErrorBody> {
        let approval_id = ApprovalId(format!("appr-{}", Uuid::new_v4()));
        let json = serde_json::to_string(&params_json).map_err(ser_err)?;
        let now = now_secs();
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.execute(
            "INSERT INTO approvals
                (approval_id, workspace_id, tool, params_json, state, created_at, resolved_at, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
            params![
                approval_id.0,
                workspace_id,
                tool.as_str(),
                json,
                ApprovalState::Pending.as_str(),
                now,
            ],
        )
        .map_err(db_err)?;
        Ok(ApprovalRecord {
            approval_id,
            workspace_id: workspace_id.to_string(),
            tool,
            params_json: json,
            state: ApprovalState::Pending,
        })
    }

    pub fn resolve_approval(
        &self,
        approval_id: &ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<ApprovalResolveResult, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
        let state = ApprovalState::parse(&row.state).map_err(bad_row)?;
        if state != ApprovalState::Pending {
            return Err(conflict("approval is no longer pending"));
        }
        let next = match decision {
            ApprovalDecision::Grant => ApprovalState::Granted,
            ApprovalDecision::Deny => ApprovalState::Denied,
        };
        let now = now_secs();
        let updated = conn
            .execute(
                "UPDATE approvals
                 SET state = ?1, resolved_at = ?2
                 WHERE approval_id = ?3 AND state = 'pending'",
                params![next.as_str(), now, approval_id.0],
            )
            .map_err(db_err)?;
        if updated != 1 {
            return Err(conflict("approval is no longer pending"));
        }
        Ok(ApprovalResolveResult {
            approval_id: approval_id.clone(),
            state: next,
        })
    }

    pub fn claim_resume(&self, approval_id: &ApprovalId) -> Result<ResumeClaim, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
        match row.state.as_str() {
            "consumed" => match row.result_json.as_deref() {
                Some(raw) => match serde_json::from_str::<PersistedResume>(raw) {
                    Ok(PersistedResume::Success(result)) => Ok(ResumeClaim::ReplaySuccess(result)),
                    Ok(PersistedResume::Failed(err)) => Ok(ResumeClaim::ReplayError(err)),
                    Err(_) => Err(conflict("approval already consumed")),
                },
                None => Err(conflict("approval already consumed")),
            },
            "pending" => Err(conflict("approval is still pending")),
            "denied" => Err(conflict("approval was denied")),
            "granted" => {
                let updated = conn
                    .execute(
                        "UPDATE approvals
                         SET state = 'consumed'
                         WHERE approval_id = ?1 AND state = 'granted'",
                        params![approval_id.0],
                    )
                    .map_err(db_err)?;
                if updated != 1 {
                    return Err(conflict("approval already consumed"));
                }
                Ok(ResumeClaim::Execute(row.into_record()?))
            }
            other => Err(conflict(format!(
                "approval is in an unknown state `{other}`"
            ))),
        }
    }

    pub fn finish_resume(
        &self,
        approval_id: &ApprovalId,
        result: Result<&OperationResumeResult, &ErrorBody>,
    ) -> Result<(), ErrorBody> {
        let persisted = match result {
            Ok(success) => PersistedResume::Success(success.clone()),
            Err(err) => PersistedResume::Failed(err.clone()),
        };
        let json = serde_json::to_string(&persisted).map_err(ser_err)?;
        let conn = self.conn.lock().expect("sqlite mutex");
        conn.execute(
            "UPDATE approvals SET result_json = ?1 WHERE approval_id = ?2 AND state = 'consumed'",
            params![json, approval_id.0],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

fn load(conn: &rusqlite::Connection, approval_id: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        "SELECT approval_id, workspace_id, tool, params_json, state, result_json
         FROM approvals WHERE approval_id = ?1",
        params![approval_id],
        |row| {
            Ok(Row {
                approval_id: row.get(0)?,
                workspace_id: row.get(1)?,
                tool: row.get(2)?,
                params_json: row.get(3)?,
                state: row.get(4)?,
                result_json: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(db_err)
}

fn not_found(id: &ApprovalId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ApprovalNotFound,
        format!("unknown approval_id `{}`", id.0),
    )
}

fn conflict(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::ApprovalConflict, message)
}

fn db_err(err: rusqlite::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::ApprovalNotFound, format!("store: {err}"))
}

fn ser_err(err: serde_json::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidPatch, err.to_string())
}

fn bad_row(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::ApprovalConflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::PatchStatus;
    use serde_json::json;

    #[test]
    fn grant_then_consume_replays_stored_result() {
        let store = Store::memory().unwrap();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                json!({"workspace_id":"demo","patch":"x"}),
            )
            .unwrap();
        assert_eq!(created.state, ApprovalState::Pending);
        assert!(created.approval_id.0.starts_with("appr-"));

        let granted = store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        assert_eq!(granted.state, ApprovalState::Granted);
        assert_eq!(
            store
                .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
                .unwrap_err()
                .code,
            ErrorCode::ApprovalConflict
        );

        let ResumeClaim::Execute(record) = store.claim_resume(&created.approval_id).unwrap() else {
            panic!("expected execute");
        };
        assert_eq!(record.tool, ApprovalTargetTool::ApplyPatch);
        let result = OperationResumeResult {
            approval_id: created.approval_id.clone(),
            state: ApprovalState::Consumed,
            apply_patch: Some(codespace_domain::ApplyPatchResult::new(
                PatchStatus::Applied,
                codespace_domain::OperationId("op-1".into()),
            )),
            exec_command: None,
        };
        store
            .finish_resume(&created.approval_id, Ok(&result))
            .unwrap();
        match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::ReplaySuccess(replayed) => {
                assert_eq!(replayed.apply_patch.unwrap().status, PatchStatus::Applied);
            }
            other => panic!("expected replay, got {other:?}"),
        }
    }

    #[test]
    fn deny_cannot_resume() {
        let store = Store::memory().unwrap();
        let created = store
            .create_approval("demo", ApprovalTargetTool::ExecCommand, json!({}))
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Deny)
            .unwrap();
        assert_eq!(
            store.claim_resume(&created.approval_id).unwrap_err().code,
            ErrorCode::ApprovalConflict
        );
    }
}
