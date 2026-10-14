//! Process records of a gateway connected to an independent Runner (P1R-C02). Each execution is
//! recorded in the operations database before it is sent to the Runner, and followed to its
//! end, so that a gateway that lost its memory can identify the same execution again. A record
//! never revives a handle, and never by itself proves that an execution started or how it
//! ended: the Runner is the authority on live processes.

use std::sync::Arc;

use codespace_domain::{
    ApprovalId, ErrorBody, ErrorCode, ExecCommandParams, ProcessId, ProcessState,
};
use codespace_runner::{
    Runner, RunnerIdentity, RunnerProcessRoot, RunnerProcessStatus, RuntimeBackend, ShellRelease,
};
use codespace_store::{
    NewProcessRecord, ProcessEnd, RecordedParticipation, RootProcess, RunnerRef, Store,
};
use tokio::sync::mpsc;

/// The exits that a gateway's release hook hands to its process records.
pub struct Exits(mpsc::UnboundedReceiver<ProcessId>);

/// What a gateway records of the executions it sends to one independent Runner.
pub struct ProcessRecords {
    store: Arc<Store>,
    runner: RunnerRef,
}

impl ProcessRecords {
    /// The release hook of a gateway connected to an independent Runner: it releases the lease
    /// of each process that exits, as every gateway's hook does, and hands the exit on to the
    /// records. An exit that comes before the records exist waits for them.
    pub fn release_hook(store: Arc<Store>) -> (ShellRelease, Exits) {
        let (exits, exited) = mpsc::unbounded_channel();
        let release: ShellRelease = Arc::new(move |process_id: &str| {
            store.release_process(process_id);
            let _ = exits.send(ProcessId(process_id.to_string()));
        });
        (release, Exits(exited))
    }

    /// The records of the executions sent to the Runner with `identity`, which from now on
    /// follow its exit events: each one is recorded as the Runner reports it.
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
        });
        let following = records.clone();
        let Exits(mut exited) = exits;
        tokio::spawn(async move {
            while let Some(process_id) = exited.recv().await {
                match runner.process_status(&process_id).await {
                    Ok(status) if status.state == ProcessState::Exited => following.ended(&status),
                    Ok(_) => {}
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
                }
            }
        });
        records
    }

    /// Record an execution before it is sent to the Runner, and mark the approval it resumes
    /// `resuming` with it. When the record cannot be written, nothing is sent.
    pub(crate) fn dispatching(
        &self,
        process_id: &ProcessId,
        params: &ExecCommandParams,
        participation: RecordedParticipation,
        attempt_id: Option<&str>,
        resume: Option<&ApprovalId>,
    ) -> Result<(), ErrorBody> {
        self.store
            .record_dispatch(&NewProcessRecord {
                process_id: &process_id.0,
                runner: &self.runner,
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

    /// The Runner answered that it did not start the execution.
    pub(crate) fn not_started(&self, process_id: &ProcessId) {
        self.end(process_id, &ProcessEnd::NotStarted);
    }

    /// The Runner reported the execution's status after it exited.
    pub(crate) fn ended(&self, status: &RunnerProcessStatus) {
        self.end(
            &status.process_id,
            &ProcessEnd::Ended {
                termination: status.termination,
                exit_code: status.exit_code,
                signal: status.signal.as_ref().map(|signal| signal.number),
                output_total: Some(status.output_total),
            },
        );
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

    fn end(&self, process_id: &ProcessId, end: &ProcessEnd) {
        self.write(
            process_id,
            "end",
            self.store.record_end(&self.runner, &process_id.0, end),
        );
    }

    /// A record that cannot be updated keeps what it said: the execution goes on, and the
    /// Runner remains the authority on it.
    fn write(&self, process_id: &ProcessId, what: &str, written: Result<bool, ErrorBody>) {
        if let Err(err) = written {
            tracing::warn!(
                process_id = %process_id.0,
                record = what,
                error = %err.message,
                "the process record was not updated"
            );
        }
    }
}
