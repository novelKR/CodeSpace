//! Process records of a gateway connected to an independent Runner (P1R-C02). Each execution is
//! recorded in the operations database before it is sent to the Runner, and followed to its
//! end, so that a gateway that lost its memory can identify the same execution again. A record
//! never revives a handle, and never by itself proves that an execution started or how it
//! ended: the Runner is the authority on live processes. Once a record says how an execution
//! ended, the gateway acknowledges it to the Runner, which until then keeps the ended process
//! for a controller (P1R-C04).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use codespace_domain::ProcessTermination;
use codespace_domain::{
    ApprovalId, ErrorBody, ErrorCode, ExecCommandParams, ProcessId, ProcessState,
};
use codespace_runner::{
    Runner, RunnerIdentity, RunnerInventoryEntry, RunnerProcessRoot, RunnerProcessStatus,
    RuntimeBackend, ShellRelease,
};
use codespace_store::{
    NewProcessRecord, ProcessEnd, RecordedParticipation, RootProcess, RunnerRef, Store,
};
use tokio::sync::{mpsc, watch};

/// The exits that a gateway's release hook hands to its process records.
pub struct Exits(mpsc::UnboundedReceiver<ProcessId>);

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
        });
        let following = records.clone();
        let Exits(mut exited) = exits;
        tokio::spawn(async move {
            while let Some(process_id) = exited.recv().await {
                let recorded = match runner.process_status(&process_id).await {
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
