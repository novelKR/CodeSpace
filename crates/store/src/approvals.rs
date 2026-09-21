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
    pub fingerprint: String,
    pub params_json: String,
    pub state: ApprovalState,
    pub resolved_at: Option<i64>,
}

pub struct ResumeInflightGuard<'a> {
    store: &'a Store,
    id: String,
}

impl Drop for ResumeInflightGuard<'_> {
    fn drop(&mut self) {
        self.store
            .resume_inflight
            .lock()
            .expect("inflight mutex")
            .remove(&self.id);
    }
}

impl std::fmt::Debug for ResumeInflightGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeInflightGuard")
            .field("id", &self.id)
            .finish()
    }
}

#[derive(Debug)]
pub enum ResumeClaim<'a> {
    ReplaySuccess(OperationResumeResult),
    ReplayError(ErrorBody),
    Execute(ApprovalRecord, ResumeInflightGuard<'a>),
    Reconcile(ApprovalRecord, ResumeInflightGuard<'a>),
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
    fingerprint: String,
    params_json: String,
    state: String,
    resolved_at: Option<i64>,
    result_json: Option<String>,
}

impl Row {
    fn into_record(self) -> Result<ApprovalRecord, ErrorBody> {
        Ok(ApprovalRecord {
            approval_id: ApprovalId(self.approval_id),
            workspace_id: self.workspace_id,
            tool: ApprovalTargetTool::parse(&self.tool).map_err(bad_row)?,
            fingerprint: self.fingerprint,
            params_json: self.params_json,
            state: ApprovalState::parse(&self.state).map_err(bad_row)?,
            resolved_at: self.resolved_at,
        })
    }
}

impl Store {
    pub fn create_approval(
        &self,
        workspace_id: &str,
        tool: ApprovalTargetTool,
        fingerprint: &str,
        params_json: serde_json::Value,
    ) -> Result<ApprovalRecord, ErrorBody> {
        let json = serde_json::to_string(&params_json).map_err(ser_err)?;
        let now = now_secs();
        let conn = self.conn.lock().expect("sqlite mutex");
        if let Some(existing) = load_active_by_fingerprint(&conn, workspace_id, fingerprint)? {
            return existing.into_record();
        }
        let approval_id = ApprovalId(format!("appr-{}", Uuid::new_v4()));
        match conn.execute(
            "INSERT INTO approvals
                (approval_id, workspace_id, tool, fingerprint, params_json, state, created_at, resolved_at, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL)",
            params![
                approval_id.0,
                workspace_id,
                tool.as_str(),
                fingerprint,
                json,
                ApprovalState::Pending.as_str(),
                now,
            ],
        ) {
            Ok(_) => Ok(ApprovalRecord {
                approval_id,
                workspace_id: workspace_id.to_string(),
                tool,
                fingerprint: fingerprint.to_string(),
                params_json: json,
                state: ApprovalState::Pending,
                resolved_at: None,
            }),
            Err(err) if is_constraint(&err) => load_active_by_fingerprint(
                &conn,
                workspace_id,
                fingerprint,
            )?
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::ApprovalConflict,
                    "active approval fingerprint conflict",
                )
            })
            .and_then(Row::into_record),
            Err(err) => Err(db_err(err)),
        }
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
        let now = now_secs();
        let updated = match decision {
            ApprovalDecision::Grant => conn
                .execute(
                    "UPDATE approvals
                     SET state = ?1, resolved_at = ?2
                     WHERE approval_id = ?3 AND state = 'pending'",
                    params![ApprovalState::Granted.as_str(), now, approval_id.0],
                )
                .map_err(db_err)?,
            ApprovalDecision::Deny => conn
                .execute(
                    "UPDATE approvals
                     SET state = ?1, resolved_at = ?2, params_json = ?3
                     WHERE approval_id = ?4 AND state = 'pending'",
                    params![
                        ApprovalState::Denied.as_str(),
                        now,
                        scrubbed_params(&row.tool, &row.workspace_id, &row.fingerprint),
                        approval_id.0
                    ],
                )
                .map_err(db_err)?,
        };
        if updated != 1 {
            return Err(conflict("approval is no longer pending"));
        }
        Ok(ApprovalResolveResult {
            approval_id: approval_id.clone(),
            state: match decision {
                ApprovalDecision::Grant => ApprovalState::Granted,
                ApprovalDecision::Deny => ApprovalState::Denied,
            },
        })
    }

    pub fn claim_resume(&self, approval_id: &ApprovalId) -> Result<ResumeClaim<'_>, ErrorBody> {
        loop {
            let conn = self.conn.lock().expect("sqlite mutex");
            let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
            match row.state.as_str() {
                "consumed" => return replay_consumed(approval_id, row.result_json.as_deref()),
                "pending" => return Err(conflict("approval is still pending")),
                "denied" => return Err(conflict("approval was denied")),
                "granted" | "queued" | "resuming" => {
                    drop(conn);
                    let guard = self.mark_inflight(&approval_id.0)?;
                    let conn = self.conn.lock().expect("sqlite mutex");
                    let row = match load(&conn, &approval_id.0)? {
                        Some(row) => row,
                        None => {
                            drop(conn);
                            drop(guard);
                            return Err(not_found(approval_id));
                        }
                    };
                    match row.state.as_str() {
                        "granted" => {
                            let updated = conn
                                .execute(
                                    "UPDATE approvals
                                     SET state = 'queued'
                                     WHERE approval_id = ?1 AND state = 'granted'",
                                    params![approval_id.0],
                                )
                                .map_err(db_err)?;
                            if updated != 1 {
                                drop(conn);
                                drop(guard);
                                continue;
                            }
                            return Ok(ResumeClaim::Execute(row.into_record()?, guard));
                        }
                        "queued" => {
                            return Ok(ResumeClaim::Execute(row.into_record()?, guard));
                        }
                        "resuming" => {
                            return Ok(ResumeClaim::Reconcile(row.into_record()?, guard));
                        }
                        "consumed" | "pending" | "denied" => {
                            drop(conn);
                            drop(guard);
                            continue;
                        }
                        other => {
                            drop(conn);
                            drop(guard);
                            return Err(conflict(format!(
                                "approval is in an unknown state `{other}`"
                            )));
                        }
                    }
                }
                other => {
                    return Err(conflict(format!(
                        "approval is in an unknown state `{other}`"
                    )))
                }
            }
        }
    }

    pub fn finish_resume(
        &self,
        approval_id: &ApprovalId,
        result: Result<&OperationResumeResult, &ErrorBody>,
    ) -> Result<(), ErrorBody> {
        let queued = {
            let conn = self.conn.lock().expect("sqlite mutex");
            load(&conn, &approval_id.0)?
                .ok_or_else(|| not_found(approval_id))?
                .state
                == ApprovalState::Queued.as_str()
        };
        if queued
            && matches!(
                &result,
                Err(err)
                    if matches!(
                        err.code,
                        ErrorCode::WorkspaceBusy | ErrorCode::ResourceQueueFull
                    )
            )
        {
            return Ok(());
        }
        if self.take_fail_next_finish() {
            return Err(ambiguous(
                approval_id,
                "resume result could not be persisted",
                result.ok().and_then(|ok| {
                    ok.apply_patch
                        .as_ref()
                        .map(|patch| patch.operation_id.0.clone())
                }),
            ));
        }
        let persisted = match result {
            Ok(success) => PersistedResume::Success(success.clone()),
            Err(err) => PersistedResume::Failed(err.clone()),
        };
        let json = serde_json::to_string(&persisted).map_err(ser_err)?;
        let conn = self.conn.lock().expect("sqlite mutex");
        let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
        let scrubbed = scrubbed_params(&row.tool, &row.workspace_id, &row.fingerprint);
        let updated = conn
            .execute(
                "UPDATE approvals
                 SET state = 'consumed', result_json = ?1, params_json = ?2
                 WHERE approval_id = ?3 AND state IN ('queued', 'resuming')",
                params![json, scrubbed, approval_id.0],
            )
            .map_err(db_err)?;
        if updated != 1 {
            return Err(ambiguous(
                approval_id,
                "resume result could not be persisted",
                None,
            ));
        }
        Ok(())
    }

    pub fn mark_resuming(&self, approval_id: &ApprovalId) -> Result<(), ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
        match row.state.as_str() {
            "resuming" => Ok(()),
            "queued" => {
                let updated = conn
                    .execute(
                        "UPDATE approvals
                         SET state = 'resuming'
                         WHERE approval_id = ?1 AND state = 'queued'",
                        params![approval_id.0],
                    )
                    .map_err(db_err)?;
                if updated != 1 {
                    return Err(conflict("approval is not queued for dispatch"));
                }
                Ok(())
            }
            other => Err(conflict(format!(
                "approval is in an unknown state `{other}`"
            ))),
        }
    }

    pub fn fail_next_finish(&self) {
        *self.fail_next_finish.lock().expect("fail flag") = true;
    }

    pub fn force_approval_state(
        &self,
        approval_id: &ApprovalId,
        state: ApprovalState,
        clear_result: bool,
    ) -> Result<(), ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let updated = if clear_result {
            conn.execute(
                "UPDATE approvals SET state = ?1, result_json = NULL WHERE approval_id = ?2",
                params![state.as_str(), approval_id.0],
            )
            .map_err(db_err)?
        } else {
            conn.execute(
                "UPDATE approvals SET state = ?1 WHERE approval_id = ?2",
                params![state.as_str(), approval_id.0],
            )
            .map_err(db_err)?
        };
        if updated != 1 {
            return Err(not_found(approval_id));
        }
        Ok(())
    }

    pub fn inspect_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<(ApprovalState, String, Option<String>), ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let row = load(&conn, &approval_id.0)?.ok_or_else(|| not_found(approval_id))?;
        Ok((
            ApprovalState::parse(&row.state).map_err(bad_row)?,
            row.params_json,
            row.result_json,
        ))
    }

    fn mark_inflight(&self, id: &str) -> Result<ResumeInflightGuard<'_>, ErrorBody> {
        let mut set = self.resume_inflight.lock().expect("inflight mutex");
        if !set.insert(id.to_string()) {
            return Err(conflict("resume already in progress"));
        }
        Ok(ResumeInflightGuard {
            store: self,
            id: id.to_string(),
        })
    }

    fn take_fail_next_finish(&self) -> bool {
        let mut flag = self.fail_next_finish.lock().expect("fail flag");
        let set = *flag;
        *flag = false;
        set
    }
}

fn load(conn: &rusqlite::Connection, approval_id: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        "SELECT approval_id, workspace_id, tool, fingerprint, params_json, state, resolved_at, result_json
         FROM approvals WHERE approval_id = ?1",
        params![approval_id],
        map_row,
    )
    .optional()
    .map_err(db_err)
}

fn load_active_by_fingerprint(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    fingerprint: &str,
) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        "SELECT approval_id, workspace_id, tool, fingerprint, params_json, state, resolved_at, result_json
         FROM approvals
         WHERE workspace_id = ?1 AND fingerprint = ?2
           AND state IN ('pending','granted','queued','resuming')
         LIMIT 1",
        params![workspace_id, fingerprint],
        map_row,
    )
    .optional()
    .map_err(db_err)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        approval_id: row.get(0)?,
        workspace_id: row.get(1)?,
        tool: row.get(2)?,
        fingerprint: row.get(3)?,
        params_json: row.get(4)?,
        state: row.get(5)?,
        resolved_at: row.get(6)?,
        result_json: row.get(7)?,
    })
}

fn scrubbed_params(tool: &str, workspace_id: &str, fingerprint: &str) -> String {
    serde_json::json!({
        "scrubbed": true,
        "tool": tool,
        "workspace_id": workspace_id,
        "fingerprint": fingerprint,
    })
    .to_string()
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

fn replay_consumed<'a>(
    approval_id: &ApprovalId,
    result_json: Option<&str>,
) -> Result<ResumeClaim<'a>, ErrorBody> {
    match result_json {
        Some(raw) => match serde_json::from_str::<PersistedResume>(raw) {
            Ok(PersistedResume::Success(result)) => Ok(ResumeClaim::ReplaySuccess(result)),
            Ok(PersistedResume::Failed(err)) => Ok(ResumeClaim::ReplayError(err)),
            Err(_) => Err(ambiguous(
                approval_id,
                "consumed approval has an unreadable result",
                None,
            )),
        },
        None => Err(ambiguous(
            approval_id,
            "consumed approval is missing a terminal result",
            None,
        )),
    }
}

fn ambiguous(
    id: &ApprovalId,
    message: impl Into<String>,
    operation_id: Option<String>,
) -> ErrorBody {
    let mut err =
        ErrorBody::new(ErrorCode::ApprovalAmbiguous, message).with_approval_id(id.0.as_str());
    if let Some(operation_id) = operation_id {
        err = err.with_operation_id(operation_id);
    }
    err
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

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(info, _)
            if info.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{ApplyPatchParams, OperationId, PatchStatus, WorkspaceId};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn patch_params() -> ApplyPatchParams {
        ApplyPatchParams {
            workspace_id: WorkspaceId("demo".into()),
            patch: "x".into(),
            expected_versions: BTreeMap::new(),
            operation_key: None,
            check_only: false,
            work_id: None,
        }
    }

    #[test]
    fn grant_then_consume_replays_stored_result() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &Store::fingerprint(&params),
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        assert_eq!(created.state, ApprovalState::Pending);
        assert!(created.approval_id.0.starts_with("appr-"));

        let granted = store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        assert_eq!(granted.state, ApprovalState::Granted);

        let result = OperationResumeResult {
            approval_id: created.approval_id.clone(),
            state: ApprovalState::Consumed,
            apply_patch: Some(codespace_domain::ApplyPatchResult::new(
                PatchStatus::Applied,
                OperationId("op-1".into()),
            )),
            exec_command: None,
        };
        {
            let ResumeClaim::Execute(record, _guard) =
                store.claim_resume(&created.approval_id).unwrap()
            else {
                panic!("expected execute");
            };
            assert_eq!(record.tool, ApprovalTargetTool::ApplyPatch);
            assert_eq!(
                store.claim_resume(&created.approval_id).unwrap_err().code,
                ErrorCode::ApprovalConflict
            );
            store
                .finish_resume(&created.approval_id, Ok(&result))
                .unwrap();
        }
        match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::ReplaySuccess(replayed) => {
                assert_eq!(replayed.apply_patch.unwrap().status, PatchStatus::Applied);
            }
            other => panic!("expected replay, got {other:?}"),
        }
        let (_, params_json, _) = store.inspect_approval(&created.approval_id).unwrap();
        assert!(params_json.contains("\"scrubbed\":true"));
    }

    #[test]
    fn deny_scrubs_and_cannot_resume() {
        let store = Store::memory().unwrap();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ExecCommand,
                "sha256:exec",
                json!({"workspace_id":"demo","command":["/bin/echo"]}),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Deny)
            .unwrap();
        let (_, params_json, _) = store.inspect_approval(&created.approval_id).unwrap();
        assert!(params_json.contains("\"scrubbed\":true"));
        assert_eq!(
            store.claim_resume(&created.approval_id).unwrap_err().code,
            ErrorCode::ApprovalConflict
        );
    }

    #[test]
    fn create_reuses_active_fingerprint() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let fp = Store::fingerprint(&params);
        let first = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &fp,
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        let second = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &fp,
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        assert_eq!(first.approval_id, second.approval_id);
    }

    #[test]
    fn consumed_without_result_is_ambiguous() {
        let store = Store::memory().unwrap();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                "sha256:x",
                json!({"workspace_id":"demo","patch":"x"}),
            )
            .unwrap();
        store
            .force_approval_state(&created.approval_id, ApprovalState::Consumed, true)
            .unwrap();
        assert_eq!(
            store.claim_resume(&created.approval_id).unwrap_err().code,
            ErrorCode::ApprovalAmbiguous
        );
    }

    #[test]
    fn persist_failure_leaves_resuming() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &Store::fingerprint(&params),
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        let result = OperationResumeResult {
            approval_id: created.approval_id.clone(),
            state: ApprovalState::Consumed,
            apply_patch: Some(codespace_domain::ApplyPatchResult::new(
                PatchStatus::Applied,
                OperationId("op-1".into()),
            )),
            exec_command: None,
        };
        let _guard = match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::Execute(_, guard) => guard,
            other => panic!("expected execute, got {other:?}"),
        };
        store.mark_resuming(&created.approval_id).unwrap();
        store.fail_next_finish();
        assert_eq!(
            store
                .finish_resume(&created.approval_id, Ok(&result))
                .unwrap_err()
                .code,
            ErrorCode::ApprovalAmbiguous
        );
        let (state, _, result_json) = store.inspect_approval(&created.approval_id).unwrap();
        assert_eq!(state, ApprovalState::Resuming);
        assert!(result_json.is_none());
    }

    #[test]
    fn restart_resuming_is_reconcile() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &Store::fingerprint(&params),
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        drop(match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::Execute(_, guard) => guard,
            other => panic!("expected execute, got {other:?}"),
        });
        store.mark_resuming(&created.approval_id).unwrap();
        match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::Reconcile(record, _guard) => {
                assert_eq!(record.state, ApprovalState::Resuming);
            }
            other => panic!("expected reconcile, got {other:?}"),
        };
    }

    #[test]
    fn restart_queued_is_execute() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &Store::fingerprint(&params),
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        drop(match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::Execute(_, guard) => guard,
            other => panic!("expected execute, got {other:?}"),
        });
        let (state, _, _) = store.inspect_approval(&created.approval_id).unwrap();
        assert_eq!(state, ApprovalState::Queued);
        match store.claim_resume(&created.approval_id).unwrap() {
            ResumeClaim::Execute(record, _guard) => {
                assert_eq!(record.tool, ApprovalTargetTool::ApplyPatch);
            }
            other => panic!("expected execute, got {other:?}"),
        };
    }

    #[test]
    fn queued_scheduler_errors_do_not_consume_approval() {
        for (index, code) in [ErrorCode::WorkspaceBusy, ErrorCode::ResourceQueueFull]
            .into_iter()
            .enumerate()
        {
            let store = Store::memory().unwrap();
            let created = store
                .create_approval(
                    "demo",
                    ApprovalTargetTool::ExecCommand,
                    &format!("sha256:retryable-{index}"),
                    json!({"workspace_id":"demo","command":["/bin/echo","ok"]}),
                )
                .unwrap();
            store
                .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
                .unwrap();

            let guard = match store.claim_resume(&created.approval_id).unwrap() {
                ResumeClaim::Execute(_, guard) => guard,
                other => panic!("expected execute, got {other:?}"),
            };
            let err = ErrorBody::new(code, "transient scheduler conflict");
            store
                .finish_resume(&created.approval_id, Err(&err))
                .unwrap();

            let (state, params_json, result_json) =
                store.inspect_approval(&created.approval_id).unwrap();
            assert_eq!(state, ApprovalState::Queued);
            assert!(!params_json.contains("\"scrubbed\":true"));
            assert!(result_json.is_none());

            drop(guard);
            match store.claim_resume(&created.approval_id).unwrap() {
                ResumeClaim::Execute(record, _guard) => {
                    assert_eq!(record.state, ApprovalState::Queued);
                }
                other => panic!("expected retryable execute, got {other:?}"),
            };
        }
    }

    #[test]
    fn consumed_fingerprint_can_open_a_new_hold() {
        let store = Store::memory().unwrap();
        let params = patch_params();
        let fp = Store::fingerprint(&params);
        let snapshot = serde_json::to_value(&params).unwrap();
        let first = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &fp,
                snapshot.clone(),
            )
            .unwrap();
        store
            .resolve_approval(&first.approval_id, ApprovalDecision::Grant)
            .unwrap();
        let result = OperationResumeResult {
            approval_id: first.approval_id.clone(),
            state: ApprovalState::Consumed,
            apply_patch: Some(codespace_domain::ApplyPatchResult::new(
                PatchStatus::Applied,
                OperationId("op-1".into()),
            )),
            exec_command: None,
        };
        {
            let ResumeClaim::Execute(_, _guard) = store.claim_resume(&first.approval_id).unwrap()
            else {
                panic!("expected execute");
            };
            store
                .finish_resume(&first.approval_id, Ok(&result))
                .unwrap();
        }
        let second = store
            .create_approval("demo", ApprovalTargetTool::ApplyPatch, &fp, snapshot)
            .unwrap();
        assert_ne!(first.approval_id, second.approval_id);
        assert_eq!(second.state, ApprovalState::Pending);
    }

    #[test]
    fn concurrent_claim_executes_once() {
        let store = std::sync::Arc::new(Store::memory().unwrap());
        let params = patch_params();
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ApplyPatch,
                &Store::fingerprint(&params),
                serde_json::to_value(&params).unwrap(),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let seen = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let spawn =
            |store: std::sync::Arc<Store>,
             id: ApprovalId,
             start: std::sync::Arc<std::sync::Barrier>,
             seen: std::sync::Arc<std::sync::Barrier>,
             outcomes: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>| {
                std::thread::spawn(move || {
                    start.wait();
                    let claim = store.claim_resume(&id);
                    let label = match &claim {
                        Ok(ResumeClaim::Execute(_, _)) => "execute",
                        Ok(ResumeClaim::Reconcile(_, _)) => "reconcile",
                        Err(err) if err.code == ErrorCode::ApprovalConflict => "conflict",
                        other => panic!("unexpected claim: {other:?}"),
                    };
                    outcomes.lock().expect("outcomes").push(label);
                    seen.wait();
                    drop(claim);
                })
            };
        let first = spawn(
            store.clone(),
            created.approval_id.clone(),
            start.clone(),
            seen.clone(),
            outcomes.clone(),
        );
        let second = spawn(store, created.approval_id, start, seen, outcomes.clone());
        first.join().unwrap();
        second.join().unwrap();
        let labels = outcomes.lock().expect("outcomes").clone();
        assert_eq!(
            labels.iter().filter(|label| **label == "execute").count(),
            1
        );
        assert_eq!(
            labels.iter().filter(|label| **label == "conflict").count(),
            1
        );
    }
}
