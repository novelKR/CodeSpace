//! Process records of a gateway connected to an independent Runner (P1R-C02). Each execution is
//! recorded in the operations database before it is sent to the Runner, and followed to its
//! end, so that a gateway that lost its memory can identify the same execution again. A record
//! never revives a handle, and never by itself proves that an execution started or how it
//! ended: the Runner is the authority on live processes. Once a record says how an execution
//! ended, the gateway acknowledges it to the Runner, which until then keeps the ended process
//! for a controller (P1R-C04).
//!
//! When the Runner no longer holds a process, or the gateway lost the Runner, the records answer
//! what the Runner cannot, with why (P1R-C05): an execution whose end was recorded is reported
//! as it ended, and one whose Runner was lost before it ended as `unknown`, never as an end.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use codespace_domain::{
    ApprovalId, ErrorBody, ErrorCode, ExecCommandParams, LostReason, ProcessId, ProcessState,
    ProcessStatusResult, ProcessTermination, ReadProcessResult,
};
use codespace_runner::{
    process_signal, Runner, RunnerError, RunnerIdentity, RunnerInventoryEntry, RunnerProcessRoot,
    RunnerProcessStatus, RuntimeBackend, ShellRelease,
};
use codespace_store::{
    NewProcessRecord, ProcessEnd, ProcessLoss, ProcessOutcome, ProcessPhase, ProcessRecord,
    RecordedParticipation, RootProcess, RunnerRef, Store,
};
use tokio::sync::{mpsc, watch};

/// How long the records first wait before they ask the Runner again about an exit while the
/// gateway claims it again, and the longest they wait (P1R-C04).
const RECLAIMING_RETRY_START: Duration = Duration::from_millis(10);
const RECLAIMING_RETRY_MAX: Duration = Duration::from_millis(500);

/// The exits that a gateway's release hook hands to its process records.
pub struct Exits(mpsc::UnboundedReceiver<ProcessId>);

/// The Runner's status of `process_id`, which it reported exited (P1R-C04). The report can reach
/// the gateway while it claims the Runner again, as when the Runner sends it right after the
/// Claim's answer, and the request is then refused before it is sent: it is asked again once
/// the gateway has claimed the Runner, which keeps the ended process until its end is recorded.
async fn status_after_exit(
    runner: &RuntimeBackend,
    process_id: &ProcessId,
) -> Result<RunnerProcessStatus, RunnerError> {
    let mut retry = RECLAIMING_RETRY_START;
    loop {
        match runner.process_status(process_id).await {
            Err(err)
                if err
                    .as_execution()
                    .is_some_and(|body| body.code == ErrorCode::RunnerReconnecting) =>
            {
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(RECLAIMING_RETRY_MAX);
            }
            status => return status,
        }
    }
}

/// The workspace leases of a gateway connected to an independent Runner (P1R-C04): each exit
/// releases its process's lease, and a reconciliation holds a workspace again for a process
/// that it found running. While a reconciliation runs, the exits are noted, so a process that
/// exits after the Runner reported it running is never held again.
pub struct Leases {
    store: Arc<Store>,
    /// The processes that exited since the reconciliation in progress began, if one is.
    exited: Mutex<Option<HashSet<String>>>,
}

impl Leases {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        Arc::new(Self {
            store,
            exited: Mutex::new(None),
        })
    }

    fn exited(&self) -> std::sync::MutexGuard<'_, Option<HashSet<String>>> {
        self.exited.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Release the lease of `process_id`, which exited or never started.
    pub fn release(&self, process_id: &str) {
        let mut exited = self.exited();
        if let Some(exited) = exited.as_mut() {
            exited.insert(process_id.to_string());
        }
        self.store.release_process(process_id);
    }

    /// Note the exits from now on, for a reconciliation.
    pub(crate) fn note_exits(&self) {
        *self.exited() = Some(HashSet::new());
    }

    /// Stop noting the exits: the reconciliation ended.
    pub(crate) fn forget_exits(&self) {
        *self.exited() = None;
    }

    /// Hold `workspace_id` again for `process_id`, which the Runner reported running. Returns
    /// whether it is held: not when the process exited since, as its exit released it.
    pub(crate) fn restore(&self, workspace_id: &str, process_id: &str) -> Result<bool, ErrorBody> {
        let exited = self.exited();
        if exited
            .as_ref()
            .is_some_and(|exited| exited.contains(process_id))
        {
            return Ok(false);
        }
        self.store.hold_process(workspace_id, process_id)?;
        Ok(true)
    }
}

/// What a gateway records of the executions it sends to one independent Runner.
pub struct ProcessRecords {
    store: Arc<Store>,
    runner: RunnerRef,
    /// Bumped after each end that the records follow from an exit event (P1R-C04).
    ends: watch::Sender<u64>,
    /// Set once the gateway lost the Runner (P1R-C05).
    lost: AtomicBool,
}

impl ProcessRecords {
    /// The release hook of a gateway connected to an independent Runner: it releases the lease
    /// of each process that exits, as every gateway's hook does, and hands the exit on to the
    /// records. An exit that comes before the records exist waits for them.
    pub fn release_hook(leases: Arc<Leases>) -> (ShellRelease, Exits) {
        let (exits, exited) = mpsc::unbounded_channel();
        let release: ShellRelease = Arc::new(move |process_id: &str| {
            leases.release(process_id);
            let _ = exits.send(ProcessId(process_id.to_string()));
        });
        (release, Exits(exited))
    }

    /// The records of the executions sent to the Runner with `identity`, which from now on
    /// follow its exit events: each one is recorded as the Runner reports it, then acknowledged
    /// to the Runner (P1R-C04).
    pub fn start(
        store: Arc<Store>,
        identity: &RunnerIdentity,
        runner: RuntimeBackend,
        exits: Exits,
    ) -> Arc<Self> {
        let records = Arc::new(Self {
            store,
            runner: RunnerRef {
                boot_id: identity.boot_id.clone(),
                pid: identity.pid,
                started_at_ms: identity.started_at_ms,
                nonce: identity.nonce.clone(),
            },
            ends: watch::Sender::new(0),
            lost: AtomicBool::new(false),
        });
        let following = records.clone();
        let Exits(mut exited) = exits;
        tokio::spawn(async move {
            while let Some(process_id) = exited.recv().await {
                let recorded = match status_after_exit(&runner, &process_id).await {
                    Ok(status) if status.state == ProcessState::Exited => following.ended(&status),
                    Ok(_) => false,
                    // The Runner reported the exit; how it ended is not known.
                    Err(_) => following.end(
                        &process_id,
                        &ProcessEnd::Ended {
                            termination: None,
                            exit_code: None,
                            signal: None,
                            output_total: None,
                        },
                    ),
                };
                if recorded {
                    // The Runner keeps an ended process for a controller until it is told.
                    if let Err(err) = runner.acknowledge(std::slice::from_ref(&process_id)).await {
                        tracing::debug!(
                            process_id = %process_id.0,
                            error = %err,
                            "the Runner was not told that the process's end is recorded"
                        );
                    }
                }
                following.ends.send_modify(|ends| *ends += 1);
            }
        });
        records
    }

    /// Follow the loss of the Runner these records are of (P1R-C05): once `lost` says why the
    /// gateway lost it, see [`Self::runner_lost`].
    pub fn follow_loss(self: &Arc<Self>, mut lost: watch::Receiver<Option<String>>) {
        let records = self.clone();
        tokio::spawn(async move {
            loop {
                if let Some(reason) = lost.borrow_and_update().clone() {
                    records.runner_lost(&reason);
                    return;
                }
                if lost.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// The gateway lost the Runner these records are of, for `reason` (P1R-C05): what became of
    /// each execution sent to it that had not ended cannot be learned any more, and some may
    /// still run, so each of their records becomes `unknown` and keeps its workspace held until
    /// an operator releases it. An end the Runner reported before still ends its record.
    pub(crate) fn runner_lost(&self, reason: &str) {
        self.lost.store(true, Ordering::SeqCst);
        match self
            .store
            .record_runner_lost(&self.runner, ProcessLoss::RunnerLost)
        {
            Ok(processes) => tracing::warn!(
                ?processes,
                reason,
                "the independent Runner was lost before these processes ended: what became of \
                 them cannot be learned, so their records say unknown and their workspaces stay \
                 held until an operator finds them and releases them"
            ),
            Err(err) => tracing::error!(
                reason,
                error = %err.message,
                "the independent Runner was lost, and the records of its processes could not say \
                 so: their workspaces stay held"
            ),
        }
    }

    /// The record to answer from about `process_id`, which the Runner did not answer about but
    /// with `err` (P1R-C05): when it no longer holds the process, or the gateway lost it, and
    /// the record says that the execution was sent. None when the Runner remains the one to
    /// ask, as for an execution of its own that has not ended as far as its record says.
    pub(crate) fn recorded(&self, process_id: &ProcessId, err: &RunnerError) -> Option<Recorded> {
        let lost_runner = match err.as_execution().map(|body| body.code) {
            Some(ErrorCode::ProcessNotFound) => false,
            Some(ErrorCode::RunnerDisconnected) if self.lost.load(Ordering::SeqCst) => true,
            _ => return None,
        };
        let record = match self.store.process_record(&process_id.0) {
            Ok(Some(record)) => record,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(
                    process_id = %process_id.0,
                    error = %err.message,
                    "the process record could not be read"
                );
                return None;
            }
        };
        if record.outcome == Some(ProcessOutcome::NotStarted) {
            return None;
        }
        let ours = record.runner == self.runner;
        let loss_since = || ProcessLoss::of(&record.runner, self.runner.boot_id.as_deref());
        let reason = match (record.lost, record.phase) {
            (Some(loss), _) => lost_reason(loss),
            (None, ProcessPhase::Terminal) if ours && lost_runner => LostReason::RunnerLost,
            // Its end is recorded: only what the Runner retained is gone.
            (None, ProcessPhase::Terminal) => match loss_since() {
                ProcessLoss::HostRebooted => LostReason::HostRebooted,
                ProcessLoss::RunnerLost => LostReason::RetentionExpired,
            },
            (None, _) if ours && lost_runner => LostReason::RunnerLost,
            (None, _) if ours => return None,
            // Another Runner answers at its Runner's socket.
            (None, _) => lost_reason(loss_since()),
        };
        Some(Recorded { record, reason })
    }

    /// The Runner these records are of.
    pub(crate) fn runner(&self) -> &RunnerRef {
        &self.runner
    }

    /// Each end that the records follow from an exit event, as a count (P1R-C04).
    pub(crate) fn ends(&self) -> watch::Receiver<u64> {
        self.ends.subscribe()
    }

    /// Record an execution before it is sent to the Runner under the controller epoch
    /// `controller_epoch` (P1R-C03), and mark the approval it resumes `resuming` with it. When
    /// the record cannot be written, nothing is sent.
    pub(crate) fn dispatching(
        &self,
        process_id: &ProcessId,
        params: &ExecCommandParams,
        controller_epoch: Option<u64>,
        participation: RecordedParticipation,
        attempt_id: Option<&str>,
        resume: Option<&ApprovalId>,
    ) -> Result<(), ErrorBody> {
        self.store
            .record_dispatch(&NewProcessRecord {
                process_id: &process_id.0,
                runner: &self.runner,
                controller_epoch,
                workspace_id: &params.workspace_id.0,
                approval_id: resume,
                attempt_id,
                participation,
                tty: params.tty,
                argv: &params.command,
            })
            .map_err(|err| match err.code {
                ErrorCode::Internal => ErrorBody::new(
                    ErrorCode::Internal,
                    format!(
                        "the execution could not be recorded before it was sent, so nothing was \
                         started: {}",
                        err.message
                    ),
                ),
                _ => err,
            })
    }

    /// The Runner answered that it started the execution.
    pub(crate) fn running(
        &self,
        process_id: &ProcessId,
        root: Option<&RunnerProcessRoot>,
        deadline_at_ms: Option<u64>,
    ) {
        let root = root.map(|root| RootProcess {
            pid: root.pid,
            started: root.started.clone(),
        });
        self.write(
            process_id,
            "running",
            self.store
                .record_running(&self.runner, &process_id.0, root.as_ref(), deadline_at_ms),
        );
    }

    /// The Runner answered that it did not start the execution, or proved it when the gateway
    /// reconciled (P1R-C04). Returns whether the record could be written.
    pub(crate) fn not_started(&self, process_id: &ProcessId) -> bool {
        self.end(process_id, &ProcessEnd::NotStarted)
    }

    /// The Runner reported the execution's status after it exited. Returns whether the record
    /// could be written.
    pub(crate) fn ended(&self, status: &RunnerProcessStatus) -> bool {
        self.end(
            &status.process_id,
            &ProcessEnd::Ended {
                termination: status.termination,
                exit_code: status.exit_code,
                signal: status.signal.as_ref().map(|signal| signal.number),
                output_total: Some(status.output_total),
            },
        )
    }

    /// The Runner created the execution and no longer keeps it: it ended, and how is not known
    /// any more (P1R-C04). Returns whether the record could be written.
    pub(crate) fn dropped(&self, process_id: &ProcessId) -> bool {
        self.end(
            process_id,
            &ProcessEnd::Ended {
                termination: Some(ProcessTermination::Unknown),
                exit_code: None,
                signal: None,
                output_total: None,
            },
        )
    }

    /// A reconciliation found the execution running on the Runner (P1R-C04). Returns whether
    /// the record could be written.
    pub(crate) fn found_running(&self, process_id: &ProcessId) -> bool {
        self.write(
            process_id,
            "running",
            self.store
                .record_running(&self.runner, &process_id.0, None, None),
        )
    }

    /// The Runner stopped, and reported how each process it held ended (P1R-C04).
    pub fn stopped(&self, processes: &[RunnerInventoryEntry]) {
        for entry in processes {
            if entry.status.state == ProcessState::Exited {
                self.ended(&entry.status);
            }
        }
    }

    /// A gateway observed the execution's output up to `cursor`.
    pub(crate) fn observed_output(&self, process_id: &ProcessId, cursor: u64) {
        self.write(
            process_id,
            "output",
            self.store
                .record_output(&self.runner, &process_id.0, cursor),
        );
    }

    fn end(&self, process_id: &ProcessId, end: &ProcessEnd) -> bool {
        self.write(
            process_id,
            "end",
            self.store.record_end(&self.runner, &process_id.0, end),
        )
    }

    /// A record that cannot be updated keeps what it said: the execution goes on, and the
    /// Runner remains the authority on it. Returns whether nothing failed.
    fn write(&self, process_id: &ProcessId, what: &str, written: Result<bool, ErrorBody>) -> bool {
        match written {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    process_id = %process_id.0,
                    record = what,
                    error = %err.message,
                    "the process record was not updated"
                );
                false
            }
        }
    }
}

/// An execution as its record tells it, when its Runner cannot (P1R-C05).
pub(crate) struct Recorded {
    record: ProcessRecord,
    reason: LostReason,
}

impl Recorded {
    /// The furthest output offset the gateway knows of.
    fn output_total(&self) -> u64 {
        self.record.output_cursor.unwrap_or(0)
    }

    /// The execution's status: as its record says it ended, with nothing of its output
    /// retained. One whose Runner was lost before it ended is `unknown`, never an end; after
    /// a host restart, it ended, and how is not known.
    pub(crate) fn status(&self) -> ProcessStatusResult {
        let record = &self.record;
        let unknown_end = (
            ProcessState::Exited,
            Some(ProcessTermination::Unknown),
            None,
            None,
        );
        let (state, termination, exit_code, signal) = match (record.phase, record.outcome) {
            (ProcessPhase::Terminal, Some(ProcessOutcome::Ended(termination))) => match termination
            {
                ProcessTermination::Exited => (
                    ProcessState::Exited,
                    Some(ProcessTermination::Exited),
                    record.exit_code,
                    None,
                ),
                ProcessTermination::Signaled => match record.signal {
                    Some(number) => (
                        ProcessState::Exited,
                        Some(ProcessTermination::Signaled),
                        None,
                        Some(process_signal(number)),
                    ),
                    None => unknown_end,
                },
                termination => (ProcessState::Exited, Some(termination), None, None),
            },
            (ProcessPhase::Terminal, _) => unknown_end,
            _ if self.reason == LostReason::HostRebooted => unknown_end,
            _ => (ProcessState::Unknown, None, None, None),
        };
        ProcessStatusResult {
            process_id: ProcessId(record.process_id.clone()),
            state,
            exit_code,
            termination,
            signal,
            output_total: self.output_total(),
            output_retained_from: self.output_total(),
            eof: true,
            lost_reason: Some(self.reason),
            coordination: None,
        }
    }

    /// A read from `cursor`: nothing of the output is retained, and nothing more will be.
    pub(crate) fn read(&self, cursor: u64) -> ReadProcessResult {
        ReadProcessResult {
            process_id: ProcessId(self.record.process_id.clone()),
            cursor: cursor.max(self.output_total()),
            chunk: String::new(),
            eof: true,
            output_lost: true,
            retained_from: self.output_total(),
            lost_reason: Some(self.reason),
            coordination: None,
        }
    }
}

fn lost_reason(loss: ProcessLoss) -> LostReason {
    match loss {
        ProcessLoss::RunnerLost => LostReason::RunnerLost,
        ProcessLoss::HostRebooted => LostReason::HostRebooted,
    }
}
