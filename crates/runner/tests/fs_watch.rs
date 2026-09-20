//! Internal filesystem observation: events are hints, not apply guards.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ErrorCode, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    FsWatchEvent, FsWatchKind, InProcessRunner, Runner, RunnerApplyPatchRequest,
};
use tempfile::tempdir;
use tokio::sync::broadcast;

fn workspace(root: &Path) -> Workspace {
    Workspace::new(
        WorkspaceId("demo".into()),
        root.to_path_buf(),
        Profile::WorkspaceWrite,
    )
}

fn runner() -> InProcessRunner {
    InProcessRunner::new(Arc::new(|_| {}))
}

async fn recv_until(
    rx: &mut broadcast::Receiver<FsWatchEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&FsWatchEvent) -> bool,
) -> Option<FsWatchEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(event)) => {
                if pred(&event) {
                    return Some(event);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => return None,
            Err(_) => return None,
        }
    }
}

async fn drain_for(
    rx: &mut broadcast::Receiver<FsWatchEvent>,
    timeout: Duration,
) -> Vec<FsWatchEvent> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(event)) => out.push(event),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        }
    }
    out
}

async fn prime_watch(
    runner: &InProcessRunner,
    ws: &Workspace,
) -> broadcast::Receiver<FsWatchEvent> {
    let mut rx = runner.subscribe_watch(ws).expect("subscribe");
    let probe = ws.root.join(".watch-prime");
    std::fs::write(&probe, "prime\n").unwrap();
    let seen = recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches(".watch-prime")
    })
    .await;
    assert!(seen.is_some(), "watcher did not observe the prime file");
    let _ = std::fs::remove_file(&probe);
    let _ = recv_until(&mut rx, Duration::from_millis(500), |event| {
        event.kind.touches(".watch-prime")
    })
    .await;
    rx
}

fn mentions(events: &[FsWatchEvent], path: &str) -> bool {
    events.iter().any(|event| event.kind.touches(path))
}

#[tokio::test]
async fn external_modify_is_observed() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::fs::write(dir.path().join("keep.txt"), "edited\n").unwrap();
    let event = recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("keep.txt")
    })
    .await
    .expect("modify event");
    assert_eq!(event.epoch, 1);
}

#[tokio::test]
async fn create_remove_and_rename_dirty_source_and_dest() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::fs::write(dir.path().join("created.txt"), "new\n").unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("created.txt")
    })
    .await
    .expect("create");

    std::fs::rename(
        dir.path().join("created.txt"),
        dir.path().join("renamed.txt"),
    )
    .unwrap();
    let mut saw_from = false;
    let mut saw_to = false;
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        for path in event.kind.paths() {
            if path == "created.txt" {
                saw_from = true;
            }
            if path == "renamed.txt" {
                saw_to = true;
            }
        }
        if let FsWatchKind::Rename { from, to } = &event.kind {
            if from == "created.txt" && to == "renamed.txt" {
                saw_from = true;
                saw_to = true;
            }
        }
        saw_from && saw_to
    })
    .await
    .expect("rename dirty source and dest");

    std::fs::remove_file(dir.path().join("renamed.txt")).unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("renamed.txt")
    })
    .await
    .expect("remove");
}

#[tokio::test]
async fn coalesced_writes_still_read_current_bytes() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "0").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    for i in 1..=20 {
        std::fs::write(dir.path().join("keep.txt"), i.to_string()).unwrap();
    }
    let _ = drain_for(&mut rx, Duration::from_millis(400)).await;
    let read = runner.read(&ws, "keep.txt").await.expect("read");
    assert_eq!(read.content, "20");
}

#[tokio::test]
async fn forced_resync_stays_on_the_same_epoch() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;
    runner.force_watch_resync(&ws).unwrap();
    let event = recv_until(&mut rx, Duration::from_secs(2), |event| {
        event.kind == FsWatchKind::ResyncRequired
    })
    .await
    .expect("resync");
    assert_eq!(event.epoch, 1);
}

#[tokio::test]
async fn watcher_restart_bumps_epoch_and_invalidates() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;
    let epoch = runner.restart_watch(&ws).unwrap();
    assert!(epoch > 1, "epoch={epoch}");
    let event = recv_until(&mut rx, Duration::from_secs(2), |event| {
        event.kind == FsWatchKind::ResyncRequired && event.epoch == epoch
    })
    .await
    .expect("restart resync");
    assert_eq!(event.epoch, epoch);
}

#[tokio::test]
async fn outside_and_symlink_events_are_not_emitted() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let outside_dir = tempdir().unwrap();
    let secret = outside_dir.path().join("secret.txt");
    std::fs::write(&secret, "secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, dir.path().join("link")).unwrap();

    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::fs::write(&secret, "changed\n").unwrap();
    std::fs::write(outside_dir.path().join("sibling.txt"), "nope\n").unwrap();
    std::fs::write(dir.path().join("link"), "via-link\n").unwrap();

    let events = drain_for(&mut rx, Duration::from_millis(600)).await;
    assert!(
        !mentions(&events, "link"),
        "symlink path must not be emitted: {events:?}"
    );
    assert!(
        !events.iter().any(|event| {
            event
                .kind
                .paths()
                .iter()
                .any(|path| path.contains("secret") || path.contains("sibling"))
        }),
        "outside paths must not be emitted: {events:?}"
    );
}

#[tokio::test]
async fn discarded_watch_events_do_not_bypass_version_conflict() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    let version = runner.version(&ws, "keep.txt").await.unwrap();
    std::fs::write(dir.path().join("keep.txt"), "externally-edited\n").unwrap();
    let _ = drain_for(&mut rx, Duration::from_millis(400)).await;

    let err = runner
        .apply_patch(
            &ws,
            RunnerApplyPatchRequest {
                patch:
                    "*** Begin Patch\n*** Update File: keep.txt\n@@\n-keep\n+new\n*** End Patch\n"
                        .into(),
                expected_versions: BTreeMap::from([("keep.txt".into(), version)]),
                check_only: false,
            },
        )
        .await
        .expect_err("version conflict");
    match err {
        codespace_runner::RunnerError::Execution(body) => {
            assert_eq!(body.code, ErrorCode::VersionConflict);
        }
        other => panic!("expected VERSION_CONFLICT, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "externally-edited\n"
    );
}
