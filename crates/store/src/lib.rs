//! SQLite operation store and in-process workspace write locks.
//! HTTP request ids are never stored as operations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use codespace_domain::{ApplyPatchParams, ApplyPatchResult, ErrorBody, ErrorCode, OperationId};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOperation {
    pub operation_id: String,
    pub workspace_id: String,
    pub operation_key: Option<String>,
    pub request_hash: String,
    pub result: ApplyPatchResult,
}

pub fn fingerprint(params: &ApplyPatchParams) -> String {
    let body = serde_json::json!({
        "workspace_id": params.workspace_id.0,
        "patch": params.patch,
        "expected_versions": params.expected_versions,
        "check_only": params.check_only,
    });
    let bytes = serde_json::to_vec(&body).expect("fingerprint json");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn new_operation_id() -> OperationId {
    OperationId(format!("op-{}", uuid::Uuid::new_v4()))
}

pub struct OperationStore {
    conn: Mutex<Connection>,
}

impl OperationStore {
    pub fn memory() -> Arc<Self> {
        Arc::new(Self {
            conn: Mutex::new(open_conn(None).expect("in-memory sqlite")),
        })
    }

    pub fn open(path: &Path) -> Result<Arc<Self>, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        Ok(Arc::new(Self {
            conn: Mutex::new(open_conn(Some(path))?),
        }))
    }

    pub fn get(&self, operation_id: &str) -> Result<Option<StoredOperation>, ErrorBody> {
        let conn = self.conn.lock().map_err(|_| store_err("lock"))?;
        conn.query_row(
            "SELECT operation_id, workspace_id, operation_key, request_hash, result_json
             FROM operations WHERE operation_id = ?1",
            params![operation_id],
            row_to_stored,
        )
        .optional()
        .map_err(|e| store_err(e.to_string()))
    }

    pub fn get_by_key(
        &self,
        workspace_id: &str,
        operation_key: &str,
    ) -> Result<Option<StoredOperation>, ErrorBody> {
        let conn = self.conn.lock().map_err(|_| store_err("lock"))?;
        conn.query_row(
            "SELECT operation_id, workspace_id, operation_key, request_hash, result_json
             FROM operations WHERE workspace_id = ?1 AND operation_key = ?2",
            params![workspace_id, operation_key],
            row_to_stored,
        )
        .optional()
        .map_err(|e| store_err(e.to_string()))
    }

    pub fn insert(&self, stored: &StoredOperation) -> Result<(), ErrorBody> {
        let conn = self.conn.lock().map_err(|_| store_err("lock"))?;
        let json = serde_json::to_string(&stored.result).map_err(|e| store_err(e.to_string()))?;
        conn.execute(
            "INSERT INTO operations (operation_id, workspace_id, operation_key, request_hash, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                stored.operation_id,
                stored.workspace_id,
                stored.operation_key,
                stored.request_hash,
                json
            ],
        )
        .map_err(|e| store_err(e.to_string()))?;
        Ok(())
    }
}

fn open_conn(path: Option<&Path>) -> Result<Connection, String> {
    let conn = match path {
        Some(path) => Connection::open(path).map_err(|e| e.to_string())?,
        None => Connection::open_in_memory().map_err(|e| e.to_string())?,
    };
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS operations (
            operation_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            operation_key TEXT,
            request_hash TEXT NOT NULL,
            result_json TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS operations_workspace_key
            ON operations(workspace_id, operation_key)
            WHERE operation_key IS NOT NULL;",
    )
    .map_err(|e| e.to_string())?;
    Ok(conn)
}

fn row_to_stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredOperation> {
    let result_json: String = row.get(4)?;
    let result: ApplyPatchResult = serde_json::from_str(&result_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok(StoredOperation {
        operation_id: row.get(0)?,
        workspace_id: row.get(1)?,
        operation_key: row.get(2)?,
        request_hash: row.get(3)?,
        result,
    })
}

fn store_err(detail: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::OperationNotFound, detail)
}

/// In-process write occupancy. A live shell (W10) or an in-flight patch holds
/// the workspace; other mutating work gets `WORKSPACE_BUSY`.
#[derive(Debug, Default)]
pub struct WorkspaceLocks {
    inner: Mutex<HashMap<String, Occupant>>,
}

#[derive(Debug, Clone)]
enum Occupant {
    Patch,
    #[allow(dead_code)]
    Shell(String),
}

#[derive(Debug)]
pub struct WriteGuard {
    locks: Arc<WorkspaceLocks>,
    workspace_id: String,
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.locks.inner.lock() {
            map.remove(&self.workspace_id);
        }
    }
}

impl WorkspaceLocks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn try_write(self: &Arc<Self>, workspace_id: &str) -> Result<WriteGuard, ErrorBody> {
        let mut map = self.inner.lock().map_err(|_| busy("lock poisoned"))?;
        if map.contains_key(workspace_id) {
            return Err(busy("workspace write lock is held"));
        }
        map.insert(workspace_id.to_string(), Occupant::Patch);
        Ok(WriteGuard {
            locks: Arc::clone(self),
            workspace_id: workspace_id.to_string(),
        })
    }

    pub fn hold_shell(
        self: &Arc<Self>,
        workspace_id: &str,
        process_id: &str,
    ) -> Result<(), ErrorBody> {
        let mut map = self.inner.lock().map_err(|_| busy("lock poisoned"))?;
        if map.contains_key(workspace_id) {
            return Err(busy("workspace write lock is held"));
        }
        map.insert(
            workspace_id.to_string(),
            Occupant::Shell(process_id.to_string()),
        );
        Ok(())
    }

    pub fn release(&self, workspace_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(workspace_id);
        }
    }

    pub fn is_busy(&self, workspace_id: &str) -> bool {
        self.inner
            .lock()
            .map(|map| map.contains_key(workspace_id))
            .unwrap_or(true)
    }
}

fn busy(detail: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::WorkspaceBusy, detail)
}

/// Record or replay a mutating request. The `execute` closure runs only when
/// this is not a replay.
pub fn begin_or_replay(
    store: &OperationStore,
    locks: &Arc<WorkspaceLocks>,
    params: &ApplyPatchParams,
    execute: impl FnOnce(OperationId) -> Result<ApplyPatchResult, ErrorBody>,
) -> Result<ApplyPatchResult, ErrorBody> {
    let _guard = locks.try_write(&params.workspace_id.0)?;
    let hash = fingerprint(params);
    if let Some(key) = params.operation_key.as_ref() {
        if let Some(existing) = store.get_by_key(&params.workspace_id.0, &key.0)? {
            if existing.request_hash == hash {
                let mut result = existing.result;
                result.replayed = true;
                return Ok(result);
            }
            return Err(ErrorBody::new(
                ErrorCode::OperationKeyConflict,
                "operation_key reused with a different request",
            ));
        }
    }

    let operation_id = new_operation_id();
    let mut result = execute(operation_id.clone())?;
    result.operation_id = operation_id.clone();
    result.replayed = false;
    store.insert(&StoredOperation {
        operation_id: operation_id.0,
        workspace_id: params.workspace_id.0.clone(),
        operation_key: params.operation_key.as_ref().map(|k| k.0.clone()),
        request_hash: hash,
        result: result.clone(),
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{OperationKey, PatchStatus, WorkspaceId};
    use std::collections::BTreeMap;

    fn params(patch: &str, key: &str) -> ApplyPatchParams {
        ApplyPatchParams {
            workspace_id: WorkspaceId("demo".into()),
            patch: patch.into(),
            expected_versions: BTreeMap::new(),
            operation_key: Some(OperationKey(key.into())),
            check_only: false,
        }
    }

    fn run(
        store: &OperationStore,
        locks: &Arc<WorkspaceLocks>,
        params: &ApplyPatchParams,
        runs: &std::sync::atomic::AtomicUsize,
    ) -> Result<ApplyPatchResult, ErrorBody> {
        begin_or_replay(store, locks, params, |operation_id| {
            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ApplyPatchResult {
                status: PatchStatus::Applied,
                operation_id,
                replayed: false,
                files: vec!["a.txt".into()],
            })
        })
    }

    #[test]
    fn same_key_and_request_replays_without_rerunning() {
        let store = OperationStore::memory();
        let locks = WorkspaceLocks::new();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let p = params("patch-a", "k1");
        let first = run(&store, &locks, &p, &runs).unwrap();
        let second = run(&store, &locks, &p, &runs).unwrap();
        assert!(!first.replayed);
        assert!(second.replayed);
        assert_eq!(first.operation_id, second.operation_id);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        let status = store.get(&first.operation_id.0).unwrap().unwrap();
        assert_eq!(status.result.files, vec!["a.txt".to_string()]);
    }

    #[test]
    fn same_key_different_request_conflicts() {
        let store = OperationStore::memory();
        let locks = WorkspaceLocks::new();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        run(&store, &locks, &params("patch-a", "k1"), &runs).unwrap();
        let err = run(&store, &locks, &params("patch-b", "k1"), &runs).unwrap_err();
        assert_eq!(err.code, ErrorCode::OperationKeyConflict);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn unknown_operation_id_is_missing() {
        let store = OperationStore::memory();
        assert!(store.get("op-missing").unwrap().is_none());
    }

    #[test]
    fn write_lock_rejects_second_occupant() {
        let locks = WorkspaceLocks::new();
        let _guard = locks.try_write("demo").unwrap();
        let err = locks.try_write("demo").unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkspaceBusy);
    }

    #[test]
    fn shell_occupant_blocks_patch_until_release() {
        let locks = WorkspaceLocks::new();
        locks.hold_shell("demo", "proc-1").unwrap();
        assert!(locks.is_busy("demo"));
        let err = locks.try_write("demo").unwrap_err();
        assert_eq!(err.code, ErrorCode::WorkspaceBusy);
        locks.release("demo");
        assert!(!locks.is_busy("demo"));
        assert!(locks.try_write("demo").is_ok());
    }

    #[test]
    fn file_store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.sqlite");
        let locks = WorkspaceLocks::new();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let first_id;
        {
            let store = OperationStore::open(&path).unwrap();
            first_id = run(&store, &locks, &params("patch-a", "k1"), &runs)
                .unwrap()
                .operation_id;
        }
        let store = OperationStore::open(&path).unwrap();
        let again = run(&store, &locks, &params("patch-a", "k1"), &runs).unwrap();
        assert!(again.replayed);
        assert_eq!(again.operation_id, first_id);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
