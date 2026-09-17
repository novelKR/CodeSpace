//! Single-instance SQLite operations and in-process workspace write locks.
//! HTTP/JSON-RPC request ids are never stored as [`OperationId`] values.

mod resource;

use std::path::Path;
use std::sync::Mutex;

use codespace_domain::{
    ApplyPatchParams, ApplyPatchResult, ErrorBody, ErrorCode, OperationId, OperationKey,
    OperationStatusResult, PatchStatus,
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub use resource::{LockMode, Resource, ResourceGuard};

#[derive(Debug, Clone)]
pub struct StoredOperation {
    pub operation_id: OperationId,
    pub status: PatchStatus,
    pub replayed: bool,
    pub result: ApplyPatchResult,
}

#[derive(Debug)]
pub enum Begin {
    Fresh(OperationId),
    Replayed(StoredOperation),
}

pub struct Store {
    conn: Mutex<Connection>,
    locks: Mutex<resource::ResourceSerializer>,
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
                created_at INTEGER NOT NULL
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
        Ok(Self {
            conn: Mutex::new(conn),
            locks: Mutex::new(resource::ResourceSerializer::default()),
        })
    }

    pub fn fingerprint(params: &ApplyPatchParams) -> String {
        let value = serde_json::json!({
            "workspace_id": params.workspace_id.0,
            "patch": params.patch,
            "expected_versions": params.expected_versions,
            "check_only": params.check_only,
        });
        let mut hasher = Sha256::new();
        hasher.update(value.to_string().as_bytes());
        format!("sha256:{}", hex::encode(hasher.finalize()))
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

    fn release_resource(&self, resource: &Resource, mode: LockMode) {
        self.locks
            .lock()
            .expect("lock mutex")
            .unlock(resource, mode);
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
                return Ok(Begin::Replayed(row.into_stored(true)));
            }
        }

        let operation_id = OperationId(format!("op-{}", uuid::Uuid::new_v4()));
        let pending = ApplyPatchResult::new(PatchStatus::Unknown, operation_id.clone());
        let result_json = serde_json::to_string(&pending).map_err(ser_err)?;
        let now = now_secs();
        conn.execute(
            "INSERT INTO operations
                (operation_id, operation_key, workspace_id, fingerprint, status, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                operation_id.0,
                operation_key.map(|k| k.0.as_str()),
                workspace_id,
                fingerprint,
                status_str(PatchStatus::Unknown),
                result_json,
                now,
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
        let json = serde_json::to_string(result).map_err(ser_err)?;
        let n = conn
            .execute(
                "UPDATE operations SET status = ?1, result_json = ?2 WHERE operation_id = ?3",
                params![status_str(result.status), json, operation_id.0],
            )
            .map_err(sql_err)?;
        if n == 0 {
            return Err(ErrorBody::new(
                ErrorCode::OperationNotFound,
                "unknown operation_id",
            ));
        }
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
}

fn to_status(stored: StoredOperation) -> OperationStatusResult {
    OperationStatusResult {
        operation_id: stored.operation_id,
        status: stored.status,
        replayed: false,
    }
}

struct Row {
    operation_id: String,
    fingerprint: String,
    result_json: String,
}

impl Row {
    fn into_stored(self, replayed: bool) -> StoredOperation {
        let mut result: ApplyPatchResult = serde_json::from_str(&self.result_json).unwrap_or(
            ApplyPatchResult::new(PatchStatus::Unknown, OperationId(self.operation_id.clone())),
        );
        result.replayed = replayed;
        StoredOperation {
            operation_id: OperationId(self.operation_id),
            status: result.status,
            replayed,
            result,
        }
    }
}

fn load_by_key(conn: &Connection, key: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        "SELECT operation_id, fingerprint, result_json FROM operations WHERE operation_key = ?1",
        params![key],
        |row| {
            Ok(Row {
                operation_id: row.get(0)?,
                fingerprint: row.get(1)?,
                result_json: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sql_err)
}

fn load_by_id(conn: &Connection, id: &str) -> Result<Option<Row>, ErrorBody> {
    conn.query_row(
        "SELECT operation_id, fingerprint, result_json FROM operations WHERE operation_id = ?1",
        params![id],
        |row| {
            Ok(Row {
                operation_id: row.get(0)?,
                fingerprint: row.get(1)?,
                result_json: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sql_err)
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

mod coord;

pub use coord::CreateIntent;

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::WorkspaceId;
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
    }
}
