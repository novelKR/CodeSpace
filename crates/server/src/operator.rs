//! The operator's commands on the process records of a gateway with an independent Runner
//! (P1R-C05, D8). A record whose Runner was lost before its execution ended holds its workspace,
//! since a process of a killed Runner can outlive it: `records list` shows the records that may
//! still hold one, with whether each root process still runs, and `records release` releases
//! the ones an operator confirmed nothing runs of. Neither ends a process, and neither asks the
//! resource authority anything.

use std::path::Path;
use std::time::Duration;

use codespace_runner::{host_boot_id, pid_runs, process_runs};
use codespace_store::{
    unsettled_records_at, ProcessLoss, ProcessPhase, ProcessRecord, RootProcess, Store, WriterLock,
};
use serde_json::{json, Value};

use crate::config::{Command, RecordsCommand};

/// Run `command` on the operations database `database`, print its report as JSON, and return
/// whether it did all it was asked.
pub fn run(command: &Command, database: Option<&Path>) -> Result<bool, String> {
    let database =
        database.ok_or("records needs --operations-db, the gateway's operations database")?;
    let boot_id = host_boot_id();
    let (report, done) = match command {
        Command::Records(RecordsCommand::List) => (list(database, boot_id.as_deref())?, true),
        Command::Records(RecordsCommand::Release { process_ids }) => {
            release(database, boot_id.as_deref(), process_ids)?
        }
    };
    let text = serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?;
    println!("{text}");
    Ok(done)
}

/// Every record that may still hold a workspace, read without writing to the database.
fn list(database: &Path, boot_id: Option<&str>) -> Result<Value, String> {
    let records = unsettled_records_at(database)?;
    Ok(json!({
        "operations_db": database.display().to_string(),
        "boot_id": boot_id,
        "records": records
            .iter()
            .map(|record| describe(record, boot_id))
            .collect::<Vec<_>>(),
    }))
}

/// Release the records `process_ids` while no gateway records into `database`. Returns the
/// report, and whether every record was released.
fn release(
    database: &Path,
    boot_id: Option<&str>,
    process_ids: &[String],
) -> Result<(Value, bool), String> {
    if !database.exists() {
        return Err(format!(
            "the operations database {} does not exist",
            database.display()
        ));
    }
    let _writer = WriterLock::acquire(database, Duration::ZERO, |_| {}).map_err(|err| {
        format!("records release changes records only while no gateway runs on the database: {err}")
    })?;
    let store = Store::open(database)?;
    let mut released = Vec::new();
    let mut already = Vec::new();
    let mut refused = Vec::new();
    for process_id in process_ids {
        match release_one(&store, boot_id, process_id)? {
            Released::Now => released.push(process_id.clone()),
            Released::Before => already.push(process_id.clone()),
            Released::Refused(reason) => refused.push(json!({
                "process_id": process_id,
                "reason": reason,
            })),
        }
    }
    let done = refused.is_empty();
    Ok((
        json!({
            "operations_db": database.display().to_string(),
            "released": released,
            "already_released": already,
            "refused": refused,
        }),
        done,
    ))
}

/// What became of one release.
enum Released {
    Now,
    Before,
    Refused(String),
}

/// Release the record of `process_id`, unless it does not wait for a release or its root
/// process still runs. A record whose Runner ran on an earlier boot of the host is lost to that
/// restart, and one whose Runner's PID runs no process is lost with that Runner, even when no
/// gateway said so yet: a Runner that died while no gateway ran leaves its records running.
/// While the Runner's PID runs a process, that may be the Runner, which still holds the record.
fn release_one(store: &Store, boot_id: Option<&str>, process_id: &str) -> Result<Released, String> {
    let Some(mut record) = store
        .process_record(process_id)
        .map_err(|err| err.message)?
    else {
        return Ok(Released::Refused("no record has this process ID".into()));
    };
    if record.released_at_ms.is_some() {
        return Ok(Released::Before);
    }
    match record.phase {
        ProcessPhase::Terminal => {
            return Ok(Released::Refused(
                "its execution ended, so its record holds no workspace".into(),
            ))
        }
        ProcessPhase::Dispatching | ProcessPhase::Running => {
            let loss = ProcessLoss::of(&record.runner, boot_id);
            if loss == ProcessLoss::RunnerLost && pid_runs(record.runner.pid) {
                return Ok(Released::Refused(format!(
                    "a process with its Runner's PID {} runs, and may be that Runner, which still \
                     holds the record: stop that Runner first, or start the gateway, which marks \
                     the record lost when no Runner answers for it",
                    record.runner.pid
                )));
            }
            store
                .record_runner_lost(&record.runner, loss)
                .map_err(|err| err.message)?;
            record = store
                .process_record(process_id)
                .map_err(|err| err.message)?
                .ok_or("the record disappeared")?;
        }
        ProcessPhase::Unknown => {}
    }
    if let Some(root) = record.root.as_ref() {
        if root_runs(&record, root, boot_id) == Some(true) {
            return Ok(Released::Refused(format!(
                "its root process {} still runs: end it, and what it started, first",
                root.pid
            )));
        }
    }
    Ok(
        if store
            .release_record(process_id)
            .map_err(|err| err.message)?
        {
            Released::Now
        } else {
            Released::Refused("its record does not wait for a release".into())
        },
    )
}

/// Whether the root process of `record` still runs on this host, by its PID and its start
/// identity, so that a later process with the same PID is not taken for it. None when the
/// record cannot tell: it names no start identity.
fn root_runs(record: &ProcessRecord, root: &RootProcess, boot_id: Option<&str>) -> Option<bool> {
    if ProcessLoss::of(&record.runner, boot_id) == ProcessLoss::HostRebooted {
        return Some(false);
    }
    Some(process_runs(root.pid, root.started.as_deref()?))
}

/// One record, as `records list` prints it.
fn describe(record: &ProcessRecord, boot_id: Option<&str>) -> Value {
    let this_boot = match (record.runner.boot_id.as_deref(), boot_id) {
        (Some(then), Some(now)) => Some(then == now),
        _ => None,
    };
    json!({
        "process_id": record.process_id,
        "workspace_id": record.workspace_id,
        "phase": record.phase.as_str(),
        "lost": record.lost.map(ProcessLoss::as_str),
        "participation": record.participation.as_str(),
        "attempt_id": record.attempt_id,
        "approval_id": record.approval_id.as_ref().map(|id| id.0.as_str()),
        "tty": record.tty,
        "runner": {
            "pid": record.runner.pid,
            "boot_id": record.runner.boot_id,
            "started_at_ms": record.runner.started_at_ms,
            "this_boot": this_boot,
            "pid_running": ProcessLoss::of(&record.runner, boot_id) != ProcessLoss::HostRebooted
                && pid_runs(record.runner.pid),
        },
        "root": record.root.as_ref().map(|root| json!({
            "pid": root.pid,
            "started": root.started,
            "running": root_runs(record, root, boot_id),
        })),
        "deadline_at_ms": record.deadline_at_ms,
        "output_cursor": record.output_cursor,
        "created_at_ms": record.created_at_ms,
        "updated_at_ms": record.updated_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::ProcessTermination;
    use codespace_runner::process_start_identity;
    use codespace_store::{NewProcessRecord, ProcessEnd, RecordedParticipation, RunnerRef};

    /// A Runner that ran on `boot_id`, whose PID runs no process: it is beyond the largest PID
    /// that Linux and macOS give a process.
    fn runner_on(boot_id: Option<String>) -> RunnerRef {
        RunnerRef {
            boot_id,
            pid: 4_194_999,
            started_at_ms: 1_000,
            nonce: "lost-runner".into(),
        }
    }

    /// A Runner that ran on `boot_id`, whose PID this test's own process runs now.
    fn runner_running_on(boot_id: Option<String>) -> RunnerRef {
        RunnerRef {
            pid: std::process::id(),
            ..runner_on(boot_id)
        }
    }

    /// Record an execution sent to `runner` in `workspace_id`, started with the root `root`.
    fn sent(store: &Store, runner: &RunnerRef, process_id: &str, root: Option<&RootProcess>) {
        store
            .record_dispatch(&NewProcessRecord {
                process_id,
                runner,
                controller_epoch: Some(1),
                workspace_id: "demo",
                approval_id: None,
                attempt_id: None,
                participation: RecordedParticipation::Off,
                tty: false,
                argv: &["/bin/sleep".to_string(), "30".to_string()],
            })
            .unwrap();
        store
            .record_running(runner, process_id, root, None)
            .unwrap();
    }

    /// A child that runs until it is killed, and the root that names it.
    fn live_root() -> (std::process::Child, RootProcess) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let root = RootProcess {
            pid: child.id(),
            started: process_start_identity(child.id()),
        };
        (child, root)
    }

    fn ids(report: &Value, key: &str) -> Vec<String> {
        report[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_string())
            .collect()
    }

    /// `records list` shows each record that may still hold a workspace, with whether its root
    /// process runs, and reads the database while a gateway holds it (P1R-C05).
    #[test]
    fn records_list_shows_what_may_still_hold_a_workspace() {
        let Some(boot) = host_boot_id() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let store = Store::open(&database).unwrap();
        let runner = runner_running_on(Some(boot.clone()));
        let (mut child, root) = live_root();
        sent(&store, &runner, "proc-running", Some(&root));
        sent(&store, &runner, "proc-ended", None);
        store
            .record_end(
                &runner,
                "proc-ended",
                &ProcessEnd::Ended {
                    termination: Some(ProcessTermination::Exited),
                    exit_code: Some(0),
                    signal: None,
                    output_total: Some(3),
                },
            )
            .unwrap();
        let earlier = runner_on(Some("an-earlier-boot".into()));
        sent(&store, &earlier, "proc-rebooted", Some(&root));
        let _gateway = WriterLock::acquire(&database, Duration::ZERO, |_| {}).unwrap();

        let report = list(&database, Some(&boot)).unwrap();
        assert_eq!(report["boot_id"], boot.as_str());
        let records = report["records"].as_array().unwrap();
        let listed: Vec<&str> = records
            .iter()
            .map(|record| record["process_id"].as_str().unwrap())
            .collect();
        assert_eq!(listed, ["proc-running", "proc-rebooted"], "{report}");
        let running = &records[0];
        assert_eq!(running["phase"], "running");
        assert_eq!(running["lost"], Value::Null);
        assert_eq!(running["workspace_id"], "demo");
        assert_eq!(running["participation"], "off");
        assert_eq!(running["runner"]["this_boot"], true);
        assert_eq!(running["runner"]["pid_running"], true);
        assert_eq!(running["root"]["pid"], root.pid);
        assert_eq!(running["root"]["running"], true, "{running}");
        // The same PIDs on a later boot are other processes.
        let rebooted = &records[1];
        assert_eq!(rebooted["runner"]["this_boot"], false);
        assert_eq!(rebooted["runner"]["pid_running"], false);
        assert_eq!(rebooted["root"]["running"], false, "{rebooted}");

        child.kill().unwrap();
        child.wait().unwrap();
        let report = list(&database, Some(&boot)).unwrap();
        assert_eq!(report["records"][0]["root"]["running"], false, "{report}");
        assert!(list(&dir.path().join("missing.sqlite"), Some(&boot)).is_err());
    }

    /// `records release` changes nothing while a gateway holds the database, and releases only
    /// a record whose Runner was lost once its root process no longer runs: never one that
    /// ended or that its Runner may still hold, and a later process with the root's PID is not
    /// the root (P1R-C05, D8).
    #[test]
    fn records_release_waits_for_the_gateway_and_the_root() {
        let Some(boot) = host_boot_id() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let store = Store::open(&database).unwrap();
        let runner = runner_running_on(Some(boot.clone()));
        let (mut child, root) = live_root();
        sent(&store, &runner, "proc-lost", Some(&root));
        let (mut other, reused) = live_root();
        // A record whose root PID a later process took.
        let gone = RootProcess {
            pid: reused.pid,
            started: Some("linux-start-ticks:1".into()),
        };
        sent(&store, &runner, "proc-reused", Some(&gone));
        sent(&store, &runner, "proc-ended", None);
        store
            .record_end(
                &runner,
                "proc-ended",
                &ProcessEnd::Ended {
                    termination: Some(ProcessTermination::Exited),
                    exit_code: Some(0),
                    signal: None,
                    output_total: None,
                },
            )
            .unwrap();
        let release_all = || {
            release(
                &database,
                Some(&boot),
                &[
                    "proc-lost".to_string(),
                    "proc-reused".to_string(),
                    "proc-ended".to_string(),
                    "proc-none".to_string(),
                ],
            )
        };

        let gateway = WriterLock::acquire(&database, Duration::ZERO, |_| {}).unwrap();
        let refusal = release_all().unwrap_err();
        assert!(refusal.contains("while no gateway runs"), "{refusal}");
        drop(gateway);

        // Nothing said yet that the Runner was lost, and its PID runs.
        let (report, done) = release_all().unwrap();
        assert!(!done);
        assert!(ids(&report, "released").is_empty(), "{report}");
        let refused = report["refused"].as_array().unwrap();
        assert_eq!(refused.len(), 4, "{report}");
        for held in &refused[..2] {
            let reason = held["reason"].as_str().unwrap();
            assert!(
                reason.contains("still holds the record")
                    && reason.contains(&std::process::id().to_string()),
                "{reason}"
            );
        }
        assert!(refused[2]["reason"].as_str().unwrap().contains("ended"));
        assert!(refused[3]["reason"].as_str().unwrap().contains("no record"));
        let record = store.process_record("proc-lost").unwrap().unwrap();
        assert_eq!((record.phase, record.lost), (ProcessPhase::Running, None));

        store
            .record_runner_lost(&runner, ProcessLoss::RunnerLost)
            .unwrap();
        let (report, done) = release_all().unwrap();
        assert!(!done);
        assert_eq!(ids(&report, "released"), ["proc-reused"], "{report}");
        let refused = report["refused"].as_array().unwrap();
        assert_eq!(refused[0]["process_id"], "proc-lost");
        let reason = refused[0]["reason"].as_str().unwrap();
        assert!(
            reason.contains("still runs") && reason.contains(&root.pid.to_string()),
            "{reason}"
        );

        child.kill().unwrap();
        child.wait().unwrap();
        let (report, _) = release_all().unwrap();
        assert_eq!(ids(&report, "released"), ["proc-lost"], "{report}");
        assert_eq!(
            ids(&report, "already_released"),
            ["proc-reused"],
            "{report}"
        );
        let record = store.process_record("proc-lost").unwrap().unwrap();
        assert_eq!(
            (record.phase, record.lost),
            (ProcessPhase::Unknown, Some(ProcessLoss::RunnerLost))
        );
        assert!(record.released_at_ms.is_some());
        assert!(unsettled_records_at(&database).unwrap().is_empty());
        other.kill().unwrap();
        other.wait().unwrap();
    }

    /// A Runner that died while no gateway ran left its records running: once its PID runs no
    /// process, `records release` marks every record of that Runner lost, as a gateway would,
    /// and releases the ones asked for whose root processes no longer run (P1R-C05).
    #[test]
    fn records_release_takes_a_runner_gone_unseen_as_lost() {
        let Some(boot) = host_boot_id() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let store = Store::open(&database).unwrap();
        let gone = runner_on(Some(boot.clone()));
        let (mut child, root) = live_root();
        sent(&store, &gone, "proc-survivor", Some(&root));
        store
            .record_dispatch(&NewProcessRecord {
                process_id: "proc-sent",
                runner: &gone,
                controller_epoch: Some(1),
                workspace_id: "other",
                approval_id: None,
                attempt_id: None,
                participation: RecordedParticipation::Off,
                tty: false,
                argv: &["/bin/true".to_string()],
            })
            .unwrap();
        let listed = list(&database, Some(&boot)).unwrap();
        assert_eq!(
            listed["records"][0]["runner"]["pid_running"], false,
            "{listed}"
        );

        let (report, done) = release(&database, Some(&boot), &["proc-sent".into()]).unwrap();
        assert!(done, "{report}");
        assert_eq!(ids(&report, "released"), ["proc-sent"]);
        let survivor = store.process_record("proc-survivor").unwrap().unwrap();
        assert_eq!(
            (survivor.phase, survivor.lost, survivor.released_at_ms),
            (ProcessPhase::Unknown, Some(ProcessLoss::RunnerLost), None)
        );
        let (report, done) = release(&database, Some(&boot), &["proc-survivor".into()]).unwrap();
        assert!(!done);
        let reason = report["refused"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("still runs"), "{reason}");

        child.kill().unwrap();
        child.wait().unwrap();
        let (report, done) = release(&database, Some(&boot), &["proc-survivor".into()]).unwrap();
        assert!(done, "{report}");
        assert!(unsettled_records_at(&database).unwrap().is_empty());
    }

    /// A record whose Runner ran on an earlier boot of the host is released as one that the
    /// restart ended, though no gateway said so yet, and whatever runs with its root's PID now
    /// (P1R-C05).
    #[test]
    fn records_release_takes_a_restart_as_the_end() {
        let Some(boot) = host_boot_id() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let store = Store::open(&database).unwrap();
        // Its Runner's PID and its root's PID run other processes now.
        let earlier = runner_running_on(Some("an-earlier-boot".into()));
        let (mut child, root) = live_root();
        sent(&store, &earlier, "proc-rebooted", Some(&root));
        let (report, done) = release(&database, Some(&boot), &["proc-rebooted".into()]).unwrap();
        assert!(done, "{report}");
        assert_eq!(ids(&report, "released"), ["proc-rebooted"]);
        let record = store.process_record("proc-rebooted").unwrap().unwrap();
        assert_eq!(
            (record.phase, record.lost),
            (ProcessPhase::Unknown, Some(ProcessLoss::HostRebooted))
        );
        assert!(record.released_at_ms.is_some());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// The records commands need the operations database, and release needs one that exists.
    #[test]
    fn records_commands_need_their_database() {
        let refusal = run(&Command::Records(RecordsCommand::List), None).unwrap_err();
        assert!(refusal.contains("--operations-db"), "{refusal}");
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.sqlite");
        let refusal = release(&missing, None, &["proc-1".into()]).unwrap_err();
        assert!(refusal.contains("does not exist"), "{refusal}");
        assert!(!missing.exists());
    }
}
