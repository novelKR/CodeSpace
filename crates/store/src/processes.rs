//! Process records (P1R-C02). A gateway connected to an independent Runner records each
//! execution before it sends it to the Runner, and follows it to its end, so that a gateway
//! that lost its memory can identify the same execution again. A record never revives a
//! handle, and never by itself proves that an execution started or how it ended: the Runner is
//! the authority on live processes.
//!
//! A record keeps the argv's digest, never the argv, its environment or a secret.

use codespace_domain::{ApprovalId, ErrorBody, ErrorCode, ProcessTermination};
use rusqlite::{params, Connection, OptionalExtension};

use crate::{approvals, hash_canonical, Store};

/// The `processes` table. What a record learns after its execution was sent is NULL until then.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS processes (
    process_id TEXT PRIMARY KEY,
    runner_boot_id TEXT,
    runner_pid INTEGER NOT NULL,
    runner_started_at_ms INTEGER NOT NULL,
    runner_nonce TEXT NOT NULL,
    controller_epoch INTEGER,
    workspace_id TEXT NOT NULL,
    approval_id TEXT,
    attempt_id TEXT,
    participation TEXT NOT NULL,
    tty INTEGER NOT NULL,
    argv_digest TEXT NOT NULL,
    phase TEXT NOT NULL CHECK (phase IN ('dispatching', 'running', 'terminal', 'unknown')),
    deadline_at_ms INTEGER,
    root_pid INTEGER,
    root_started TEXT,
    termination TEXT,
    exit_code INTEGER,
    signal INTEGER,
    output_cursor INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_processes_workspace_phase ON processes(workspace_id, phase);";

/// The Runner an execution was sent to, by the identity its `Hello` states (P1R-C01). A Runner
/// with an earlier Runner's PID, after a restart or a reboot, is another Runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRef {
    pub boot_id: Option<String>,
    pub pid: u32,
    pub started_at_ms: u64,
    pub nonce: String,
}

/// Where an execution's record stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPhase {
    /// Recorded before it was sent to the Runner: it may or may not have started.
    Dispatching,
    /// The Runner answered that it started it.
    Running,
    /// The Runner reported that it ended, or answered that it did not start it.
    Terminal,
    /// What became of it cannot be learned any more (P1R-C05). Nothing records it yet.
    Unknown,
}

impl ProcessPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "dispatching" => Self::Dispatching,
            "running" => Self::Running,
            "terminal" => Self::Terminal,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// How an execution takes part in the resource authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedParticipation {
    Off,
    Required,
}

impl RecordedParticipation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Required => "required",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "off" => Self::Off,
            "required" => Self::Required,
            _ => return None,
        })
    }
}

/// An execution about to be sent to the Runner.
#[derive(Debug, Clone)]
pub struct NewProcessRecord<'a> {
    pub process_id: &'a str,
    pub runner: &'a RunnerRef,
    /// The controller epoch of the Claim the execution is sent under (P1R-C03).
    pub controller_epoch: Option<u64>,
    pub workspace_id: &'a str,
    /// The approval this execution resumes: it is marked `resuming` with the record, in one
    /// transaction.
    pub approval_id: Option<&'a ApprovalId>,
    /// A governed execution's DevGuard attempt.
    pub attempt_id: Option<&'a str>,
    pub participation: RecordedParticipation,
    pub tty: bool,
    /// Recorded as its digest only.
    pub argv: &'a [String],
}

/// The root process the Runner created: its PID and, where the Runner can read it, the start
/// identity that tells it apart from a later process with the same PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootProcess {
    pub pid: u32,
    pub started: Option<String>,
}

impl RootProcess {
    /// Whether `observed` is this root: the same PID with the same start identity. The same
    /// PID alone never is, since the system reuses PIDs.
    pub fn is(&self, observed: &RootProcess) -> bool {
        self.pid == observed.pid && self.started.is_some() && self.started == observed.started
    }
}

/// How an execution ended, as far as the gateway learned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEnd {
    /// The Runner answered that it did not start the execution.
    NotStarted,
    /// The Runner reported that the execution ended. `termination` is `None` when the gateway
    /// could not learn how.
    Ended {
        termination: Option<ProcessTermination>,
        exit_code: Option<i32>,
        signal: Option<i32>,
        output_total: Option<u64>,
    },
}

/// How a terminal execution's record says it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// The Runner answered that it did not start the execution.
    NotStarted,
    /// The Runner's termination of the execution.
    Ended(ProcessTermination),
}

impl ProcessOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Ended(ProcessTermination::Exited) => "exited",
            Self::Ended(ProcessTermination::Signaled) => "signaled",
            Self::Ended(ProcessTermination::Timeout) => "timeout",
            Self::Ended(ProcessTermination::Terminated) => "terminated",
            Self::Ended(ProcessTermination::Unknown) => "unknown",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "not_started" => Self::NotStarted,
            "exited" => Self::Ended(ProcessTermination::Exited),
            "signaled" => Self::Ended(ProcessTermination::Signaled),
            "timeout" => Self::Ended(ProcessTermination::Timeout),
            "terminated" => Self::Ended(ProcessTermination::Terminated),
            "unknown" => Self::Ended(ProcessTermination::Unknown),
            _ => return None,
        })
    }
}

/// An execution's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    pub process_id: String,
    pub runner: RunnerRef,
    /// The controller epoch of the Claim the execution was sent under (P1R-C03); none for a
    /// record a gateway before it wrote.
    pub controller_epoch: Option<u64>,
    pub workspace_id: String,
    pub approval_id: Option<ApprovalId>,
    pub attempt_id: Option<String>,
    pub participation: RecordedParticipation,
    pub tty: bool,
    /// `sha256:` and the hex SHA-256 of the argv as a JSON array.
    pub argv_digest: String,
    pub phase: ProcessPhase,
    /// When the Runner times the execution out, in milliseconds since the Unix epoch, as it
    /// reported. The Runner's own timer decides.
    pub deadline_at_ms: Option<u64>,
    pub root: Option<RootProcess>,
    pub outcome: Option<ProcessOutcome>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// The furthest output offset a gateway observed.
    pub output_cursor: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl ProcessRecord {
    /// The Runner answered that it started the execution. A record that already ended stays
    /// ended, since the end can be reported before the answer, but learns the root and the
    /// deadline.
    fn started(&mut self, root: Option<&RootProcess>, deadline_at_ms: Option<u64>) {
        if self.phase == ProcessPhase::Dispatching {
            self.phase = ProcessPhase::Running;
        }
        if self.root.is_none() {
            self.root = root.cloned();
        }
        if self.deadline_at_ms.is_none() {
            self.deadline_at_ms = deadline_at_ms;
        }
    }

    /// The execution ended, or never started. Only a record still dispatching can be told that
    /// its execution never started. How it ended is recorded once.
    fn ended(&mut self, end: &ProcessEnd) {
        match end {
            ProcessEnd::NotStarted => {
                if self.phase == ProcessPhase::Dispatching {
                    self.phase = ProcessPhase::Terminal;
                    self.outcome = Some(ProcessOutcome::NotStarted);
                }
            }
            // The Runner answered that it did not start it: nothing reported later changes that.
            ProcessEnd::Ended { .. } if self.outcome == Some(ProcessOutcome::NotStarted) => {}
            ProcessEnd::Ended {
                termination,
                exit_code,
                signal,
                output_total,
            } => {
                self.phase = ProcessPhase::Terminal;
                if let (None, Some(termination)) = (self.outcome, termination) {
                    self.outcome = Some(ProcessOutcome::Ended(*termination));
                    self.exit_code = *exit_code;
                    self.signal = *signal;
                }
                if let Some(total) = output_total {
                    self.observed_output(*total);
                }
            }
        }
    }

    fn observed_output(&mut self, cursor: u64) {
        if self.output_cursor.is_none_or(|seen| seen < cursor) {
            self.output_cursor = Some(cursor);
        }
    }
}

impl Store {
    /// Record an execution before it is sent to the Runner. An approval it resumes is marked
    /// `resuming` in the same transaction: both are recorded, or neither.
    pub fn record_dispatch(&self, record: &NewProcessRecord<'_>) -> Result<(), ErrorBody> {
        let argv_digest = hash_canonical(&serde_json::json!(record.argv));
        let now = to_sql(now_ms());
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(record_err)?;
        if let Some(approval_id) = record.approval_id {
            approvals::mark_resuming_on(&tx, approval_id)?;
        }
        tx.execute(
            "INSERT INTO processes (
                process_id, runner_boot_id, runner_pid, runner_started_at_ms, runner_nonce,
                controller_epoch, workspace_id, approval_id, attempt_id, participation, tty,
                argv_digest, phase, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            params![
                record.process_id,
                record.runner.boot_id,
                record.runner.pid,
                to_sql(record.runner.started_at_ms),
                record.runner.nonce,
                record.controller_epoch.map(to_sql),
                record.workspace_id,
                record.approval_id.map(|id| id.0.as_str()),
                record.attempt_id,
                record.participation.as_str(),
                record.tty,
                argv_digest,
                ProcessPhase::Dispatching.as_str(),
                now,
            ],
        )
        .map_err(record_err)?;
        #[cfg(test)]
        if CRASH_BEFORE_COMMIT.load(std::sync::atomic::Ordering::SeqCst) {
            std::process::abort();
        }
        tx.commit().map_err(record_err)
    }

    /// Record that the Runner answered that it started the execution, with the root process
    /// and the deadline it reported. Returns whether the record changed: `false` also when
    /// `runner` sent no execution with this ID.
    pub fn record_running(
        &self,
        runner: &RunnerRef,
        process_id: &str,
        root: Option<&RootProcess>,
        deadline_at_ms: Option<u64>,
    ) -> Result<bool, ErrorBody> {
        self.change_record(runner, process_id, |record| {
            record.started(root, deadline_at_ms)
        })
    }

    /// Record how the execution ended, as the Runner reported it. Returns whether the record
    /// changed: `false` also when `runner` sent no execution with this ID.
    pub fn record_end(
        &self,
        runner: &RunnerRef,
        process_id: &str,
        end: &ProcessEnd,
    ) -> Result<bool, ErrorBody> {
        self.change_record(runner, process_id, |record| record.ended(end))
    }

    /// Record the furthest output offset a gateway observed. Returns whether the record changed.
    pub fn record_output(
        &self,
        runner: &RunnerRef,
        process_id: &str,
        cursor: u64,
    ) -> Result<bool, ErrorBody> {
        self.change_record(runner, process_id, |record| record.observed_output(cursor))
    }

    /// The record of the execution with this ID, whichever Runner it was sent to.
    pub fn process_record(&self, process_id: &str) -> Result<Option<ProcessRecord>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        load(&conn, process_id)
    }

    /// Change the record of an execution that `runner` was sent, and write it back when it
    /// changed. Records of executions sent to another Runner are never changed.
    fn change_record(
        &self,
        runner: &RunnerRef,
        process_id: &str,
        change: impl FnOnce(&mut ProcessRecord),
    ) -> Result<bool, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let Some(before) = load(&conn, process_id)? else {
            return Ok(false);
        };
        if before.runner != *runner {
            return Ok(false);
        }
        let mut record = before.clone();
        change(&mut record);
        if record == before {
            return Ok(false);
        }
        let updated = conn
            .execute(
                "UPDATE processes
                 SET phase = ?6, deadline_at_ms = ?7, root_pid = ?8, root_started = ?9,
                     termination = ?10, exit_code = ?11, signal = ?12, output_cursor = ?13,
                     updated_at_ms = ?14
                 WHERE process_id = ?1 AND runner_boot_id IS ?2 AND runner_pid = ?3
                   AND runner_started_at_ms = ?4 AND runner_nonce = ?5",
                params![
                    process_id,
                    runner.boot_id,
                    runner.pid,
                    to_sql(runner.started_at_ms),
                    runner.nonce,
                    record.phase.as_str(),
                    record.deadline_at_ms.map(to_sql),
                    record.root.as_ref().map(|root| root.pid),
                    record
                        .root
                        .as_ref()
                        .and_then(|root| root.started.as_deref()),
                    record.outcome.map(ProcessOutcome::as_str),
                    record.exit_code,
                    record.signal,
                    record.output_cursor.map(to_sql),
                    to_sql(now_ms()),
                ],
            )
            .map_err(record_err)?;
        Ok(updated == 1)
    }
}

/// A crash between a record's statements and its commit, for the torn-write test.
#[cfg(test)]
pub(crate) static CRASH_BEFORE_COMMIT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

const RECORD_SELECT: &str = "SELECT process_id, runner_boot_id, runner_pid, runner_started_at_ms,
    runner_nonce, controller_epoch, workspace_id, approval_id, attempt_id, participation, tty,
    argv_digest, phase, deadline_at_ms, root_pid, root_started, termination, exit_code, signal,
    output_cursor, created_at_ms, updated_at_ms FROM processes";

fn load(conn: &Connection, process_id: &str) -> Result<Option<ProcessRecord>, ErrorBody> {
    conn.query_row(
        &format!("{RECORD_SELECT} WHERE process_id = ?1"),
        params![process_id],
        map_record,
    )
    .optional()
    .map_err(record_err)?
    .transpose()
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<ProcessRecord, ErrorBody>> {
    let process_id: String = row.get(0)?;
    let participation: String = row.get(9)?;
    let phase: String = row.get(12)?;
    let root_pid: Option<u32> = row.get(14)?;
    let root_started: Option<String> = row.get(15)?;
    let termination: Option<String> = row.get(16)?;
    let unreadable = |what: &str, value: &str| {
        Err(ErrorBody::new(
            ErrorCode::Internal,
            format!("the record of process `{process_id}` has an unknown {what} `{value}`"),
        ))
    };
    let Some(participation) = RecordedParticipation::parse(&participation) else {
        return Ok(unreadable("participation", &participation));
    };
    let Some(phase) = ProcessPhase::parse(&phase) else {
        return Ok(unreadable("phase", &phase));
    };
    let outcome = match termination {
        None => None,
        Some(raw) => match ProcessOutcome::parse(&raw) {
            Some(outcome) => Some(outcome),
            None => return Ok(unreadable("termination", &raw)),
        },
    };
    Ok(Ok(ProcessRecord {
        runner: RunnerRef {
            boot_id: row.get(1)?,
            pid: row.get(2)?,
            started_at_ms: from_sql(row.get(3)?),
            nonce: row.get(4)?,
        },
        controller_epoch: row.get::<_, Option<i64>>(5)?.map(from_sql),
        workspace_id: row.get(6)?,
        approval_id: row.get::<_, Option<String>>(7)?.map(ApprovalId),
        attempt_id: row.get(8)?,
        participation,
        tty: row.get(10)?,
        argv_digest: row.get(11)?,
        phase,
        deadline_at_ms: row.get::<_, Option<i64>>(13)?.map(from_sql),
        root: root_pid.map(|pid| RootProcess {
            pid,
            started: root_started,
        }),
        outcome,
        exit_code: row.get(17)?,
        signal: row.get(18)?,
        output_cursor: row.get::<_, Option<i64>>(19)?.map(from_sql),
        created_at_ms: from_sql(row.get(20)?),
        updated_at_ms: from_sql(row.get(21)?),
        process_id,
    }))
}

fn record_err(err: rusqlite::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, format!("process record: {err}"))
}

fn to_sql(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_sql(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codespace_domain::{ApprovalDecision, ApprovalState, ApprovalTargetTool};
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::Ordering;

    use crate::ResumeClaim;

    fn runner() -> RunnerRef {
        RunnerRef {
            boot_id: Some("boot-a".into()),
            pid: 4242,
            started_at_ms: 1_760_000_000_000,
            nonce: "nonce-a".into(),
        }
    }

    fn argv() -> Vec<String> {
        vec!["cargo".into(), "test".into()]
    }

    fn dispatch(
        store: &Store,
        process_id: &str,
        runner: &RunnerRef,
        approval_id: Option<&ApprovalId>,
    ) -> Result<(), ErrorBody> {
        store.record_dispatch(&NewProcessRecord {
            process_id,
            runner,
            controller_epoch: Some(3),
            workspace_id: "demo",
            approval_id,
            attempt_id: Some("cs-attempt-1"),
            participation: RecordedParticipation::Required,
            tty: true,
            argv: &argv(),
        })
    }

    fn record(store: &Store, process_id: &str) -> ProcessRecord {
        store.process_record(process_id).unwrap().expect("a record")
    }

    fn root() -> RootProcess {
        RootProcess {
            pid: 777,
            started: Some("linux:100".into()),
        }
    }

    fn exited(code: i32, output_total: u64) -> ProcessEnd {
        ProcessEnd::Ended {
            termination: Some(ProcessTermination::Exited),
            exit_code: Some(code),
            signal: None,
            output_total: Some(output_total),
        }
    }

    /// A granted exec approval that `operation_resume` claimed: `queued`, about to dispatch.
    fn queued_approval(store: &Store, command: &str) -> ApprovalId {
        let created = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ExecCommand,
                &format!("fp-{command}"),
                serde_json::json!({ "workspace_id": "demo", "command": [command] }),
            )
            .unwrap();
        store
            .resolve_approval(&created.approval_id, ApprovalDecision::Grant)
            .unwrap();
        let ResumeClaim::Execute(_, _guard) = store.claim_resume(&created.approval_id).unwrap()
        else {
            panic!("expected execute");
        };
        created.approval_id
    }

    fn approval_state(store: &Store, approval_id: &ApprovalId) -> ApprovalState {
        store.inspect_approval(approval_id).unwrap().0
    }

    /// A gateway that lost its memory finds the execution by its `process_id`, from the record
    /// written before it was sent to the one that says how it ended.
    #[test]
    fn a_new_gateway_finds_each_execution_from_dispatch_to_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let runner = runner();
        dispatch(&Store::open(&database).unwrap(), "proc-1", &runner, None).unwrap();

        let store = Store::open(&database).unwrap();
        let sent = record(&store, "proc-1");
        assert_eq!(sent.phase, ProcessPhase::Dispatching);
        assert_eq!(sent.runner, runner);
        assert_eq!(sent.controller_epoch, Some(3));
        assert_eq!(sent.workspace_id, "demo");
        assert_eq!(sent.approval_id, None);
        assert_eq!(sent.attempt_id.as_deref(), Some("cs-attempt-1"));
        assert_eq!(sent.participation, RecordedParticipation::Required);
        assert!(sent.tty);
        assert_eq!(sent.argv_digest, hash_canonical(&serde_json::json!(argv())));
        assert_eq!(
            (
                sent.root,
                sent.deadline_at_ms,
                sent.outcome,
                sent.output_cursor
            ),
            (None, None, None, None)
        );
        assert_eq!(sent.created_at_ms, sent.updated_at_ms);
        assert!(store
            .record_running(&runner, "proc-1", Some(&root()), Some(1_760_000_030_000))
            .unwrap());
        drop(store);

        let store = Store::open(&database).unwrap();
        let running = record(&store, "proc-1");
        assert_eq!(running.phase, ProcessPhase::Running);
        assert_eq!(running.root, Some(root()));
        assert_eq!(running.deadline_at_ms, Some(1_760_000_030_000));
        assert!(store.record_output(&runner, "proc-1", 10).unwrap());
        // A cursor never goes back.
        assert!(!store.record_output(&runner, "proc-1", 4).unwrap());
        assert!(store.record_end(&runner, "proc-1", &exited(0, 12)).unwrap());
        drop(store);

        let store = Store::open(&database).unwrap();
        let ended = record(&store, "proc-1");
        assert_eq!(ended.phase, ProcessPhase::Terminal);
        assert_eq!(
            ended.outcome,
            Some(ProcessOutcome::Ended(ProcessTermination::Exited))
        );
        assert_eq!((ended.exit_code, ended.signal), (Some(0), None));
        assert_eq!(ended.output_cursor, Some(12));
        assert_eq!(ended.root, Some(root()));
        // How it ended is recorded once, and an ended record stays ended.
        let signaled = ProcessEnd::Ended {
            termination: Some(ProcessTermination::Signaled),
            exit_code: None,
            signal: Some(9),
            output_total: Some(12),
        };
        assert!(!store.record_end(&runner, "proc-1", &signaled).unwrap());
        assert!(!store
            .record_running(&runner, "proc-1", Some(&root()), Some(1))
            .unwrap());
        assert_eq!(record(&store, "proc-1"), ended);
    }

    /// The Runner's exit event can reach the gateway before its answer to the execution: the
    /// answer then leaves the record ended, and fills in only the root and the deadline.
    #[test]
    fn an_end_reported_before_the_start_answer_stays_the_end() {
        let store = Store::memory().unwrap();
        let runner = runner();
        dispatch(&store, "proc-race", &runner, None).unwrap();
        assert!(store
            .record_end(&runner, "proc-race", &exited(3, 5))
            .unwrap());
        assert!(store
            .record_running(&runner, "proc-race", Some(&root()), Some(1_760_000_030_000))
            .unwrap());
        let raced = record(&store, "proc-race");
        assert_eq!(raced.phase, ProcessPhase::Terminal);
        assert_eq!(
            raced.outcome,
            Some(ProcessOutcome::Ended(ProcessTermination::Exited))
        );
        assert_eq!(raced.exit_code, Some(3));
        assert_eq!(raced.root, Some(root()));
        assert_eq!(raced.deadline_at_ms, Some(1_760_000_030_000));
    }

    /// An end whose details the gateway could not learn is completed by the first report
    /// that has them, and only by that one.
    #[test]
    fn an_end_without_details_is_completed_once() {
        let store = Store::memory().unwrap();
        let runner = runner();
        dispatch(&store, "proc-1", &runner, None).unwrap();
        store
            .record_running(&runner, "proc-1", Some(&root()), None)
            .unwrap();
        let unobserved = ProcessEnd::Ended {
            termination: None,
            exit_code: None,
            signal: None,
            output_total: None,
        };
        assert!(store.record_end(&runner, "proc-1", &unobserved).unwrap());
        let ended = record(&store, "proc-1");
        assert_eq!((ended.phase, ended.outcome), (ProcessPhase::Terminal, None));
        let timeout = ProcessEnd::Ended {
            termination: Some(ProcessTermination::Timeout),
            exit_code: None,
            signal: None,
            output_total: Some(5),
        };
        assert!(store.record_end(&runner, "proc-1", &timeout).unwrap());
        assert!(!store.record_end(&runner, "proc-1", &exited(0, 5)).unwrap());
        let ended = record(&store, "proc-1");
        assert_eq!(
            ended.outcome,
            Some(ProcessOutcome::Ended(ProcessTermination::Timeout))
        );
        assert_eq!((ended.exit_code, ended.output_cursor), (None, Some(5)));
    }

    /// Only an execution that is still dispatching can be one the Runner did not start.
    #[test]
    fn an_execution_the_runner_did_not_start_ends_as_not_started() {
        let store = Store::memory().unwrap();
        let runner = runner();
        dispatch(&store, "proc-refused", &runner, None).unwrap();
        assert!(store
            .record_end(&runner, "proc-refused", &ProcessEnd::NotStarted)
            .unwrap());
        let refused = record(&store, "proc-refused");
        assert_eq!(refused.phase, ProcessPhase::Terminal);
        assert_eq!(refused.outcome, Some(ProcessOutcome::NotStarted));
        assert!(!store
            .record_end(&runner, "proc-refused", &exited(0, 0))
            .unwrap());

        dispatch(&store, "proc-started", &runner, None).unwrap();
        store
            .record_running(&runner, "proc-started", None, None)
            .unwrap();
        assert!(!store
            .record_end(&runner, "proc-started", &ProcessEnd::NotStarted)
            .unwrap());
        assert_eq!(record(&store, "proc-started").phase, ProcessPhase::Running);
    }

    /// A record names its Runner by the whole identity of its `Hello`. A later Runner with the
    /// same PID, as after a reboot, never changes it, nor does a gateway connected to it.
    #[test]
    fn a_runner_with_the_same_pid_is_another_runner() {
        let store = Store::memory().unwrap();
        let runner = runner();
        dispatch(&store, "proc-1", &runner, None).unwrap();
        let before = record(&store, "proc-1");
        for other in [
            RunnerRef {
                nonce: "nonce-b".into(),
                ..runner.clone()
            },
            RunnerRef {
                started_at_ms: runner.started_at_ms + 1,
                ..runner.clone()
            },
            RunnerRef {
                boot_id: Some("boot-b".into()),
                ..runner.clone()
            },
            RunnerRef {
                boot_id: None,
                ..runner.clone()
            },
            RunnerRef {
                pid: runner.pid + 1,
                ..runner.clone()
            },
        ] {
            assert!(!store
                .record_running(&other, "proc-1", Some(&root()), Some(1))
                .unwrap());
            assert!(!store.record_end(&other, "proc-1", &exited(0, 1)).unwrap());
            assert!(!store.record_output(&other, "proc-1", 1).unwrap());
        }
        assert_eq!(record(&store, "proc-1"), before);
    }

    /// A root process is the one recorded only with the same PID and the same start identity,
    /// so a later process that reuses the PID is told apart. A PID without its start identity
    /// proves nothing.
    #[test]
    fn a_root_is_the_recorded_process_only_with_its_start_identity() {
        let recorded = root();
        assert!(recorded.is(&root()));
        let reused = RootProcess {
            pid: 777,
            started: Some("linux:250".into()),
        };
        assert!(!recorded.is(&reused));
        let other = RootProcess {
            pid: 778,
            started: Some("linux:100".into()),
        };
        assert!(!recorded.is(&other));
        let unnamed = RootProcess {
            pid: 777,
            started: None,
        };
        assert!(!recorded.is(&unnamed));
        assert!(!unnamed.is(&unnamed.clone()));
    }

    /// An execution that resumes an approval is recorded in the transaction that marks the
    /// approval `resuming`: both are written, or neither.
    #[test]
    fn a_resumed_execution_is_recorded_with_its_approval_or_not_at_all() {
        let store = Store::memory().unwrap();
        let runner = runner();
        let approval = queued_approval(&store, "one");
        dispatch(&store, "proc-resume", &runner, Some(&approval)).unwrap();
        assert_eq!(approval_state(&store, &approval), ApprovalState::Resuming);
        assert_eq!(
            record(&store, "proc-resume").approval_id,
            Some(approval.clone())
        );

        // The record cannot be written: the approval stays queued.
        let second = queued_approval(&store, "two");
        let err = dispatch(&store, "proc-resume", &runner, Some(&second)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(approval_state(&store, &second), ApprovalState::Queued);
        assert_eq!(record(&store, "proc-resume").approval_id, Some(approval));

        // The approval cannot be resumed: nothing is recorded.
        let pending = store
            .create_approval(
                "demo",
                ApprovalTargetTool::ExecCommand,
                "fp-pending",
                serde_json::json!({ "workspace_id": "demo", "command": ["three"] }),
            )
            .unwrap()
            .approval_id;
        let err = dispatch(&store, "proc-pending", &runner, Some(&pending)).unwrap_err();
        assert_eq!(err.code, ErrorCode::ApprovalConflict);
        assert!(store.process_record("proc-pending").unwrap().is_none());
        assert_eq!(approval_state(&store, &pending), ApprovalState::Pending);
    }

    /// A gateway that crashes while it writes an execution's record, after the statements and
    /// before the commit, leaves neither the record nor the approval's resume: the next gateway
    /// finds the approval queued, and the database intact.
    #[test]
    fn a_crash_while_writing_leaves_neither_the_record_nor_the_resume() {
        const DATABASE: &str = "CODESPACE_STORE_TORN_DATABASE";
        const APPROVAL: &str = "CODESPACE_STORE_TORN_APPROVAL";
        if let (Some(database), Ok(approval)) =
            (std::env::var_os(DATABASE), std::env::var(APPROVAL))
        {
            // The crashing gateway, in a copy of this test binary.
            let store = Store::open(Path::new(&database)).unwrap();
            CRASH_BEFORE_COMMIT.store(true, Ordering::SeqCst);
            let written = dispatch(&store, "proc-torn", &runner(), Some(&ApprovalId(approval)));
            panic!("the write returned instead of crashing: {written:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let approval = queued_approval(&Store::open(&database).unwrap(), "torn");
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "processes::tests::a_crash_while_writing_leaves_neither_the_record_nor_the_resume",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(DATABASE, &database)
            .env(APPROVAL, &approval.0)
            .output()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGABRT),
            "the copy did not crash at the commit:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let store = Store::open(&database).unwrap();
        assert!(store.process_record("proc-torn").unwrap().is_none());
        assert_eq!(approval_state(&store, &approval), ApprovalState::Queued);
        let check: String = rusqlite::Connection::open(&database)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, "ok");
        // The next attempt records both.
        dispatch(&store, "proc-torn", &runner(), Some(&approval)).unwrap();
        assert_eq!(approval_state(&store, &approval), ApprovalState::Resuming);
    }

    /// A record keeps the argv's digest: neither the argv nor a secret in it reaches the
    /// database file.
    #[test]
    fn a_record_keeps_no_argv_and_no_secret() {
        const SECRET: &str = "cs-test-secret-6d1f0c9a";
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ops.sqlite");
        let argv = vec![
            "curl".to_string(),
            "--header".to_string(),
            format!("Authorization: Bearer {SECRET}"),
        ];
        let runner = runner();
        {
            let store = Store::open(&database).unwrap();
            store
                .record_dispatch(&NewProcessRecord {
                    process_id: "proc-secret",
                    runner: &runner,
                    controller_epoch: None,
                    workspace_id: "demo",
                    approval_id: None,
                    attempt_id: None,
                    participation: RecordedParticipation::Off,
                    tty: false,
                    argv: &argv,
                })
                .unwrap();
            store
                .record_running(&runner, "proc-secret", Some(&root()), Some(1))
                .unwrap();
            store
                .record_end(&runner, "proc-secret", &exited(0, 1))
                .unwrap();
            let kept = record(&store, "proc-secret");
            assert_eq!(kept.argv_digest, hash_canonical(&serde_json::json!(argv)));
            assert!(
                kept.argv_digest.starts_with("sha256:"),
                "{}",
                kept.argv_digest
            );
        }
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            bytes.extend(std::fs::read(entry.unwrap().path()).unwrap());
        }
        for needle in [SECRET, "Authorization", "--header"] {
            assert!(
                !bytes
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "{needle} reached the database"
            );
        }
    }
}
