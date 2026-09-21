//! Internal filesystem observation: events are hints, not apply guards.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use codespace_domain::{ErrorCode, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{
    FsWatchEvent, FsWatchKind, InProcessRunner, Runner, RunnerApplyPatchRequest, WatchSubscription,
};
use tempfile::tempdir;

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
    rx: &mut WatchSubscription,
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
            Ok(Err(_)) => return None,
            Err(_) => return None,
        }
    }
}

async fn drain_for(rx: &mut WatchSubscription, timeout: Duration) -> Vec<FsWatchEvent> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Ok(event)) => out.push(event),
            _ => break,
        }
    }
    out
}

async fn prime_watch(runner: &InProcessRunner, ws: &Workspace) -> WatchSubscription {
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
    let read = runner
        .read(&ws, "keep.txt", None, None)
        .await
        .expect("read");
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
async fn subscriber_lag_becomes_resync_required() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;
    runner.overflow_watch_for_tests(&ws).unwrap();
    let event = recv_until(&mut rx, Duration::from_secs(2), |event| {
        event.kind == FsWatchKind::ResyncRequired && event.epoch == 1
    })
    .await
    .expect("lagged resync");
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
async fn restart_failure_keeps_the_live_watcher() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    runner.fail_next_watch_spawn(&ws).unwrap();
    let err = runner.restart_watch(&ws).expect_err("spawn failure");
    assert_eq!(err.code, ErrorCode::FileOperationFailed);
    recv_until(&mut rx, Duration::from_secs(2), |event| {
        event.kind == FsWatchKind::ResyncRequired && event.epoch == 1
    })
    .await
    .expect("failed restart still resyncs");

    std::fs::write(dir.path().join("keep.txt"), "still-watched\n").unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("keep.txt") && event.epoch == 1
    })
    .await
    .expect("old watcher still live");

    let epoch = runner.restart_watch(&ws).unwrap();
    assert!(epoch > 1, "epoch={epoch}");
    recv_until(&mut rx, Duration::from_secs(2), |event| {
        event.kind == FsWatchKind::ResyncRequired && event.epoch == epoch
    })
    .await
    .expect("successful restart resync");
}

#[tokio::test]
async fn outside_paths_are_not_emitted() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let outside_dir = tempdir().unwrap();
    let secret = outside_dir.path().join("secret.txt");
    std::fs::write(&secret, "secret\n").unwrap();

    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::fs::write(&secret, "changed\n").unwrap();
    std::fs::write(outside_dir.path().join("sibling.txt"), "nope\n").unwrap();

    let events = drain_for(&mut rx, Duration::from_millis(600)).await;
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
async fn symlink_replacement_invalidates_the_name() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let outside_dir = tempdir().unwrap();
    let secret = outside_dir.path().join("secret.txt");
    std::fs::write(&secret, "secret\n").unwrap();

    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::os::unix::fs::symlink(&secret, dir.path().join("tmp-link")).unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("tmp-link")
    })
    .await
    .expect("symlink create invalidates the name");

    std::fs::rename(dir.path().join("tmp-link"), dir.path().join("keep.txt")).unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("keep.txt")
    })
    .await
    .expect("destination name invalidated");

    let err = runner
        .read(&ws, "keep.txt", None, None)
        .await
        .expect_err("symlink");
    match err {
        codespace_runner::RunnerError::Execution(body) => {
            assert_eq!(body.code, ErrorCode::SymlinkRejected);
        }
        other => panic!("expected SYMLINK_REJECTED, got {other:?}"),
    }
}

#[tokio::test]
async fn rename_across_workspace_boundary_maps_each_side() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("leave.txt"), "go\n").unwrap();
    let outside_dir = tempdir().unwrap();
    std::fs::write(outside_dir.path().join("enter.txt"), "in\n").unwrap();

    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    std::fs::rename(
        dir.path().join("leave.txt"),
        outside_dir.path().join("left.txt"),
    )
    .unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("leave.txt")
    })
    .await
    .expect("inside to outside is Remove/touch of source");

    std::fs::rename(
        outside_dir.path().join("enter.txt"),
        dir.path().join("entered.txt"),
    )
    .unwrap();
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("entered.txt")
    })
    .await
    .expect("outside to inside is Create/touch of dest");
}

#[tokio::test]
async fn special_file_name_is_still_invalidated() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").unwrap();
    let ws = workspace(dir.path());
    let runner = runner();
    let mut rx = prime_watch(&runner, &ws).await;

    let fifo = dir.path().join("pipe.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "mkfifo should exist");
    recv_until(&mut rx, Duration::from_secs(5), |event| {
        event.kind.touches("pipe.fifo")
    })
    .await
    .expect("special-file name invalidated");

    let err = runner
        .read(&ws, "pipe.fifo", None, None)
        .await
        .expect_err("special");
    match err {
        codespace_runner::RunnerError::Execution(body) => {
            assert_eq!(body.code, ErrorCode::SpecialFileRejected);
        }
        other => panic!("expected SPECIAL_FILE_REJECTED, got {other:?}"),
    }
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
