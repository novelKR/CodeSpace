//! Internal workspace filesystem observation. Events are invalidation
//! hints: missing, coalesced, or restarted watches must never weaken
//! `expected_versions` / `VERSION_CONFLICT`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codespace_domain::{ErrorBody, ErrorCode};
use codespace_policy::Workspace;
use notify::event::{ModifyKind, RenameMode};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::process::InProcessRunner;
use crate::PathSandbox;

const WATCH_LAG: usize = 512;

/// One observation delivered by a live watcher. `seq` is that watcher's
/// delivery order, not a filesystem causal revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsWatchEvent {
    pub epoch: u64,
    pub seq: u64,
    pub kind: FsWatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsWatchKind {
    Create { path: String },
    Modify { path: String },
    Remove { path: String },
    Rename { from: String, to: String },
    ResyncRequired,
}

impl FsWatchKind {
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Self::Create { path } | Self::Modify { path } | Self::Remove { path } => {
                vec![path.as_str()]
            }
            Self::Rename { from, to } => vec![from.as_str(), to.as_str()],
            Self::ResyncRequired => Vec::new(),
        }
    }

    pub fn touches(&self, path: &str) -> bool {
        self.paths().contains(&path)
    }
}

struct WatchShared {
    root: PathBuf,
    epoch: AtomicU64,
    seq: AtomicU64,
    tx: broadcast::Sender<FsWatchEvent>,
}

/// Recursive watcher for one workspace root.
pub struct WorkspaceWatch {
    shared: Arc<WatchShared>,
    watcher: Mutex<Option<RecommendedWatcher>>,
}

#[derive(Clone, Default)]
pub(crate) struct WatchSet {
    inner: Arc<Mutex<HashMap<PathBuf, Arc<WorkspaceWatch>>>>,
}

impl WatchSet {
    fn watch_for(&self, root: &Path) -> Result<Arc<WorkspaceWatch>, ErrorBody> {
        let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let mut map = self.inner.lock().expect("watch set");
        if let Some(existing) = map.get(&key) {
            return Ok(existing.clone());
        }
        let watch = WorkspaceWatch::start(key.clone())?;
        map.insert(key, watch.clone());
        Ok(watch)
    }

    fn touch(&self, root: &Path) {
        let _ = self.watch_for(root);
    }
}

impl WorkspaceWatch {
    pub fn start(root: PathBuf) -> Result<Arc<Self>, ErrorBody> {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let (tx, _) = broadcast::channel(WATCH_LAG);
        let shared = Arc::new(WatchShared {
            root: root.clone(),
            epoch: AtomicU64::new(1),
            seq: AtomicU64::new(0),
            tx,
        });
        let watcher = spawn_watcher(shared.clone())?;
        Ok(Arc::new(Self {
            shared,
            watcher: Mutex::new(Some(watcher)),
        }))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<FsWatchEvent> {
        self.shared.tx.subscribe()
    }

    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }

    pub fn force_resync(&self) {
        emit_resync(&self.shared);
    }

    pub fn restart(&self) -> Result<u64, ErrorBody> {
        let mut slot = self.watcher.lock().expect("watcher");
        *slot = None;
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        self.shared.seq.store(0, Ordering::SeqCst);
        *slot = Some(spawn_watcher(self.shared.clone())?);
        drop(slot);
        emit_resync(&self.shared);
        Ok(self.epoch())
    }
}

impl Drop for WorkspaceWatch {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.watcher.lock() {
            *slot = None;
        }
    }
}

impl InProcessRunner {
    /// Best-effort start. Observation must never fail a file operation.
    pub(crate) fn touch_watch(&self, ws: &Workspace) {
        self.watches.touch(&ws.root);
    }

    pub fn subscribe_watch(
        &self,
        ws: &Workspace,
    ) -> Result<broadcast::Receiver<FsWatchEvent>, ErrorBody> {
        Ok(self.watches.watch_for(&ws.root)?.subscribe())
    }

    pub fn restart_watch(&self, ws: &Workspace) -> Result<u64, ErrorBody> {
        self.watches.watch_for(&ws.root)?.restart()
    }

    pub fn force_watch_resync(&self, ws: &Workspace) -> Result<(), ErrorBody> {
        self.watches.watch_for(&ws.root)?.force_resync();
        Ok(())
    }
}

fn spawn_watcher(shared: Arc<WatchShared>) -> Result<RecommendedWatcher, ErrorBody> {
    let callback_shared = shared.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => handle_event(&callback_shared, event),
            Err(_) => emit_resync(&callback_shared),
        },
        Config::default().with_follow_symlinks(false),
    )
    .map_err(watch_err)?;
    watcher
        .watch(&shared.root, RecursiveMode::Recursive)
        .map_err(watch_err)?;
    Ok(watcher)
}

fn handle_event(shared: &WatchShared, event: Event) {
    if event.need_rescan() {
        emit_resync(shared);
        return;
    }
    match event.kind {
        EventKind::Access(_) => {}
        EventKind::Other => emit_resync(shared),
        EventKind::Any => {
            let rels = relative_paths(shared, &event.paths);
            if rels.is_empty() {
                if event.paths.is_empty() {
                    emit_resync(shared);
                }
                return;
            }
            for path in rels {
                emit(shared, FsWatchKind::Modify { path });
            }
        }
        EventKind::Create(_) => {
            for path in relative_paths(shared, &event.paths) {
                emit(shared, FsWatchKind::Create { path });
            }
        }
        EventKind::Remove(_) => {
            for path in relative_paths(shared, &event.paths) {
                emit(shared, FsWatchKind::Remove { path });
            }
        }
        EventKind::Modify(ModifyKind::Name(mode)) => handle_rename(shared, mode, &event.paths),
        EventKind::Modify(_) => {
            for path in relative_paths(shared, &event.paths) {
                emit(shared, FsWatchKind::Modify { path });
            }
        }
    }
}

fn handle_rename(shared: &WatchShared, mode: RenameMode, paths: &[PathBuf]) {
    let rels = relative_paths(shared, paths);
    match mode {
        RenameMode::Both if rels.len() >= 2 => {
            emit(
                shared,
                FsWatchKind::Rename {
                    from: rels[0].clone(),
                    to: rels[1].clone(),
                },
            );
        }
        RenameMode::From => {
            for path in rels {
                emit(shared, FsWatchKind::Remove { path });
            }
        }
        RenameMode::To => {
            for path in rels {
                emit(shared, FsWatchKind::Create { path });
            }
        }
        _ => match rels.len() {
            0 => {}
            1 => emit(
                shared,
                FsWatchKind::Modify {
                    path: rels[0].clone(),
                },
            ),
            _ => emit(
                shared,
                FsWatchKind::Rename {
                    from: rels[0].clone(),
                    to: rels[1].clone(),
                },
            ),
        },
    }
}

fn relative_paths(shared: &WatchShared, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| PathSandbox::watch_relative(&shared.root, path))
        .collect()
}

fn emit(shared: &WatchShared, kind: FsWatchKind) {
    let epoch = shared.epoch.load(Ordering::SeqCst);
    let seq = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = shared.tx.send(FsWatchEvent { epoch, seq, kind });
}

fn emit_resync(shared: &WatchShared) {
    emit(shared, FsWatchKind::ResyncRequired);
}

fn watch_err(err: notify::Error) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::FileOperationFailed,
        format!("workspace watch: {err}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn watch_relative_keeps_workspace_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
        let rel = PathSandbox::watch_relative(dir.path(), &dir.path().join("keep.txt"));
        assert_eq!(rel.as_deref(), Some("keep.txt"));
    }

    #[test]
    fn watch_relative_drops_escape_symlink_and_outside() {
        let dir = tempdir().unwrap();
        let outside = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "nope\n").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("link")).unwrap();

        assert_eq!(
            PathSandbox::watch_relative(dir.path(), &dir.path().join("..").join("outside.txt")),
            None
        );
        assert_eq!(PathSandbox::watch_relative(dir.path(), &outside), None);
        assert_eq!(
            PathSandbox::watch_relative(dir.path(), &dir.path().join("link")),
            None
        );
    }
}
