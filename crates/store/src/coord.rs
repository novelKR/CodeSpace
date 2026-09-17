//! Work / UserIntent persistence. MCP revision is unknown here.

use codespace_domain::{
    ClaimedIntent, CoordinationHint, DeliveryPolicy, ErrorBody, ErrorCode, IntentId, IntentKind,
    IntentState, SteerClaimNextResult, SteerOutcome, SteerStatusResult, UserIntent, Work,
    WorkFinishResult, WorkId, WorkOpenResult, WorkState, WorkspaceId, FINISH_REASON_PENDING,
};
use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::{now_secs, Store};

pub struct CreateIntent {
    pub body: String,
    pub kind: IntentKind,
    pub delivery: DeliveryPolicy,
}

impl Store {
    pub fn open_work(
        &self,
        workspace_id: &WorkspaceId,
        title: Option<String>,
    ) -> Result<WorkOpenResult, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let now = now_secs();
        let work_id = WorkId(format!("work-{}", Uuid::new_v4()));
        tx.execute(
            "INSERT INTO works (work_id, workspace_id, title, state, queue_revision, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)",
            params![
                work_id.0,
                workspace_id.0,
                title,
                WorkState::Active.as_str(),
                now
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE intents
             SET work_id = ?1,
                 delivery = CASE WHEN delivery = 'next_work' THEN 'after_current_work' ELSE delivery END,
                 updated_at = ?2,
                 revision = revision + 1
             WHERE workspace_id = ?3 AND work_id IS NULL AND state IN ('draft', 'queued')",
            params![work_id.0, now, workspace_id.0],
        )
        .map_err(db_err)?;
        bump_revision(&tx, &work_id.0)?;
        tx.commit().map_err(db_err)?;
        Ok(WorkOpenResult {
            work_id,
            workspace_id: workspace_id.clone(),
            state: WorkState::Active,
        })
    }

    pub fn get_work(&self, work_id: &WorkId) -> Result<Work, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        load_work(&conn, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))
    }

    pub fn list_works(&self, workspace_id: Option<&str>) -> Result<Vec<Work>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let mut sql =
            "SELECT work_id, workspace_id, title, state, queue_revision FROM works".to_string();
        if workspace_id.is_some() {
            sql.push_str(" WHERE workspace_id = ?1");
        }
        sql.push_str(" ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = if let Some(ws) = workspace_id {
            stmt.query_map(params![ws], work_from_row)
        } else {
            stmt.query_map([], work_from_row)
        }
        .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    pub fn unique_open_work(&self, workspace_id: &str) -> Result<Option<Work>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let mut stmt = conn
            .prepare(
                "SELECT work_id, workspace_id, title, state, queue_revision FROM works
                 WHERE workspace_id = ?1 AND state IN ('active', 'closing')
                 ORDER BY created_at",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![workspace_id], work_from_row)
            .map_err(db_err)?;
        let found = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
        if found.len() == 1 {
            Ok(found.into_iter().next())
        } else {
            Ok(None)
        }
    }

    pub fn coordination_hint(
        &self,
        workspace_id: &str,
        work_id: Option<&WorkId>,
    ) -> Result<Option<CoordinationHint>, ErrorBody> {
        let work = if let Some(id) = work_id {
            match self.get_work(id) {
                Ok(work) if work.workspace_id.0 == workspace_id && work.state.is_open() => work,
                Ok(_) => return Ok(None),
                Err(err) if err.code == ErrorCode::WorkNotFound => return Ok(None),
                Err(err) => return Err(err),
            }
        } else {
            match self.unique_open_work(workspace_id)? {
                Some(work) => work,
                None => return Ok(None),
            }
        };
        let pending = self.pending_user_items(&work.work_id)?;
        Ok(Some(CoordinationHint {
            work_id: work.work_id,
            pending_user_items: pending,
            queue_revision: work.queue_revision,
        }))
    }

    pub fn pending_user_items(&self, work_id: &WorkId) -> Result<u32, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM intents
                 WHERE work_id = ?1 AND state IN ('queued', 'claimed') AND delivery != 'next_work'",
                params![work_id.0],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(n as u32)
    }

    pub fn steer_status(&self, work_id: &WorkId) -> Result<SteerStatusResult, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let work = load_work(&conn, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        let queued = count_state(&conn, &work_id.0, "queued")?;
        let claimed = count_state(&conn, &work_id.0, "claimed")?;
        let claimable_now = count_claimable(&conn, &work_id.0, work.state)?;
        Ok(SteerStatusResult {
            work_id: work.work_id,
            state: work.state,
            queued,
            claimed,
            claimable_now,
            queue_revision: work.queue_revision,
        })
    }

    pub fn claim_next(&self, work_id: &WorkId) -> Result<SteerClaimNextResult, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let work = load_work(&tx, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        if !work.state.accepts_claims() {
            return Err(ErrorBody::new(
                ErrorCode::WorkClosed,
                "work is closed; open a new work_id",
            ));
        }
        let deliveries = claimable_deliveries(work.state);
        let selected = tx
            .query_row(
                &format!(
                    "SELECT intent_id, revision, kind, delivery, body FROM intents
                     WHERE work_id = ?1 AND state = 'queued' AND delivery IN ({})
                     ORDER BY position ASC LIMIT 1",
                    deliveries
                ),
                params![work_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err)?;
        let Some((intent_id, revision, kind, delivery, body)) = selected else {
            tx.commit().map_err(db_err)?;
            return Ok(SteerClaimNextResult { item: None });
        };
        let now = now_secs();
        let n = tx
            .execute(
                "UPDATE intents
                 SET state = 'claimed', claimed_at = ?1, updated_at = ?1
                 WHERE intent_id = ?2 AND state = 'queued' AND revision = ?3",
                params![now, intent_id, revision],
            )
            .map_err(db_err)?;
        if n != 1 {
            tx.commit().map_err(db_err)?;
            return Ok(SteerClaimNextResult { item: None });
        }
        bump_revision(&tx, &work_id.0)?;
        tx.commit().map_err(db_err)?;
        Ok(SteerClaimNextResult {
            item: Some(ClaimedIntent {
                intent_id: IntentId(intent_id),
                revision: revision as u64,
                kind: IntentKind::parse(&kind).unwrap_or(IntentKind::FollowUp),
                delivery: DeliveryPolicy::parse(&delivery).unwrap_or_default(),
                content: body,
            }),
        })
    }

    pub fn complete_intent(
        &self,
        work_id: &WorkId,
        intent_id: &IntentId,
        outcome: SteerOutcome,
    ) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let intent = load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        if intent.work_id.as_ref() != Some(work_id) {
            return Err(intent_missing(&intent_id.0));
        }
        if intent.state != IntentState::Claimed {
            return Err(ErrorBody::new(
                ErrorCode::IntentNotEditable,
                "only claimed intents can be completed",
            ));
        }
        let now = now_secs();
        let next = match outcome {
            SteerOutcome::Done => IntentState::Done,
            SteerOutcome::Blocked => IntentState::Blocked,
        };
        tx.execute(
            "UPDATE intents SET state = ?1, completed_at = ?2, updated_at = ?2 WHERE intent_id = ?3",
            params![next.as_str(), now, intent_id.0],
        )
        .map_err(db_err)?;
        bump_revision(&tx, &work_id.0)?;
        tx.commit().map_err(db_err)?;
        let mut done = intent;
        done.state = next;
        Ok(done)
    }

    pub fn finish_work(&self, work_id: &WorkId) -> Result<WorkFinishResult, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let work = load_work(&tx, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        if !work.state.is_open() {
            return Err(ErrorBody::new(
                ErrorCode::WorkClosed,
                "work is already closed",
            ));
        }
        let claimed = count_state(&tx, &work_id.0, "claimed")?;
        let queued_checkpoint = count_delivery(&tx, &work_id.0, "queued", "next_checkpoint")?;
        let queued_after = count_delivery(&tx, &work_id.0, "queued", "after_current_work")?;
        let remaining = claimed + queued_checkpoint + queued_after;

        if claimed > 0 || queued_checkpoint > 0 {
            tx.commit().map_err(db_err)?;
            return Ok(WorkFinishResult {
                work_id: work.work_id,
                closed: false,
                reason: Some(FINISH_REASON_PENDING.to_string()),
                queued: remaining,
                state: work.state,
            });
        }

        if queued_after > 0 {
            if work.state == WorkState::Active {
                tx.execute(
                    "UPDATE works SET state = ?1 WHERE work_id = ?2",
                    params![WorkState::Closing.as_str(), work_id.0],
                )
                .map_err(db_err)?;
                bump_revision(&tx, &work_id.0)?;
                tx.commit().map_err(db_err)?;
                return Ok(WorkFinishResult {
                    work_id: work.work_id,
                    closed: false,
                    reason: Some(FINISH_REASON_PENDING.to_string()),
                    queued: queued_after,
                    state: WorkState::Closing,
                });
            }
            tx.commit().map_err(db_err)?;
            return Ok(WorkFinishResult {
                work_id: work.work_id,
                closed: false,
                reason: Some(FINISH_REASON_PENDING.to_string()),
                queued: queued_after,
                state: WorkState::Closing,
            });
        }

        let now = now_secs();
        tx.execute(
            "UPDATE intents
             SET work_id = NULL, updated_at = ?1
             WHERE work_id = ?2 AND (state IN ('draft', 'queued') OR delivery = 'next_work')",
            params![now, work_id.0],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE works SET state = ?1, closed_at = ?2, queue_revision = queue_revision + 1
             WHERE work_id = ?3",
            params![WorkState::Closed.as_str(), now, work_id.0],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(WorkFinishResult {
            work_id: work.work_id,
            closed: true,
            reason: None,
            queued: 0,
            state: WorkState::Closed,
        })
    }

    pub fn create_draft(
        &self,
        work_id: &WorkId,
        create: CreateIntent,
    ) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let work = load_work(&tx, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        let now = now_secs();
        let attach = work.state.is_open();
        let stored_work_id = if attach {
            Some(work_id.0.as_str())
        } else {
            None
        };
        let position = next_position(&tx, stored_work_id, &work.workspace_id.0)?;
        let intent_id = format!("fb-{}", Uuid::new_v4());
        let delivery = if attach {
            create.delivery
        } else {
            DeliveryPolicy::NextWork
        };
        tx.execute(
            "INSERT INTO intents (
                intent_id, work_id, workspace_id, position, revision, kind, delivery, body, state,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, 'draft', ?8, ?8)",
            params![
                intent_id,
                stored_work_id,
                work.workspace_id.0,
                position,
                create.kind.as_str(),
                delivery.as_str(),
                create.body,
                now
            ],
        )
        .map_err(db_err)?;
        if attach {
            bump_revision(&tx, &work_id.0)?;
        }
        let intent = load_intent(&tx, &intent_id)?.ok_or_else(|| intent_missing(&intent_id))?;
        tx.commit().map_err(db_err)?;
        Ok(intent)
    }

    pub fn list_intents(
        &self,
        work_id: &WorkId,
        include_draft: bool,
    ) -> Result<Vec<UserIntent>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let work = load_work(&conn, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        list_intents_conn(&conn, &work.work_id.0, &work.workspace_id.0, include_draft)
    }

    pub fn edit_intent(
        &self,
        intent_id: &IntentId,
        revision: u64,
        body: String,
        delivery: Option<DeliveryPolicy>,
    ) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let current =
            load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        if current.state == IntentState::Claimed {
            return Err(ErrorBody::new(
                ErrorCode::IntentAlreadyClaimed,
                "this intent was already read; add a new item",
            ));
        }
        if !current.state.is_editable() {
            return Err(ErrorBody::new(
                ErrorCode::IntentNotEditable,
                "only draft and queued intents can be edited",
            ));
        }
        if current.revision != revision {
            return Err(ErrorBody::new(
                ErrorCode::IntentRevisionConflict,
                "intent revision does not match",
            ));
        }
        let now = now_secs();
        let next_delivery = delivery.unwrap_or(current.delivery);
        let n = tx
            .execute(
                "UPDATE intents
                 SET body = ?1, delivery = ?2, revision = revision + 1, updated_at = ?3
                 WHERE intent_id = ?4 AND revision = ?5 AND state IN ('draft', 'queued')",
                params![
                    body,
                    next_delivery.as_str(),
                    now,
                    intent_id.0,
                    revision as i64
                ],
            )
            .map_err(db_err)?;
        if n != 1 {
            return Err(ErrorBody::new(
                ErrorCode::IntentAlreadyClaimed,
                "this intent was already read; add a new item",
            ));
        }
        if let Some(work_id) = current.work_id.as_ref() {
            bump_revision(&tx, &work_id.0)?;
        }
        let intent = load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        tx.commit().map_err(db_err)?;
        Ok(intent)
    }

    pub fn queue_intent(
        &self,
        intent_id: &IntentId,
        revision: u64,
    ) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let current =
            load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        if current.state != IntentState::Draft {
            return Err(ErrorBody::new(
                ErrorCode::IntentNotEditable,
                "only drafts can be queued",
            ));
        }
        if current.revision != revision {
            return Err(ErrorBody::new(
                ErrorCode::IntentRevisionConflict,
                "intent revision does not match",
            ));
        }
        let now = now_secs();
        let n = tx
            .execute(
                "UPDATE intents
                 SET state = 'queued', revision = revision + 1, updated_at = ?1
                 WHERE intent_id = ?2 AND revision = ?3 AND state = 'draft'",
                params![now, intent_id.0, revision as i64],
            )
            .map_err(db_err)?;
        if n != 1 {
            return Err(ErrorBody::new(
                ErrorCode::IntentAlreadyClaimed,
                "this intent was already read; add a new item",
            ));
        }
        if let Some(work_id) = current.work_id.as_ref() {
            bump_revision(&tx, &work_id.0)?;
        }
        let intent = load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        tx.commit().map_err(db_err)?;
        Ok(intent)
    }

    pub fn cancel_intent(&self, intent_id: &IntentId) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let current =
            load_intent(&tx, &intent_id.0)?.ok_or_else(|| intent_missing(&intent_id.0))?;
        if !current.state.is_editable() {
            return Err(ErrorBody::new(
                ErrorCode::IntentNotEditable,
                "claimed intents cannot be cancelled; add a new item",
            ));
        }
        let now = now_secs();
        tx.execute(
            "UPDATE intents SET state = 'cancelled', updated_at = ?1 WHERE intent_id = ?2",
            params![now, intent_id.0],
        )
        .map_err(db_err)?;
        if let Some(work_id) = current.work_id.as_ref() {
            bump_revision(&tx, &work_id.0)?;
        }
        tx.commit().map_err(db_err)?;
        let mut done = current;
        done.state = IntentState::Cancelled;
        Ok(done)
    }

    pub fn reorder_queued(
        &self,
        work_id: &WorkId,
        intent_ids: &[IntentId],
    ) -> Result<Vec<UserIntent>, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let work = load_work(&tx, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        let mut stmt = tx
            .prepare(
                "SELECT intent_id FROM intents WHERE work_id = ?1 AND state = 'queued' ORDER BY position",
            )
            .map_err(db_err)?;
        let existing: Vec<String> = stmt
            .query_map(params![work_id.0], |row| row.get(0))
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        drop(stmt);
        if existing.len() != intent_ids.len()
            || !intent_ids
                .iter()
                .all(|id| existing.iter().any(|e| e == &id.0))
        {
            return Err(ErrorBody::new(
                ErrorCode::IntentNotEditable,
                "reorder must list exactly the queued intents on this work",
            ));
        }
        let now = now_secs();
        for (idx, id) in intent_ids.iter().enumerate() {
            tx.execute(
                "UPDATE intents SET position = ?1, updated_at = ?2 WHERE intent_id = ?3 AND state = 'queued'",
                params![(idx as i64) + 1, now, id.0],
            )
            .map_err(db_err)?;
        }
        bump_revision(&tx, &work_id.0)?;
        let listed = list_intents_conn(&tx, &work.work_id.0, &work.workspace_id.0, true)?;
        tx.commit().map_err(db_err)?;
        Ok(listed)
    }

    pub fn insert_stop_notice(
        &self,
        work_id: &WorkId,
        reason: &str,
    ) -> Result<UserIntent, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(db_err)?;
        let work = load_work(&tx, &work_id.0)?.ok_or_else(|| work_missing(&work_id.0))?;
        let now = now_secs();
        let attach = work.state.is_open();
        let stored_work_id = if attach {
            Some(work_id.0.as_str())
        } else {
            None
        };
        let position = next_position(&tx, stored_work_id, &work.workspace_id.0)?;
        let intent_id = format!("fb-{}", Uuid::new_v4());
        let body = if reason.trim().is_empty() {
            "user stopped current execution".to_string()
        } else {
            format!("user stopped current execution: {reason}")
        };
        let state = if attach {
            IntentState::Claimed
        } else {
            IntentState::Queued
        };
        tx.execute(
            "INSERT INTO intents (
                intent_id, work_id, workspace_id, position, revision, kind, delivery, body, state,
                created_at, updated_at, claimed_at
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, 'next_checkpoint', ?6, ?7, ?8, ?8, ?9)",
            params![
                intent_id,
                stored_work_id,
                work.workspace_id.0,
                position,
                IntentKind::StopNotice.as_str(),
                body,
                state.as_str(),
                now,
                if attach { Some(now) } else { None }
            ],
        )
        .map_err(db_err)?;
        if attach {
            bump_revision(&tx, &work_id.0)?;
        }
        let intent = load_intent(&tx, &intent_id)?.ok_or_else(|| intent_missing(&intent_id))?;
        tx.commit().map_err(db_err)?;
        Ok(intent)
    }
}

fn list_intents_conn(
    conn: &rusqlite::Connection,
    work_id: &str,
    workspace_id: &str,
    include_draft: bool,
) -> Result<Vec<UserIntent>, ErrorBody> {
    let sql = if include_draft {
        "SELECT intent_id, work_id, workspace_id, position, revision, kind, delivery, body, state
         FROM intents
         WHERE work_id = ?1 OR (work_id IS NULL AND workspace_id = ?2)
         ORDER BY CASE state WHEN 'claimed' THEN 0 WHEN 'queued' THEN 1 WHEN 'draft' THEN 2 ELSE 3 END,
                  position"
    } else {
        "SELECT intent_id, work_id, workspace_id, position, revision, kind, delivery, body, state
         FROM intents
         WHERE (work_id = ?1 OR (work_id IS NULL AND workspace_id = ?2))
           AND state != 'draft'
         ORDER BY CASE state WHEN 'claimed' THEN 0 WHEN 'queued' THEN 1 ELSE 3 END, position"
    };
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let rows = stmt
        .query_map(params![work_id, workspace_id], intent_from_row)
        .map_err(db_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
}

fn load_work(conn: &rusqlite::Connection, work_id: &str) -> Result<Option<Work>, ErrorBody> {
    conn.query_row(
        "SELECT work_id, workspace_id, title, state, queue_revision FROM works WHERE work_id = ?1",
        params![work_id],
        work_from_row,
    )
    .optional()
    .map_err(db_err)
}

fn load_intent(
    conn: &rusqlite::Connection,
    intent_id: &str,
) -> Result<Option<UserIntent>, ErrorBody> {
    conn.query_row(
        "SELECT intent_id, work_id, workspace_id, position, revision, kind, delivery, body, state
         FROM intents WHERE intent_id = ?1",
        params![intent_id],
        intent_from_row,
    )
    .optional()
    .map_err(db_err)
}

fn work_from_row(row: &rusqlite::Row) -> rusqlite::Result<Work> {
    let state_raw: String = row.get(3)?;
    Ok(Work {
        work_id: WorkId(row.get(0)?),
        workspace_id: WorkspaceId(row.get(1)?),
        title: row.get(2)?,
        state: WorkState::parse(&state_raw).unwrap_or(WorkState::Active),
        queue_revision: row.get::<_, i64>(4)? as u64,
    })
}

fn intent_from_row(row: &rusqlite::Row) -> rusqlite::Result<UserIntent> {
    let work_id: Option<String> = row.get(1)?;
    let kind: String = row.get(5)?;
    let delivery: String = row.get(6)?;
    let state: String = row.get(8)?;
    Ok(UserIntent {
        intent_id: IntentId(row.get(0)?),
        work_id: work_id.map(WorkId),
        workspace_id: WorkspaceId(row.get(2)?),
        position: row.get(3)?,
        revision: row.get::<_, i64>(4)? as u64,
        kind: IntentKind::parse(&kind).unwrap_or(IntentKind::FollowUp),
        delivery: DeliveryPolicy::parse(&delivery).unwrap_or_default(),
        body: row.get(7)?,
        state: IntentState::parse(&state).unwrap_or(IntentState::Draft),
    })
}

fn bump_revision(tx: &Transaction, work_id: &str) -> Result<(), ErrorBody> {
    tx.execute(
        "UPDATE works SET queue_revision = queue_revision + 1 WHERE work_id = ?1",
        params![work_id],
    )
    .map_err(db_err)?;
    Ok(())
}

fn next_position(
    conn: &rusqlite::Connection,
    work_id: Option<&str>,
    workspace_id: &str,
) -> Result<i64, ErrorBody> {
    let n: i64 = if let Some(work_id) = work_id {
        conn.query_row(
            "SELECT COALESCE(MAX(position), 0) FROM intents WHERE work_id = ?1",
            params![work_id],
            |row| row.get(0),
        )
    } else {
        conn.query_row(
            "SELECT COALESCE(MAX(position), 0) FROM intents WHERE work_id IS NULL AND workspace_id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
    }
    .map_err(db_err)?;
    Ok(n + 1)
}

fn count_state(conn: &rusqlite::Connection, work_id: &str, state: &str) -> Result<u32, ErrorBody> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE work_id = ?1 AND state = ?2",
            params![work_id, state],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(n as u32)
}

fn count_delivery(
    conn: &rusqlite::Connection,
    work_id: &str,
    state: &str,
    delivery: &str,
) -> Result<u32, ErrorBody> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intents WHERE work_id = ?1 AND state = ?2 AND delivery = ?3",
            params![work_id, state, delivery],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    Ok(n as u32)
}

fn count_claimable(
    conn: &rusqlite::Connection,
    work_id: &str,
    state: WorkState,
) -> Result<u32, ErrorBody> {
    let sql = format!(
        "SELECT COUNT(*) FROM intents WHERE work_id = ?1 AND state = 'queued' AND delivery IN ({})",
        claimable_deliveries(state)
    );
    let n: i64 = conn
        .query_row(&sql, params![work_id], |row| row.get(0))
        .map_err(db_err)?;
    Ok(n as u32)
}

fn claimable_deliveries(state: WorkState) -> &'static str {
    match state {
        WorkState::Active => "'next_checkpoint'",
        WorkState::Closing => "'next_checkpoint', 'after_current_work'",
        WorkState::Closed | WorkState::Cancelled => "'__none__'",
    }
}

fn db_err(err: rusqlite::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::WorkNotFound, format!("store: {err}"))
}

fn work_missing(id: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::WorkNotFound, format!("unknown work_id `{id}`"))
}

fn intent_missing(id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::IntentNotFound,
        format!("unknown intent_id `{id}`"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn ws() -> WorkspaceId {
        WorkspaceId("demo".into())
    }

    fn draft(store: &Store, work: &WorkId, body: &str, delivery: DeliveryPolicy) -> UserIntent {
        store
            .create_draft(
                work,
                CreateIntent {
                    body: body.into(),
                    kind: IntentKind::FollowUp,
                    delivery,
                },
            )
            .unwrap()
    }

    fn queue(store: &Store, intent: &UserIntent) -> UserIntent {
        store
            .queue_intent(&intent.intent_id, intent.revision)
            .unwrap()
    }

    #[test]
    fn draft_is_hidden_from_claim_and_status_counts() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), Some("auth".into())).unwrap();
        draft(
            &store,
            &opened.work_id,
            "secret draft",
            DeliveryPolicy::NextCheckpoint,
        );
        let status = store.steer_status(&opened.work_id).unwrap();
        assert_eq!(status.queued, 0);
        assert_eq!(status.claimable_now, 0);
        assert!(store.claim_next(&opened.work_id).unwrap().item.is_none());
        let listed = store.list_intents(&opened.work_id, false).unwrap();
        assert!(listed.is_empty());
        let with_draft = store.list_intents(&opened.work_id, true).unwrap();
        assert_eq!(with_draft.len(), 1);
        assert_eq!(with_draft[0].state, IntentState::Draft);
    }

    #[test]
    fn claim_freezes_and_blocks_edit() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        let item = queue(
            &store,
            &draft(
                &store,
                &opened.work_id,
                "README",
                DeliveryPolicy::NextCheckpoint,
            ),
        );
        let claimed = store.claim_next(&opened.work_id).unwrap().item.unwrap();
        assert_eq!(claimed.content, "README");
        assert_eq!(claimed.intent_id, item.intent_id);
        let err = store
            .edit_intent(&item.intent_id, item.revision + 1, "do not".into(), None)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IntentAlreadyClaimed);
        assert!(store.claim_next(&opened.work_id).unwrap().item.is_none());
    }

    #[test]
    fn edit_wins_over_stale_revision_then_claim_sees_new_body() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        let item = queue(
            &store,
            &draft(
                &store,
                &opened.work_id,
                "old",
                DeliveryPolicy::NextCheckpoint,
            ),
        );
        let edited = store
            .edit_intent(&item.intent_id, item.revision, "new".into(), None)
            .unwrap();
        assert_eq!(edited.body, "new");
        let claimed = store.claim_next(&opened.work_id).unwrap().item.unwrap();
        assert_eq!(claimed.content, "new");
        let err = store
            .edit_intent(&item.intent_id, item.revision, "stale".into(), None)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IntentAlreadyClaimed);
    }

    #[test]
    fn reorder_only_queued_items() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        let a = queue(
            &store,
            &draft(&store, &opened.work_id, "a", DeliveryPolicy::NextCheckpoint),
        );
        let b = queue(
            &store,
            &draft(&store, &opened.work_id, "b", DeliveryPolicy::NextCheckpoint),
        );
        store
            .reorder_queued(&opened.work_id, &[b.intent_id.clone(), a.intent_id.clone()])
            .unwrap();
        let claimed = store.claim_next(&opened.work_id).unwrap().item.unwrap();
        assert_eq!(claimed.content, "b");
        let err = store
            .reorder_queued(&opened.work_id, &[a.intent_id.clone(), claimed.intent_id])
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IntentNotEditable);
    }

    #[test]
    fn finish_barrier_then_after_current_work_claim() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        queue(
            &store,
            &draft(
                &store,
                &opened.work_id,
                "later",
                DeliveryPolicy::AfterCurrentWork,
            ),
        );
        assert!(store.claim_next(&opened.work_id).unwrap().item.is_none());
        let first = store.finish_work(&opened.work_id).unwrap();
        assert!(!first.closed);
        assert_eq!(first.reason.as_deref(), Some(FINISH_REASON_PENDING));
        assert_eq!(first.state, WorkState::Closing);
        let claimed = store.claim_next(&opened.work_id).unwrap().item.unwrap();
        assert_eq!(claimed.content, "later");
        store
            .complete_intent(&opened.work_id, &claimed.intent_id, SteerOutcome::Done)
            .unwrap();
        let closed = store.finish_work(&opened.work_id).unwrap();
        assert!(closed.closed);
        assert_eq!(closed.state, WorkState::Closed);
    }

    #[test]
    fn closed_work_new_intent_goes_to_next_batch() {
        let store = Store::memory().unwrap();
        let first = store.open_work(&ws(), None).unwrap();
        store.finish_work(&first.work_id).unwrap();
        let overflow = store
            .create_draft(
                &first.work_id,
                CreateIntent {
                    body: "next please".into(),
                    kind: IntentKind::FollowUp,
                    delivery: DeliveryPolicy::AfterCurrentWork,
                },
            )
            .unwrap();
        assert!(overflow.work_id.is_none());
        assert_eq!(overflow.delivery, DeliveryPolicy::NextWork);
        queue(&store, &overflow);
        let second = store.open_work(&ws(), None).unwrap();
        let listed = store.list_intents(&second.work_id, true).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].body, "next please");
        assert_eq!(listed[0].work_id.as_ref(), Some(&second.work_id));
        assert_eq!(listed[0].delivery, DeliveryPolicy::AfterCurrentWork);
    }

    #[test]
    fn next_work_delivery_never_claimed_on_current_work() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        queue(
            &store,
            &draft(
                &store,
                &opened.work_id,
                "later job",
                DeliveryPolicy::NextWork,
            ),
        );
        assert!(store.claim_next(&opened.work_id).unwrap().item.is_none());
        let closed = store.finish_work(&opened.work_id).unwrap();
        assert!(closed.closed);
        let next = store.open_work(&ws(), None).unwrap();
        let listed = store.list_intents(&next.work_id, false).unwrap();
        assert_eq!(listed[0].body, "later job");
    }

    #[test]
    fn stop_notice_is_claimed_and_not_editable() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        let notice = store
            .insert_stop_notice(&opened.work_id, "wrong target")
            .unwrap();
        assert_eq!(notice.state, IntentState::Claimed);
        assert_eq!(notice.kind, IntentKind::StopNotice);
        let err = store
            .edit_intent(&notice.intent_id, notice.revision, "nope".into(), None)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::IntentAlreadyClaimed);
    }

    #[test]
    fn intent_is_not_a_capability() {
        let store = Store::memory().unwrap();
        let opened = store.open_work(&ws(), None).unwrap();
        let item = queue(
            &store,
            &draft(
                &store,
                &opened.work_id,
                "edit /etc/passwd",
                DeliveryPolicy::NextCheckpoint,
            ),
        );
        assert_eq!(item.body, "edit /etc/passwd");
        assert_eq!(
            store.get_work(&opened.work_id).unwrap().workspace_id.0,
            "demo"
        );
    }
}
