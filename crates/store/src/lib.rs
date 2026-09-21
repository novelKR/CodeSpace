//! Single-instance SQLite operations, confirmation holds, and in-memory
//! resource occupancy. Occupancy is not a SQLite schema. Request-owned
//! conflicts wait on a per-resource FIFO; a live process still rejects
//! immediately with `WORKSPACE_BUSY`.
//! HTTP/JSON-RPC request ids are never stored as [`OperationId`] values.

mod approvals;
mod coord;
mod resource;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use codespace_domain::{
    ApplyPatchParams, ApplyPatchResult, ErrorBody, ErrorCode, ExecCommandParams, OperationEvent,
    OperationEventName, OperationId, OperationKey, OperationKind, OperationStatusResult,
    PatchStatus, WorkspaceId,
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub use approvals::{ApprovalRecord, ResumeClaim};
pub use coord::CreateIntent;
use resource::AcquireOutcome;
pub use resource::{LockMode, Resource, ResourceGuard};

#[derive(Debug, Clone)]
pub struct StoredOperation {
    pub operation_id: OperationId,
    pub status: PatchStatus,
    pub replayed: bool,
    pub result: ApplyPatchResult,
    pub workspace_id: WorkspaceId,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub events: Vec<OperationEvent>,
}

#[derive(Debug)]
pub enum Begin {
    Fresh(OperationId),
    Replayed(Box<StoredOperation>),
}

pub struct Store {
    conn: Mutex<Connection>,
    locks: Mutex<resource::ResourceSerializer>,
    resume_inflight: Mutex<HashSet<String>>,
    fail_next_finish: Mutex<bool>,
}

pub struct WriteGuard<'a> {
    store: &'a Store,
    workspace_id: String,
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.store.release_write(&self.workspace_id);
    }
}

struct WaitTicket<'a> {
    store: &'a Store,
    resource: Resource,
    mode: LockMode,
    id: u64,
    rx: tokio::sync::oneshot::Receiver<Result<(), ErrorBody>>,
    finished: bool,
}

impl WaitTicket<'_> {
    async fn wait(mut self) -> Result<(), ErrorBody> {
        let result = match (&mut self.rx).await {
            Ok(value) => value,
            Err(_) => Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "resource waiter closed",
            )),
        };
        self.finished = true;
        result
    }
}

impl Drop for WaitTicket<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut serializer = self.store.locks.lock().expect("lock mutex");
        if serializer.cancel_waiter(&self.resource, self.id) {
            return;
        }
        if matches!(self.rx.try_recv(), Ok(Ok(()))) {
            serializer.unlock(&self.resource, self.mode);
        }
    }
}

impl Store {
    pub fn memory() -> Result<Self, String> {
        Self::from_connection(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        Self::from_connection(Connection::open(path).map_err(|e| e.to_string())?)
    }

    fn from_connection(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS operations (
                operation_id TEXT PRIMARY KEY,
                operation_key TEXT,
                workspace_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                finished_at INTEGER,
                events_json TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_operations_key
                ON operations(operation_key)
                WHERE operation_key IS NOT NULL;
            CREATE TABLE IF NOT EXISTS works (
                work_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                title TEXT,
                state TEXT NOT NULL,
                queue_revision INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                closed_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_works_workspace ON works(workspace_id);
            CREATE TABLE IF NOT EXISTS intents (
                intent_id TEXT PRIMARY KEY,
                work_id TEXT,
                workspace_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1,
                kind TEXT NOT NULL,
                delivery TEXT NOT NULL,
                body TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                claimed_at INTEGER,
                completed_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_intents_work_state
                ON intents(work_id, state, position);
            CREATE INDEX IF NOT EXISTS idx_intents_workspace_next
                ON intents(workspace_id, state)
                WHERE work_id IS NULL;",
        )
        .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS approvals (
                approval_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                tool TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                params_json TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                resolved_at INTEGER,
                result_json TEXT
            );",
        )
        .map_err(|e| e.to_string())?;
        migrate_operations(&conn)?;
        migrate_approvals(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            locks: Mutex::new(resource::ResourceSerializer::default()),
            resume_inflight: Mutex::new(HashSet::new()),
            fail_next_finish: Mutex::new(false),
        })
    }

    pub fn fingerprint(params: &ApplyPatchParams) -> String {
        hash_canonical(&serde_json::json!({
            "workspace_id": params.workspace_id.0,
            "patch": params.patch,
            "expected_versions": params.expected_versions,
            "check_only": params.check_only,
        }))
    }

    pub fn exec_fingerprint(params: &ExecCommandParams) -> String {
        hash_canonical(&serde_json::json!({
            "workspace_id": params.workspace_id.0,
            "command": params.command,
            "tty": params.tty,
        }))
    }

    pub fn try_acquire_write<'a>(
        &'a self,
        workspace_id: &str,
    ) -> Result<WriteGuard<'a>, ErrorBody> {
        self.locks
            .lock()
            .expect("lock mutex")
            .try_exclusive_write(workspace_id)?;
        Ok(WriteGuard {
            store: self,
            workspace_id: workspace_id.to_string(),
        })
    }

    pub async fn acquire_write<'a>(
        &'a self,
        workspace_id: &str,
    ) -> Result<WriteGuard<'a>, ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        let outcome = self
            .locks
            .lock()
            .expect("lock mutex")
            .acquire_exclusive_write(workspace_id);
        self.complete_acquire(resource, LockMode::Exclusive, outcome)
            .await?;
        Ok(WriteGuard {
            store: self,
            workspace_id: workspace_id.to_string(),
        })
    }

    fn release_write(&self, workspace_id: &str) {
        self.locks
            .lock()
            .expect("lock mutex")
            .release_write(workspace_id);
    }

    pub fn mark_shell_busy(&self, workspace_id: &str, process_id: &str) -> Result<(), ErrorBody> {
        self.locks
            .lock()
            .expect("lock mutex")
            .mark_shell_busy(workspace_id, process_id)
    }

    pub async fn acquire_shell_busy(
        &self,
        workspace_id: &str,
        process_id: &str,
    ) -> Result<(), ErrorBody> {
        let resource = Resource::Workspace(workspace_id.to_string());
        let outcome = self
            .locks
            .lock()
            .expect("lock mutex")
            .acquire_shell_busy(workspace_id, process_id);
        self.complete_acquire(resource, LockMode::Exclusive, outcome)
            .await
    }

    pub fn clear_shell(&self, workspace_id: &str) {
        self.locks
            .lock()
            .expect("lock mutex")
            .clear_shell(workspace_id);
    }

    pub fn release_process(&self, process_id: &str) {
        self.locks
            .lock()
            .expect("lock mutex")
            .release_process(process_id);
    }

    pub fn release_all_processes(&self) {
        self.locks
            .lock()
            .expect("lock mutex")
            .release_all_processes();
    }

    pub fn try_lock(
        &self,
        resource: Resource,
        mode: LockMode,
    ) -> Result<ResourceGuard<'_>, ErrorBody> {
        self.locks
            .lock()
            .expect("lock mutex")
            .try_lock(resource.clone(), mode)?;
        Ok(ResourceGuard::new(self, resource, mode))
    }

    pub async fn lock(
        &self,
        resource: Resource,
        mode: LockMode,
    ) -> Result<ResourceGuard<'_>, ErrorBody> {
        let outcome = self
            .locks
            .lock()
            .expect("lock mutex")
            .acquire_lock(resource.clone(), mode);
        self.complete_acquire(resource.clone(), mode, outcome)
            .await?;
        Ok(ResourceGuard::new(self, resource, mode))
    }

    fn release_resource(&self, resource: &Resource, mode: LockMode) {
        self.locks
            .lock()
            .expect("lock mutex")
            .unlock(resource, mode);
    }

    async fn complete_acquire(
        &self,
        resource: Resource,
        mode: LockMode,
        outcome: AcquireOutcome,
    ) -> Result<(), ErrorBody> {
        match outcome {
            AcquireOutcome::Granted => Ok(()),
            AcquireOutcome::Busy(err) => Err(err),
            AcquireOutcome::Waiting { id, rx } => {
                WaitTicket {
                    store: self,
                    resource,
                    mode,
                    id,
                    rx,
                    finished: false,
                }
                .wait()
                .await
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn waiter_count(&self, resource: &Resource) -> usize {
        self.locks
            .lock()
            .expect("lock mutex")
            .waiter_count(resource)
    }

    pub fn begin(
        &self,
        operation_key: Option<&OperationKey>,
        workspace_id: &str,
        fingerprint: &str,
    ) -> Result<Begin, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        if let Some(key) = operation_key {
            if let Some(row) = load_by_key(&conn, &key.0)? {
                if row.fingerprint != fingerprint {
                    return Err(ErrorBody::new(
                        ErrorCode::OperationKeyConflict,
                        "operation_key reused with a different request",
                    ));
                }
                return Ok(Begin::Replayed(Box::new(row.into_stored(true))));
            }
        }

        let operation_id = OperationId(format!("op-{}", uuid::Uuid::new_v4()));
        let pending = ApplyPatchResult::new(PatchStatus::Unknown, operation_id.clone());
        let result_json = serde_json::to_string(&pending).map_err(ser_err)?;
        let now = now_secs();
        let events = vec![OperationEvent::minted(now)];
        let events_json = serde_json::to_string(&events).map_err(ser_err)?;
        conn.execute(
            "INSERT INTO operations
                (operation_id, operation_key, workspace_id, fingerprint, status, result_json, created_at, finished_at, events_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
            params![
                operation_id.0,
                operation_key.map(|k| k.0.as_str()),
                workspace_id,
                fingerprint,
                status_str(PatchStatus::Unknown),
                result_json,
                now,
                events_json,
            ],
        )
        .map_err(sql_err)?;
        Ok(Begin::Fresh(operation_id))
    }

    pub fn finish(
        &self,
        operation_id: &OperationId,
        result: &ApplyPatchResult,
    ) -> Result<(), ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let Some(row) = load_by_id(&conn, &operation_id.0)? else {
            return Err(ErrorBody::new(
                ErrorCode::OperationNotFound,
                "unknown operation_id",
            ));
        };
        let now = now_secs();
        let mut events = row.events();
        if !events
            .iter()
            .any(|event| event.name == OperationEventName::Minted)
        {
            events.insert(0, OperationEvent::minted(row.created_at));
        }
        events.push(OperationEvent::finished(now, result.status));
        let events_json = serde_json::to_string(&events).map_err(ser_err)?;
        let json = serde_json::to_string(result).map_err(ser_err)?;
        conn.execute(
            "UPDATE operations
             SET status = ?1, result_json = ?2, finished_at = ?3, events_json = ?4
             WHERE operation_id = ?5",
            params![
                status_str(result.status),
                json,
                now,
                events_json,
                operation_id.0
            ],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    pub fn get(&self, operation_id: &OperationId) -> Result<StoredOperation, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        load_by_id(&conn, &operation_id.0)?
            .map(|row| row.into_stored(false))
            .ok_or_else(|| ErrorBody::new(ErrorCode::OperationNotFound, "unknown operation_id"))
    }

    pub fn status(&self, operation_id: &OperationId) -> Result<OperationStatusResult, ErrorBody> {
        self.status_lookup(Some(operation_id), None)
    }

    pub fn status_lookup(
        &self,
        operation_id: Option<&OperationId>,
        operation_key: Option<&OperationKey>,
    ) -> Result<OperationStatusResult, ErrorBody> {
        match (operation_id, operation_key) {
            (Some(id), None) => {
                let stored = self.get(id)?;
                Ok(to_status(stored))
            }
            (None, Some(key)) => {
                let conn = self.conn.lock().expect("sqlite mutex");
                let stored = load_by_key(&conn, &key.0)?
                    .map(|row| row.into_stored(false))
                    .ok_or_else(|| {
                        ErrorBody::new(ErrorCode::OperationNotFound, "unknown operation_key")
                    })?;
                Ok(to_status(stored))
            }
            _ => Err(ErrorBody::new(
                ErrorCode::OperationNotFound,
                "provide exactly one of operation_id or operation_key",
            )),
        }
    }

    pub fn find_operation_by_fingerprint(
        &self,
        workspace_id: &str,
        fingerprint: &str,
        created_not_before: Option<i64>,
    ) -> Result<Option<StoredOperation>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let sql = format!(
            "{OPERATION_SELECT} WHERE workspace_id = ?1 AND fingerprint = ?2 AND (?3 IS NULL OR created_at >= ?3) ORDER BY created_at DESC LIMIT 1"
        );
        conn.query_row(
            &sql,
            params![workspace_id, fingerprint, created_not_before],
            map_operation_row,
        )
        .optional()
        .map_err(sql_err)
        .map(|row| row.map(|row| row.into_stored(false)))
    }
}

fn to_status(stored: StoredOperation) -> OperationStatusResult {
    OperationStatusResult {
        operation_id: stored.operation_id,
        status: stored.status,
        replayed: false,
        kind: OperationKind::Patch,
        workspace_id: stored.workspace_id,
        created_at: stored.created_at,
        finished_at: stored.finished_at,
        files: stored.result.files,
        changes: stored.result.changes,
        events: stored.events,
    }
}

struct Row {
    operation_id: String,
    fingerprint: String,
    result_json: String,
    workspace_id: String,
    created_at: i64,
    finished_at: Option<i64>,
    events_json: Option<String>,
}

impl Row {
    fn events(&self) -> Vec<OperationEvent> {
        self.events_json
            .as_deref()
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default()
    }

    fn into_stored(self, replayed: bool) -> StoredOperation {
        let mut result: ApplyPatchResult = serde_json::from_str(&self.result_json).unwrap_or(
            ApplyPatchResult::new(PatchStatus::Unknown, OperationId(self.operation_id.clone())),
        );
        result.replayed = replayed;
        let events = self.events();
        StoredOperation {
            operation_id: OperationId(self.operation_id),
            status: result.status,
            replayed,
            result,
            workspace_id: WorkspaceId(self.workspace_id),
            created_at: self.created_at,
            finished_at: self.finished_at,
            events,
        }
    }
}

const OPERATION_SELECT: &str = "SELECT operation_id, fingerprint, result_json, workspace_id, created_at, finished_at, events_json FROM operations";

fn map_operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        operation_id: row.get(0)?,
        fingerprint: row.get(1)?,
        result_json: row.get(2)?,
        workspace_id: row.get(3)?,
        created_at: row.get(4)?,
        finished_at: row.get(5)?,
        events_json: row.get(6)?,
    })
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        &format!("{OPERATION_SELECT} WHERE operation_key = ?1"),
        params![key],
        map_operation_row,
    )
    .optional()
    .map_err(sql_err)
}

fn load_by_id(conn: &Connection, id: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        &format!("{OPERATION_SELECT} WHERE operation_id = ?1"),
        params![id],
        map_operation_row,
    )
    .optional()
    .map_err(sql_err)
}

fn migrate_operations(conn: &Connection) -> Result<(), String> {
    let columns = table_columns(conn, "operations")?;
    if !columns.iter().any(|name| name == "finished_at") {
        conn.execute("ALTER TABLE operations ADD COLUMN finished_at INTEGER", [])
            .map_err(|err| err.to_string())?;
    }
    if !columns.iter().any(|name| name == "events_json") {
        conn.execute("ALTER TABLE operations ADD COLUMN events_json TEXT", [])
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn migrate_approvals(conn: &Connection) -> Result<(), String> {
    let columns = table_columns(conn, "approvals")?;
    if !columns.iter().any(|name| name == "fingerprint") {
        conn.execute(
            "ALTER TABLE approvals ADD COLUMN fingerprint TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|err| err.to_string())?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_approvals_active_fp
         ON approvals(workspace_id, fingerprint)
         WHERE state IN ('pending','granted','resuming') AND length(fingerprint) > 0;",
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|err| err.to_string())?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    Ok(columns)
}

fn hash_canonical(value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.to_string().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn status_str(status: PatchStatus) -> &'static str {
    match status {
        PatchStatus::Applied => "applied",
        PatchStatus::Checked => "checked",
        PatchStatus::Rejected => "rejected",
        PatchStatus::FailedRolledBack => "failed_rolled_back",
        PatchStatus::FailedPartial => "failed_partial",
        PatchStatus::Unknown => "unknown",
    }
}

fn sql_err(err: rusqlite::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::OperationNotFound, err.to_string())
}

fn ser_err(err: serde_json::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidPatch, err.to_string())
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{
        FileChange, FileChangeKind, OperationEventName, OperationKind, WorkspaceId,
    };
    use std::collections::BTreeMap;

    fn params(patch: &str, key: &str) -> ApplyPatchParams {
        ApplyPatchParams {
            workspace_id: WorkspaceId("demo".into()),
            patch: patch.into(),
            expected_versions: BTreeMap::new(),
            operation_key: Some(OperationKey(key.into())),
            check_only: false,
            work_id: None,
        }
    }

    #[test]
    fn replay_same_key_and_fingerprint() {
        let store = Store::memory().unwrap();
        let first = params("patch-a", "k1");
        let fp = Store::fingerprint(&first);
        let Begin::Fresh(id) = store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap()
        else {
            panic!("expected fresh");
        };
        let done = ApplyPatchResult::new(PatchStatus::Rejected, id.clone());
        store.finish(&id, &done).unwrap();
        let Begin::Replayed(replay) = store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap()
        else {
            panic!("expected replay");
        };
        assert!(replay.replayed);
        assert_eq!(replay.operation_id, id);
        assert_eq!(replay.status, PatchStatus::Rejected);
    }

    #[test]
    fn different_body_same_key_conflicts() {
        let store = Store::memory().unwrap();
        let first = params("patch-a", "k1");
        let fp = Store::fingerprint(&first);
        store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap();
        let second = params("patch-b", "k1");
        let err = store
            .begin(
                second.operation_key.as_ref(),
                "demo",
                &Store::fingerprint(&second),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::OperationKeyConflict);
    }

    #[test]
    fn unknown_operation_id_is_rejected() {
        let store = Store::memory().unwrap();
        let err = store
            .get(&OperationId("op-does-not-exist".into()))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::OperationNotFound);
    }

    #[test]
    fn status_lookup_by_key_and_rejects_both_or_neither() {
        let store = Store::memory().unwrap();
        let first = params("patch-a", "lookup-1");
        let fp = Store::fingerprint(&first);
        let Begin::Fresh(id) = store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap()
        else {
            panic!("expected fresh");
        };
        store
            .finish(
                &id,
                &ApplyPatchResult::new(PatchStatus::Applied, id.clone()),
            )
            .unwrap();
        let by_key = store
            .status_lookup(None, first.operation_key.as_ref())
            .unwrap();
        assert_eq!(by_key.operation_id, id);
        assert_eq!(by_key.status, PatchStatus::Applied);
        assert!(!by_key.replayed);
        assert_eq!(by_key.kind, OperationKind::Patch);
        assert_eq!(by_key.workspace_id.0, "demo");
        assert!(by_key.finished_at.is_some());
        assert_eq!(by_key.events[0].name, OperationEventName::Minted);
        assert_eq!(by_key.events[1].name, OperationEventName::Finished);
        assert_eq!(by_key.events[1].status, Some(PatchStatus::Applied));
        assert_eq!(
            store.status_lookup(None, None).unwrap_err().code,
            ErrorCode::OperationNotFound
        );
        assert_eq!(
            store
                .status_lookup(Some(&id), first.operation_key.as_ref())
                .unwrap_err()
                .code,
            ErrorCode::OperationNotFound
        );
    }

    #[test]
    fn busy_shell_blocks_other_writes() {
        let store = Store::memory().unwrap();
        store.mark_shell_busy("demo", "proc-1").unwrap();
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
        store.clear_shell("demo");
        let _guard = store.try_acquire_write("demo").unwrap();
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
    }

    #[test]
    fn process_owned_lease_releases_by_process_id() {
        let store = Store::memory().unwrap();
        store.mark_shell_busy("demo", "proc-1").unwrap();
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
        store.release_process("proc-other");
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
        store.release_process("proc-1");
        let _guard = store.try_acquire_write("demo").unwrap();
    }

    #[test]
    fn request_owned_write_is_not_cleared_by_release_process() {
        let store = Store::memory().unwrap();
        let _guard = store.try_acquire_write("demo").unwrap();
        store.release_process("proc-1");
        assert_eq!(
            store.try_acquire_write("demo").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
    }

    #[test]
    fn release_all_processes_keeps_request_owned_writes() {
        let store = Store::memory().unwrap();
        store.mark_shell_busy("demo", "proc-1").unwrap();
        store.mark_shell_busy("other", "proc-2").unwrap();
        let _guard = store.try_acquire_write("held").unwrap();
        store.release_all_processes();
        let _demo = store.try_acquire_write("demo").unwrap();
        let _other = store.try_acquire_write("other").unwrap();
        assert_eq!(
            store.try_acquire_write("held").err().map(|e| e.code),
            Some(ErrorCode::WorkspaceBusy)
        );
    }

    #[test]
    fn minted_operation_id_is_not_a_jsonrpc_id() {
        let store = Store::memory().unwrap();
        let first = params("p", "k");
        let Begin::Fresh(id) = store
            .begin(
                first.operation_key.as_ref(),
                "demo",
                &Store::fingerprint(&first),
            )
            .unwrap()
        else {
            panic!("fresh");
        };
        assert!(id.0.starts_with("op-"));
        assert_ne!(id.0, "1");
    }

    #[test]
    fn unfinished_operation_replays_as_unknown_without_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.sqlite");
        let first = params("p", "k-unfinished");
        let fp = Store::fingerprint(&first);
        let original;
        {
            let store = Store::open(&path).unwrap();
            let Begin::Fresh(id) = store
                .begin(first.operation_key.as_ref(), "demo", &fp)
                .unwrap()
            else {
                panic!("fresh");
            };
            original = id;
        }
        let store = Store::open(&path).unwrap();
        let Begin::Replayed(replay) = store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap()
        else {
            panic!("replay unfinished");
        };
        assert!(replay.replayed);
        assert_eq!(replay.operation_id, original);
        assert_eq!(replay.status, PatchStatus::Unknown);
        let status = store.status(&original).unwrap();
        assert_eq!(status.kind, OperationKind::Patch);
        assert!(status.finished_at.is_none());
        assert_eq!(status.events.len(), 1);
        assert_eq!(status.events[0].name, OperationEventName::Minted);
        assert!(status.files.is_empty());
        assert!(status.changes.is_empty());
    }

    #[test]
    fn status_lookup_exposes_files_changes_and_finish_reason() {
        let store = Store::memory().unwrap();
        let first = params("patch-a", "ledger-1");
        let fp = Store::fingerprint(&first);
        let Begin::Fresh(id) = store
            .begin(first.operation_key.as_ref(), "demo", &fp)
            .unwrap()
        else {
            panic!("expected fresh");
        };
        let mut done = ApplyPatchResult::new(PatchStatus::Applied, id.clone());
        done.files = vec!["a.txt".into()];
        done.changes = vec![FileChange {
            path: "a.txt".into(),
            before_version: Some("absent".into()),
            after_version: Some("hash-a".into()),
            kind: FileChangeKind::Add,
        }];
        store.finish(&id, &done).unwrap();
        let status = store
            .status_lookup(None, first.operation_key.as_ref())
            .unwrap();
        assert_eq!(status.files, vec!["a.txt"]);
        assert_eq!(status.changes[0].after_version.as_deref(), Some("hash-a"));
        assert_eq!(status.events[1].status, Some(PatchStatus::Applied));
        assert_eq!(status.events[1].reason, None);

        let Begin::Fresh(unknown_id) = store.begin(None, "demo", "fp-unknown").unwrap() else {
            panic!("fresh unknown");
        };
        store
            .finish(
                &unknown_id,
                &ApplyPatchResult::new(PatchStatus::Unknown, unknown_id.clone()),
            )
            .unwrap();
        let unknown = store.status(&unknown_id).unwrap();
        assert_eq!(unknown.status, PatchStatus::Unknown);
        assert!(unknown.changes.is_empty());
        assert_eq!(unknown.events[1].reason.as_deref(), Some("unknown"));
        assert!(unknown.finished_at.is_some());
    }

    #[test]
    fn legacy_operations_table_gains_ledger_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE operations (
                    operation_id TEXT PRIMARY KEY,
                    operation_key TEXT,
                    workspace_id TEXT NOT NULL,
                    fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL,
                    result_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let first = params("legacy", "k-legacy");
        let Begin::Fresh(id) = store
            .begin(
                first.operation_key.as_ref(),
                "demo",
                &Store::fingerprint(&first),
            )
            .unwrap()
        else {
            panic!("fresh");
        };
        store
            .finish(
                &id,
                &ApplyPatchResult::new(PatchStatus::Checked, id.clone()),
            )
            .unwrap();
        let status = store.status(&id).unwrap();
        assert_eq!(status.status, PatchStatus::Checked);
        assert_eq!(status.events[0].name, OperationEventName::Minted);
        assert_eq!(status.events[1].name, OperationEventName::Finished);
    }
}
