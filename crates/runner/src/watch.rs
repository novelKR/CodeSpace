//! Internal workspace filesystem observation. Events are invalidation
//! hints: missing, coalesced, or restarted watches must never weaken
//! `expected_versions` / `VERSION_CONFLICT`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codespace_domain::{ErrorBody, ErrorCode};
use codespace_policy::Workspace;
use notify::event::{ModifyKind, RenameMode};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::process::InProcessRunner;

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

/// Normalized watch consumer. Lag is a local `ResyncRequired`, not a skip.
pub struct WatchSubscription {
    rx: broadcast::Receiver<FsWatchEvent>,
    epoch: Arc<AtomicU64>,
}

/// The watcher is gone; the subscriber cannot recover from this stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchClosed;

impl WatchSubscription {
    pub async fn recv(&mut self) -> Result<FsWatchEvent, WatchClosed> {
        match self.rx.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(_)) => Ok(FsWatchEvent {
                epoch: self.epoch.load(Ordering::SeqCst),
                seq: 0,
                kind: FsWatchKind::ResyncRequired,
            }),
            Err(broadcast::error::RecvError::Closed) => Err(WatchClosed),
        }
    }
}

struct WatchShared {
    root: PathBuf,
    roots: Vec<PathBuf>,
    epoch: Arc<AtomicU64>,
    seq: AtomicU64,
    tx: broadcast::Sender<FsWatchEvent>,
    fail_next_spawn: AtomicBool,
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
        let watch = WorkspaceWatch::start(root.to_path_buf())?;
        map.insert(key, watch.clone());
        Ok(watch)
    }

    fn touch(&self, root: &Path) {
        let _ = self.watch_for(root);
    }
}

impl WorkspaceWatch {
    pub fn start(root: PathBuf) -> Result<Arc<Self>, ErrorBody> {
        let roots = watch_root_aliases(&root);
        let watched = roots[0].clone();
        let (tx, _) = broadcast::channel(WATCH_LAG);
        let shared = Arc::new(WatchShared {
            root: watched,
            roots,
            epoch: Arc::new(AtomicU64::new(1)),
            seq: AtomicU64::new(0),
            tx,
            fail_next_spawn: AtomicBool::new(false),
        });
        let watcher = spawn_watcher(shared.clone())?;
        Ok(Arc::new(Self {
            shared,
            watcher: Mutex::new(Some(watcher)),
        }))
    }

    pub fn subscribe(&self) -> WatchSubscription {
        WatchSubscription {
            rx: self.shared.tx.subscribe(),
            epoch: Arc::clone(&self.shared.epoch),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }

    pub fn force_resync(&self) {
        emit_resync(&self.shared);
    }

    pub fn restart(&self) -> Result<u64, ErrorBody> {
        match spawn_watcher(self.shared.clone()) {
            Ok(new) => {
                let mut slot = self.watcher.lock().expect("watcher");
                *slot = Some(new);
                self.shared.epoch.fetch_add(1, Ordering::SeqCst);
                self.shared.seq.store(0, Ordering::SeqCst);
                drop(slot);
                emit_resync(&self.shared);
                Ok(self.epoch())
            }
            Err(err) => {
                emit_resync(&self.shared);
                Err(err)
            }
        }
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

    pub fn subscribe_watch(&self, ws: &Workspace) -> Result<WatchSubscription, ErrorBody> {
        Ok(self.watches.watch_for(&ws.root)?.subscribe())
    }

    pub fn restart_watch(&self, ws: &Workspace) -> Result<u64, ErrorBody> {
        self.watches.watch_for(&ws.root)?.restart()
    }

    pub fn force_watch_resync(&self, ws: &Workspace) -> Result<(), ErrorBody> {
        self.watches.watch_for(&ws.root)?.force_resync();
        Ok(())
    }

    #[doc(hidden)]
    pub fn fail_next_watch_spawn(&self, ws: &Workspace) -> Result<(), ErrorBody> {
        self.watches
            .watch_for(&ws.root)?
            .shared
            .fail_next_spawn
            .store(true, Ordering::SeqCst);
        Ok(())
    }

    #[doc(hidden)]
    pub fn overflow_watch_for_tests(&self, ws: &Workspace) -> Result<(), ErrorBody> {
        let watch = self.watches.watch_for(&ws.root)?;
        for i in 0..(WATCH_LAG + 8) {
            emit(
                &watch.shared,
                FsWatchKind::Modify {
                    path: format!("overflow-{i}"),
                },
            );
        }
        Ok(())
    }
}

fn spawn_watcher(shared: Arc<WatchShared>) -> Result<RecommendedWatcher, ErrorBody> {
    if shared.fail_next_spawn.swap(false, Ordering::SeqCst) {
        return Err(ErrorBody::new(
            ErrorCode::FileOperationFailed,
            "workspace watch: injected spawn failure",
        ));
    }
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
    match mode {
        RenameMode::Both => match (paths.first(), paths.get(1)) {
            (Some(from), Some(to)) => emit_mapped_rename(shared, from, to),
            (Some(only), None) => emit_inside(shared, only, |path| FsWatchKind::Modify { path }),
            _ => {}
        },
        RenameMode::From => {
            for path in paths {
                emit_inside(shared, path, |p| FsWatchKind::Remove { path: p });
            }
        }
        RenameMode::To => {
            for path in paths {
                emit_inside(shared, path, |p| FsWatchKind::Create { path: p });
            }
        }
        _ if paths.len() >= 2 => emit_mapped_rename(shared, &paths[0], &paths[1]),
        _ => {
            for path in paths {
                emit_inside(shared, path, |p| FsWatchKind::Modify { path: p });
            }
        }
    }
}

fn emit_mapped_rename(shared: &WatchShared, from: &Path, to: &Path) {
    if let Some(kind) = map_rename_sides(
        workspace_relative_event_path(&shared.roots, from),
        workspace_relative_event_path(&shared.roots, to),
    ) {
        emit(shared, kind);
    }
}

fn emit_inside(shared: &WatchShared, path: &Path, kind: impl FnOnce(String) -> FsWatchKind) {
    if let Some(rel) = workspace_relative_event_path(&shared.roots, path) {
        emit(shared, kind(rel));
    }
}

fn map_rename_sides(from: Option<String>, to: Option<String>) -> Option<FsWatchKind> {
    match (from, to) {
        (Some(from), Some(to)) => Some(FsWatchKind::Rename { from, to }),
        (Some(from), None) => Some(FsWatchKind::Remove { path: from }),
        (None, Some(to)) => Some(FsWatchKind::Create { path: to }),
        (None, None) => None,
    }
}

fn relative_paths(shared: &WatchShared, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| workspace_relative_event_path(&shared.roots, path))
        .collect()
}

/// Lexical workspace names only. File type and access policy are not consulted.
fn workspace_relative_event_path(roots: &[PathBuf], event_path: &Path) -> Option<String> {
    let primary = roots.first()?;
    let abs = if event_path.is_absolute() {
        event_path.to_path_buf()
    } else {
        primary.join(event_path)
    };
    for root in roots {
        if let Some(rel) = strip_under(root, &abs) {
            return Some(rel);
        }
    }
    None
}

fn watch_root_aliases(root: &Path) -> Vec<PathBuf> {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut roots = vec![canon.clone()];
    if root != canon.as_path() {
        roots.push(root.to_path_buf());
    }
    roots
}

fn strip_under(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    if rel.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    }) {
        return None;
    }
    let text = rel.to_str()?.replace('\\', "/");
    if text.starts_with('/') || text.split('/').any(|part| part == "..") {
        return None;
    }
    Some(text)
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

    fn roots_for(dir: &Path) -> Vec<PathBuf> {
        watch_root_aliases(dir)
    }

    #[test]
    fn namespace_keeps_workspace_names_including_symlinks() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
        let roots = roots_for(dir.path());
        assert_eq!(
            workspace_relative_event_path(&roots, &dir.path().join("keep.txt")).as_deref(),
            Some("keep.txt")
        );
        assert_eq!(
            workspace_relative_event_path(&roots, &dir.path().join("link")).as_deref(),
            Some("link")
        );
    }

    #[test]
    fn namespace_drops_outside_and_escape() {
        let dir = tempdir().unwrap();
        let outside = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "nope\n").unwrap();
        let roots = roots_for(dir.path());
        assert_eq!(
            workspace_relative_event_path(&roots, &dir.path().join("..").join("outside.txt")),
            None
        );
        assert_eq!(workspace_relative_event_path(&roots, &outside), None);
    }

    #[test]
    fn rename_sides_are_classified_independently() {
        assert_eq!(
            map_rename_sides(Some("a.txt".into()), Some("b.txt".into())),
            Some(FsWatchKind::Rename {
                from: "a.txt".into(),
                to: "b.txt".into(),
            })
        );
        assert_eq!(
            map_rename_sides(Some("a.txt".into()), None),
            Some(FsWatchKind::Remove {
                path: "a.txt".into(),
            })
        );
        assert_eq!(
            map_rename_sides(None, Some("b.txt".into())),
            Some(FsWatchKind::Create {
                path: "b.txt".into(),
            })
        );
        assert_eq!(map_rename_sides(None, None), None);
    }
}
