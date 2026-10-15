//! Process records (P1R-C02). A gateway connected to an independent Runner records each
//! execution before it sends it to the Runner, and follows it to its end, so that a gateway
//! that lost its memory can identify the same execution again. A record never revives a
//! handle, and never by itself proves that an execution started or how it ended: the Runner is
//! the authority on live processes.
//!
//! A record keeps the argv's digest, never the argv, its environment or a secret.
//!
//! When the Runner an execution was sent to is lost, what became of the execution cannot be
//! learned any more (P1R-C05): a record that had not ended becomes `unknown`, with why, and
//! holds its workspace until an operator confirms that nothing of it runs and releases it.

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
    updated_at_ms INTEGER NOT NULL,
    lost TEXT,
    released_at_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_processes_workspace_phase ON processes(workspace_id, phase);";

/// The columns that a database from before P1R-C05 lacks: added, empty, when it is opened. A
/// CodeSpace from before them leaves them alone, so the schema version stays.
const LOSS_COLUMNS: [(&str, &str); 2] = [("lost", "TEXT"), ("released_at_ms", "INTEGER")];

/// Add the columns that a `processes` table from before P1R-C05 lacks.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let columns = crate::table_columns(conn, "processes")?;
    for (name, kind) in LOSS_COLUMNS {
        if !columns.iter().any(|column| column == name) {
            conn.execute(
                &format!("ALTER TABLE processes ADD COLUMN {name} {kind}"),
                [],
            )
            .map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

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
    /// What became of it cannot be learned any more: the Runner it was sent to was lost before
    /// it ended (P1R-C05). The record says why.
    Unknown,
}

impl ProcessPhase {
    pub fn as_str(self) -> &'static str {
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

/// Why what became of an execution cannot be learned any more (P1R-C05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLoss {
    /// The Runner it was sent to was lost: nothing accepts connections at the Runner's socket,
    /// or another Runner answers there. A process of a killed Runner can outlive it, so the
    /// execution may still run.
    RunnerLost,
    /// The host restarted since the Runner it was sent to started, which ended it.
    HostRebooted,
}

impl ProcessLoss {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunnerLost => "runner_lost",
            Self::HostRebooted => "host_rebooted",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "runner_lost" => Self::RunnerLost,
            "host_rebooted" => Self::HostRebooted,
            _ => return None,
        })
    }

    /// Why an execution sent to the Runner `lost` cannot be followed any more, on the host
    /// whose current boot is `boot_id`: a Runner of another boot ended with that boot. Without
    /// both boots, nothing proves a restart.
    pub fn of(lost: &RunnerRef, boot_id: Option<&str>) -> Self {
        match (lost.boot_id.as_deref(), boot_id) {
            (Some(then), Some(now)) if then != now => Self::HostRebooted,
            _ => Self::RunnerLost,
        }
    }
}

/// How an execution takes part in the resource authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedParticipation {
    Off,
    Required,
}

impl RecordedParticipation {
    pub fn as_str(self) -> &'static str {
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
    /// Why what became of the execution cannot be learned, once its Runner was lost before it
    /// ended (P1R-C05).
    pub lost: Option<ProcessLoss>,
    /// When an operator released the workspace that the record held after its Runner was lost,
    /// in milliseconds since the Unix epoch (P1R-C05, D8).
    pub released_at_ms: Option<u64>,
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

    /// Whether the record still holds its workspace although what became of its execution
    /// cannot be learned: it waits for an operator (P1R-C05).
    pub fn awaits_release(&self) -> bool {
        self.phase == ProcessPhase::Unknown && self.released_at_ms.is_none()
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

    /// The records that a gateway reconciles after its Claim of `runner` under `epoch`
    /// (P1R-C04): every execution sent to another Runner, or to `runner` under an earlier Claim
    /// or before Claims were recorded, that has not ended as far as its record says, and every
    /// one whose approval is still being resumed. What the gateway sends under its own Claim is
    /// not, nor what an operator released (P1R-C05). Oldest first.
    pub fn records_to_reconcile(
        &self,
        runner: &RunnerRef,
        epoch: u64,
    ) -> Result<Vec<ProcessRecord>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let mut statement = conn
            .prepare(&format!(
                "{RECORD_SELECT}
                 WHERE (phase IN ('dispatching', 'running', 'unknown')
                        OR approval_id IN
                           (SELECT approval_id FROM approvals WHERE state = 'resuming'))
                   AND released_at_ms IS NULL
                   AND NOT (runner_boot_id IS ?1 AND runner_pid = ?2
                            AND runner_started_at_ms = ?3 AND runner_nonce = ?4
                            AND controller_epoch IS NOT NULL AND controller_epoch >= ?5)
                 ORDER BY created_at_ms, process_id"
            ))
            .map_err(record_err)?;
        let rows = statement
            .query_map(
                params![
                    runner.boot_id,
                    runner.pid,
                    to_sql(runner.started_at_ms),
                    runner.nonce,
                    to_sql(epoch),
                ],
                map_record,
            )
            .map_err(record_err)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(record_err)??);
        }
        Ok(records)
    }

    /// The records of the executions that resumed this approval, oldest first (P1R-C04).
    pub fn records_of_approval(
        &self,
        approval_id: &ApprovalId,
    ) -> Result<Vec<ProcessRecord>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let mut statement = conn
            .prepare(&format!(
                "{RECORD_SELECT} WHERE approval_id = ?1 ORDER BY created_at_ms, process_id"
            ))
            .map_err(record_err)?;
        let rows = statement
            .query_map(params![approval_id.0], map_record)
            .map_err(record_err)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(record_err)??);
        }
        Ok(records)
    }

    /// The Runner `lost` was lost (P1R-C05): what became of each execution sent to it that had
    /// not ended, as far as its record says, cannot be learned any more. Each such record
    /// becomes `unknown` for `loss`, and holds its workspace until an operator releases it.
    /// Returns their IDs, oldest first; a record that ended stays as it is.
    pub fn record_runner_lost(
        &self,
        lost: &RunnerRef,
        loss: ProcessLoss,
    ) -> Result<Vec<String>, ErrorBody> {
        let mut conn = self.conn.lock().expect("sqlite mutex");
        let tx = conn.transaction().map_err(record_err)?;
        let identity = params![
            lost.boot_id,
            lost.pid,
            to_sql(lost.started_at_ms),
            lost.nonce
        ];
        let unended = "runner_boot_id IS ?1 AND runner_pid = ?2 AND runner_started_at_ms = ?3
             AND runner_nonce = ?4 AND phase IN ('dispatching', 'running')";
        let ids = {
            let mut statement = tx
                .prepare(&format!(
                    "SELECT process_id FROM processes WHERE {unended}
                     ORDER BY created_at_ms, process_id"
                ))
                .map_err(record_err)?;
            let rows = statement
                .query_map(identity, |row| row.get::<_, String>(0))
                .map_err(record_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(record_err)?
        };
        tx.execute(
            &format!(
                "UPDATE processes SET phase = 'unknown', lost = ?5, updated_at_ms = ?6
                 WHERE {unended}"
            ),
            params![
                lost.boot_id,
                lost.pid,
                to_sql(lost.started_at_ms),
                lost.nonce,
                loss.as_str(),
                to_sql(now_ms()),
            ],
        )
        .map_err(record_err)?;
        tx.commit().map_err(record_err)?;
        Ok(ids)
    }

    /// An operator confirmed that nothing of the execution `process_id` runs any more, whose
    /// Runner was lost, and releases the workspace its record holds (P1R-C05, D8). The record
    /// stays `unknown`: how the execution ended was never learned. Returns whether it was
    /// released now: not when it is not waiting for a release, or already was released.
    pub fn release_record(&self, process_id: &str) -> Result<bool, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        let now = to_sql(now_ms());
        let released = conn
            .execute(
                "UPDATE processes SET released_at_ms = ?2, updated_at_ms = ?2
                 WHERE process_id = ?1 AND phase = 'unknown' AND released_at_ms IS NULL",
                params![process_id, now],
            )
            .map_err(record_err)?;
        Ok(released == 1)
    }

    /// The records that may still hold a workspace (P1R-C05): every execution whose record has
    /// not ended and was not released, whichever Runner it was sent to, oldest first.
    pub fn unsettled_records(&self) -> Result<Vec<ProcessRecord>, ErrorBody> {
        let conn = self.conn.lock().expect("sqlite mutex");
        unsettled(&conn, RECORD_SELECT)
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
    output_cursor, created_at_ms, updated_at_ms, lost, released_at_ms FROM processes";

/// [`RECORD_SELECT`] for a table from before P1R-C05, which records no loss.
const RECORD_SELECT_BEFORE_LOSS: &str = "SELECT * FROM (SELECT process_id, runner_boot_id,
    runner_pid, runner_started_at_ms, runner_nonce, controller_epoch, workspace_id, approval_id,
    attempt_id, participation, tty, argv_digest, phase, deadline_at_ms, root_pid, root_started,
    termination, exit_code, signal, output_cursor, created_at_ms, updated_at_ms,
    NULL AS lost, NULL AS released_at_ms FROM processes)";

/// The records that may still hold a workspace, read with `select`.
fn unsettled(conn: &Connection, select: &str) -> Result<Vec<ProcessRecord>, ErrorBody> {
    let mut statement = conn
        .prepare(&format!(
            "{select}
             WHERE phase IN ('dispatching', 'running', 'unknown') AND released_at_ms IS NULL
             ORDER BY created_at_ms, process_id"
        ))
        .map_err(record_err)?;
    let rows = statement.query_map([], map_record).map_err(record_err)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(record_err)??);
    }
    Ok(records)
}

/// The records that may still hold a workspace in the operations database at `database`, read
/// without writing anything to it, so that an operator can look while a gateway records into
/// it (P1R-C05). A database without process records has none; one that a newer CodeSpace wrote
/// is refused.
pub fn unsettled_records_at(database: &std::path::Path) -> Result<Vec<ProcessRecord>, String> {
    use rusqlite::OpenFlags;
    if !database.exists() {
        return Err(format!(
            "the operations database {} does not exist",
            database.display()
        ));
    }
    let conn = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| format!("cannot open {}: {err}", database.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|err| err.to_string())?;
    let version = crate::schema_version(&conn)?;
    if version > crate::SCHEMA_VERSION {
        return Err(format!(
            "the operations database has schema version {version}, newer than this \
             CodeSpace's {}: read it with the CodeSpace that wrote it",
            crate::SCHEMA_VERSION
        ));
    }
    let columns = crate::table_columns(&conn, "processes")?;
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let select = if LOSS_COLUMNS
        .iter()
        .all(|(name, _)| columns.iter().any(|column| column == name))
    {
        RECORD_SELECT
    } else {
        RECORD_SELECT_BEFORE_LOSS
    };
    unsettled(&conn, select).map_err(|err| err.message)
}

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
    let lost: Option<String> = row.get(22)?;
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
    let lost = match lost {
        None => None,
        Some(raw) => match ProcessLoss::parse(&raw) {
            Some(loss) => Some(loss),
            None => return Ok(unreadable("loss", &raw)),
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
        lost,
        released_at_ms: row.get::<_, Option<i64>>(23)?.map(from_sql),
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

    /// An execution sent under the controller epoch `epoch`, which may resume `approval_id`.
    fn dispatch_under(
        store: &Store,
        process_id: &str,
        epoch: Option<u64>,
        approval_id: Option<&ApprovalId>,
    ) {
        dispatch_to(store, &runner(), process_id, epoch, approval_id);
    }

    /// An execution sent to `runner` under the controller epoch `epoch`, which may resume
    /// `approval_id`.
    fn dispatch_to(
        store: &Store,
        runner: &RunnerRef,
        process_id: &str,
        epoch: Option<u64>,
        approval_id: Option<&ApprovalId>,
    ) {
        store
            .record_dispatch(&NewProcessRecord {
                process_id,
                runner,
                controller_epoch: epoch,
                workspace_id: "demo",
                approval_id,
                attempt_id: None,
                participation: RecordedParticipation::Off,
                tty: false,
                argv: &argv(),
            })
            .unwrap();
    }

    fn ids(records: &[ProcessRecord]) -> Vec<&str> {
        let mut ids: Vec<_> = records
            .iter()
            .map(|record| record.process_id.as_str())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// After its Claim under epoch 4 a gateway reconciles what earlier Claims, gateways before
    /// Claims were recorded, and other Runners left unsettled: executions whose records have
    /// not ended, and those whose approval is still being resumed. What it sends under its own
    /// Claim, and what ended, are not (P1R-C04).
    #[test]
    fn a_gateway_reconciles_what_earlier_claims_left_unsettled() {
        let store = Store::memory().unwrap();
        let runner = runner();
        dispatch_under(&store, "proc-dispatching", Some(3), None);
        dispatch_under(&store, "proc-running", Some(2), None);
        store
            .record_running(&runner, "proc-running", Some(&root()), None)
            .unwrap();
        dispatch_under(&store, "proc-unrecorded-epoch", None, None);
        dispatch_under(&store, "proc-ended", Some(3), None);
        store
            .record_end(&runner, "proc-ended", &exited(0, 4))
            .unwrap();
        let resumed = queued_approval(&store, "resumed");
        dispatch_under(&store, "proc-resumed", Some(3), Some(&resumed));
        store
            .record_end(&runner, "proc-resumed", &ProcessEnd::NotStarted)
            .unwrap();
        dispatch_under(&store, "proc-current", Some(4), None);
        // Another Runner's epochs are its own; a Runner after a reboot is another Runner.
        let other = RunnerRef {
            nonce: "nonce-b".into(),
            ..runner.clone()
        };
        dispatch_to(&store, &other, "proc-other-runner", Some(9), None);
        let rebooted = RunnerRef {
            boot_id: None,
            ..runner.clone()
        };
        dispatch_to(&store, &rebooted, "proc-other-boot", Some(4), None);
        dispatch_to(&store, &other, "proc-other-ended", Some(1), None);
        store
            .record_end(&other, "proc-other-ended", &exited(0, 1))
            .unwrap();
        assert_eq!(
            ids(&store.records_to_reconcile(&runner, 4).unwrap()),
            [
                "proc-dispatching",
                "proc-other-boot",
                "proc-other-runner",
                "proc-resumed",
                "proc-running",
                "proc-unrecorded-epoch"
            ]
        );
        assert_eq!(
            ids(&store.records_to_reconcile(&runner, 3).unwrap()),
            [
                "proc-other-boot",
                "proc-other-runner",
                "proc-running",
                "proc-unrecorded-epoch"
            ]
        );
        // Once its approval is settled, an ended execution needs nothing more.
        assert!(store.settle_interrupted_resume(&resumed, None).unwrap());
        assert_eq!(
            ids(&store.records_to_reconcile(&runner, 4).unwrap()),
            [
                "proc-dispatching",
                "proc-other-boot",
                "proc-other-runner",
                "proc-running",
                "proc-unrecorded-epoch"
            ]
        );
        assert_eq!(
            ids(&store.records_of_approval(&resumed).unwrap()),
            ["proc-resumed"]
        );
    }

    /// Once a Runner is lost, each record of what was sent to it that had not ended becomes
    /// `unknown`, with why, and holds its workspace: a gateway reconciles it, and it is listed,
    /// until an operator releases it (P1R-C05). A record that ended, and one of another Runner,
    /// stay as they are. An end the lost Runner reported still ends its record.
    #[test]
    fn a_lost_runners_unended_records_wait_for_a_release() {
        let store = Store::memory().unwrap();
        let runner = runner();
        let other = RunnerRef {
            nonce: "nonce-b".into(),
            ..runner.clone()
        };
        dispatch_under(&store, "proc-dispatching", Some(1), None);
        dispatch_under(&store, "proc-running", Some(1), None);
        store
            .record_running(&runner, "proc-running", Some(&root()), None)
            .unwrap();
        dispatch_under(&store, "proc-ended", Some(1), None);
        store
            .record_end(&runner, "proc-ended", &exited(0, 3))
            .unwrap();
        dispatch_to(&store, &other, "proc-other", Some(1), None);
        let ended = record(&store, "proc-ended");

        assert_eq!(
            store
                .record_runner_lost(&runner, ProcessLoss::RunnerLost)
                .unwrap(),
            ["proc-dispatching", "proc-running"]
        );
        for process_id in ["proc-dispatching", "proc-running"] {
            let lost = record(&store, process_id);
            assert_eq!(lost.phase, ProcessPhase::Unknown, "{process_id}");
            assert_eq!(lost.lost, Some(ProcessLoss::RunnerLost), "{process_id}");
            assert_eq!(lost.released_at_ms, None);
            assert!(lost.awaits_release());
        }
        assert_eq!(record(&store, "proc-running").root, Some(root()));
        assert_eq!(record(&store, "proc-ended"), ended);
        assert_eq!(
            record(&store, "proc-other").phase,
            ProcessPhase::Dispatching
        );
        // Nothing is left to lose.
        assert!(store
            .record_runner_lost(&runner, ProcessLoss::HostRebooted)
            .unwrap()
            .is_empty());
        assert_eq!(
            record(&store, "proc-running").lost,
            Some(ProcessLoss::RunnerLost)
        );

        let current = RunnerRef {
            nonce: "nonce-c".into(),
            ..runner.clone()
        };
        assert_eq!(
            ids(&store.unsettled_records().unwrap()),
            ["proc-dispatching", "proc-other", "proc-running"]
        );
        assert_eq!(
            ids(&store.records_to_reconcile(&current, 1).unwrap()),
            ["proc-dispatching", "proc-other", "proc-running"]
        );

        // Only a record that waits for it is released, once.
        assert!(store.release_record("proc-running").unwrap());
        assert!(!store.release_record("proc-running").unwrap());
        for process_id in ["proc-ended", "proc-other", "proc-unrecorded"] {
            assert!(!store.release_record(process_id).unwrap(), "{process_id}");
        }
        let released = record(&store, "proc-running");
        assert_eq!(released.phase, ProcessPhase::Unknown);
        assert!(released.released_at_ms.is_some());
        assert!(!released.awaits_release());
        assert_eq!(
            ids(&store.unsettled_records().unwrap()),
            ["proc-dispatching", "proc-other"]
        );
        assert_eq!(
            ids(&store.records_to_reconcile(&current, 1).unwrap()),
            ["proc-dispatching", "proc-other"]
        );

        // The lost Runner reported this end before it was lost: the record ends.
        assert!(store
            .record_end(&runner, "proc-dispatching", &exited(2, 1))
            .unwrap());
        let ended = record(&store, "proc-dispatching");
        assert_eq!(ended.phase, ProcessPhase::Terminal);
        assert_eq!(
            (ended.exit_code, ended.lost),
            (Some(2), Some(ProcessLoss::RunnerLost))
        );
        assert!(!store.release_record("proc-dispatching").unwrap());
        assert_eq!(ids(&store.unsettled_records().unwrap()), ["proc-other"]);
    }

    /// A lost Runner of an earlier boot of the host ended with that boot; without both boots,
    /// nothing proves a restart (P1R-C05).
    #[test]
    fn a_loss_names_a_restart_only_across_two_known_boots() {
        let then = runner();
        assert_eq!(
            ProcessLoss::of(&then, Some("boot-b")),
            ProcessLoss::HostRebooted
        );
        assert_eq!(
            ProcessLoss::of(&then, Some("boot-a")),
            ProcessLoss::RunnerLost
        );
        assert_eq!(ProcessLoss::of(&then, None), ProcessLoss::RunnerLost);
        let unbooted = RunnerRef {
            boot_id: None,
            ..then
        };
        assert_eq!(
            ProcessLoss::of(&unbooted, Some("boot-b")),
            ProcessLoss::RunnerLost
        );
        for loss in [ProcessLoss::RunnerLost, ProcessLoss::HostRebooted] {
            assert_eq!(ProcessLoss::parse(loss.as_str()), Some(loss));
        }
    }

    /// An approval that a gateway left `resuming` is consumed with the process the Runner
    /// created, or given back when it created none, once; never while a resume of it is in
    /// progress (P1R-C04).
    #[test]
    fn an_interrupted_resume_is_settled_once() {
        let store = Store::memory().unwrap();
        let started = queued_approval(&store, "started");
        dispatch_under(&store, "proc-started", Some(1), Some(&started));
        let result = codespace_domain::OperationResumeResult {
            approval_id: started.clone(),
            state: ApprovalState::Consumed,
            apply_patch: None,
            exec_command: Some(codespace_domain::ExecCommandResult {
                process_id: codespace_domain::ProcessId("proc-started".into()),
                dispatch_status: codespace_domain::ExecDispatchStatus::Confirmed,
                coordination: None,
            }),
        };
        assert!(store
            .settle_interrupted_resume(&started, Some(&result))
            .unwrap());
        assert_eq!(approval_state(&store, &started), ApprovalState::Consumed);
        assert!(!store.settle_interrupted_resume(&started, None).unwrap());
        assert_eq!(approval_state(&store, &started), ApprovalState::Consumed);
        match store.claim_resume(&started).unwrap() {
            ResumeClaim::ReplaySuccess(replayed) => assert_eq!(replayed, result),
            other => panic!("unexpected {other:?}"),
        }

        let unstarted = queued_approval(&store, "unstarted");
        dispatch_under(&store, "proc-unstarted", Some(1), Some(&unstarted));
        {
            // A resume in progress settles it itself.
            let ResumeClaim::Reconcile(_, _guard) = store.claim_resume(&unstarted).unwrap() else {
                panic!("expected reconcile");
            };
            let err = store
                .settle_interrupted_resume(&unstarted, None)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::ApprovalConflict);
            assert_eq!(approval_state(&store, &unstarted), ApprovalState::Resuming);
        }
        assert!(store.settle_interrupted_resume(&unstarted, None).unwrap());
        assert_eq!(approval_state(&store, &unstarted), ApprovalState::Granted);
        assert!(!store.settle_interrupted_resume(&unstarted, None).unwrap());
    }
}
