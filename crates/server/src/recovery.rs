//! Reconciliation of a gateway with its independent Runner after each Claim (P1R-C04).
//!
//! A gateway that claims an independent Runner, when it starts or again after it lost its
//! connection, compares three accounts of what earlier claims left: its process records, what
//! the Runner holds and remembers of each process it was asked to start, and, for an execution
//! that takes part in the resource authority, the authority's attempt, which the Runner looks
//! up. Until a workspace is reconciled, the gateway and the Runner both refuse new executions,
//! patches and approval resumes there with `RECOVERY_PENDING`; the processes the gateway
//! inherits can always be queried, read, written to, resized and terminated.
//!
//! | Record | The record's Runner | Attempt (`required` only) | Outcome |
//! | --- | --- | --- | --- |
//! | dispatching or running | holds it running | a helper claimed it | the workspace is held for the process again |
//! | dispatching or running | ended it, held or dropped | settled | recorded as ended, and its lease released |
//! | dispatching | never received it, or created nothing | none, or settled before it started | recorded as not started, and its approval given back |
//! | none | holds it running | any | the workspace stays blocked |
//! | anything else | | | the workspace stays blocked: a failed lookup is asked again, a mismatch waits for an operator |
//!
//! A Runner proves that it never received a request only for itself. A record of an execution
//! sent to another Runner that has not ended means that Runner was lost, since another one
//! answers at its socket (P1R-C05): the record becomes `unknown`, with why, and like every
//! record whose Runner was lost it keeps its workspace blocked until an operator releases it
//! (D8). An approval left `resuming` is consumed with the process the Runner created for it,
//! given back when the Runner proved that it created none, and otherwise left as it is. A
//! workspace that waits is reconciled again after each exit, and one that waits for an answer
//! also after a growing pause.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use codespace_domain::{
    ApprovalId, ApprovalState, ErrorBody, ErrorCode, ExecCommandResult, ExecDispatchStatus,
    OperationResumeResult, ProcessId, ProcessState,
};
use codespace_runner::{Recalled, RunnerClaim, RunnerProcessStatus, UdsRunner};
use codespace_store::{
    ProcessLoss, ProcessOutcome, ProcessPhase, ProcessRecord, RecordedParticipation, RunnerRef,
    Store,
};
use tokio::sync::watch;

use crate::records::{Leases, ProcessRecords};

/// The pause before a reconciliation that left a workspace waiting for an answer is tried
/// again, doubled after each further try up to [`RETRY_MAX`].
const RETRY_START: Duration = Duration::from_millis(500);
/// The longest pause between two tries.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Why nothing new starts in a workspace yet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bar {
    /// What its reconciliation needs is not known yet: it is tried again.
    Pending(String),
    /// The records, the Runner and the resource authority do not agree, or this gateway cannot
    /// ask about it: an operator decides.
    Blocked(String),
}

/// What new work waits for.
#[derive(Default)]
struct Gate {
    /// The controller epoch of the last Claim taken into account.
    claimed: Option<u64>,
    /// Why the process records could not be read, if they could not: nothing new starts.
    unreadable: Option<String>,
    /// The workspaces where nothing new starts yet, and why.
    barred: BTreeMap<String, Bar>,
}

/// What a reconciliation left.
#[derive(Debug, Default, Clone, Copy)]
struct Outcome {
    /// Something waits for an answer that may come later.
    pending: bool,
    /// Some workspace waits.
    barred: bool,
}

/// The reconciliation of a gateway with the independent Runner it claimed (P1R-C04).
pub struct Recovery {
    store: Arc<Store>,
    runner: UdsRunner,
    records: Arc<ProcessRecords>,
    leases: Arc<Leases>,
    gate: Mutex<Gate>,
    /// The number of reconciliations so far.
    passes: watch::Sender<u64>,
}

impl Recovery {
    /// Reconcile what earlier claims of `runner` left, from its Claim `claim` on, and again
    /// after each later Claim. Until a workspace is reconciled, [`Self::admit`] refuses new work
    /// there.
    pub fn start(
        store: Arc<Store>,
        runner: UdsRunner,
        records: Arc<ProcessRecords>,
        leases: Arc<Leases>,
        claim: &RunnerClaim,
    ) -> Arc<Self> {
        let recovery = Arc::new(Self {
            store,
            runner,
            records,
            leases,
            gate: Mutex::new(Gate::default()),
            passes: watch::Sender::new(0),
        });
        recovery.take_account(claim);
        tokio::spawn(run(
            Arc::downgrade(&recovery),
            recovery.runner.claims(),
            recovery.records.ends(),
        ));
        recovery
    }

    fn gate(&self) -> MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether new work starts in `workspace_id` under the controller epoch `epoch`: refused
    /// with `RECOVERY_PENDING` until the gateway reconciled that workspace after the Claim of
    /// `epoch`. Work under no epoch, for a worker that is not claimed, is not reconciled.
    pub fn admit(&self, workspace_id: &str, epoch: Option<u64>) -> Result<(), ErrorBody> {
        let Some(epoch) = epoch else {
            return Ok(());
        };
        if self.gate().claimed.is_none_or(|claimed| claimed < epoch) {
            if let Some(claim) = self.runner.claimed().filter(|claim| claim.epoch == epoch) {
                self.take_account(&claim);
            }
        }
        let gate = self.gate();
        if gate.claimed != Some(epoch) {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryPending,
                "this gateway claimed its independent Runner again and has not taken account of \
                 that Claim yet, so nothing new is started; its processes can be queried, read, \
                 written to, resized and terminated. Retry shortly",
            ));
        }
        if let Some(reason) = &gate.unreadable {
            return Err(ErrorBody::new(
                ErrorCode::RecoveryPending,
                format!(
                    "this gateway could not read its process records to reconcile them with its \
                     independent Runner ({reason}), so nothing new is started; its processes can \
                     be queried, read, written to, resized and terminated, and it tries again by \
                     itself"
                ),
            ));
        }
        match gate.barred.get(workspace_id) {
            None => Ok(()),
            Some(Bar::Pending(reason)) => Err(ErrorBody::new(
                ErrorCode::RecoveryPending,
                format!(
                    "nothing new is started in workspace `{workspace_id}` until this gateway \
                     reconciled it with its independent Runner: {reason}. Its processes can be \
                     queried, read, written to, resized and terminated, and the gateway tries \
                     again by itself"
                ),
            )),
            Some(Bar::Blocked(reason)) => Err(ErrorBody::new(
                ErrorCode::RecoveryPending,
                format!(
                    "nothing new is started in workspace `{workspace_id}`: {reason}, so this \
                     gateway cannot reconcile it with its independent Runner until an operator \
                     resolves it. Its processes can be queried, read, written to, resized and \
                     terminated"
                ),
            )),
        }
    }

    /// The number of reconciliations so far, as each one ends. For tests.
    #[doc(hidden)]
    pub fn passes(&self) -> watch::Receiver<u64> {
        self.passes.subscribe()
    }

    /// Take account of the Claim `claim`, once: until it is reconciled, nothing new starts in a
    /// workspace where an earlier controller started work, where the Runner holds a running
    /// process, or that a record left to reconcile names.
    fn take_account(&self, claim: &RunnerClaim) {
        if self
            .gate()
            .claimed
            .is_some_and(|claimed| claimed >= claim.epoch)
        {
            return;
        }
        let waiting = Bar::Pending("its reconciliation after the Claim has not finished".into());
        let mut barred: BTreeMap<String, Bar> = claim
            .barrier
            .iter()
            .map(|workspace_id| (workspace_id.clone(), waiting.clone()))
            .collect();
        for entry in &claim.processes {
            if entry.status.state == ProcessState::Running {
                barred.insert(entry.workspace_id.clone(), waiting.clone());
            }
        }
        let unreadable = match self
            .store
            .records_to_reconcile(self.records.runner(), claim.epoch)
        {
            Ok(records) => {
                for record in records {
                    barred.insert(record.workspace_id, waiting.clone());
                }
                None
            }
            Err(err) => Some(err.message),
        };
        let mut gate = self.gate();
        if gate.claimed.is_some_and(|claimed| claimed >= claim.epoch) {
            return;
        }
        *gate = Gate {
            claimed: Some(claim.epoch),
            unreadable,
            barred,
        };
    }

    /// Reconcile what the claims before `claim` left, while `claim` is the current one.
    async fn pass(&self, claim: &RunnerClaim) -> Outcome {
        let epoch = claim.epoch;
        if self.runner.claimed().map(|claimed| claimed.epoch) != Some(epoch) {
            // The connection is lost or was claimed again: a later Claim is reconciled.
            return Outcome {
                pending: true,
                barred: true,
            };
        }
        let records = match self
            .store
            .records_to_reconcile(self.records.runner(), epoch)
        {
            Ok(records) => records,
            Err(err) => {
                let mut gate = self.gate();
                if gate.claimed == Some(epoch) {
                    gate.unreadable = Some(err.message);
                }
                return Outcome {
                    pending: true,
                    barred: true,
                };
            }
        };
        // From before the Runner is asked: a process that exits after it was reported running
        // is never held again.
        self.leases.note_exits();
        let bars = self
            .reconcile(&self.runner.at_epoch(epoch), claim, &records)
            .await;
        self.leases.forget_exits();
        let outcome = Outcome {
            pending: bars.values().any(|bar| matches!(bar, Bar::Pending(_))),
            barred: !bars.is_empty(),
        };
        let mut gate = self.gate();
        if gate.claimed == Some(epoch) {
            log_changes(&gate.barred, &bars);
            gate.barred = bars;
            gate.unreadable = None;
        }
        outcome
    }

    /// Reconcile `records` with what `runner` holds and remembers, and with the resource
    /// authority; then tell the Runner which workspaces of `claim`'s barrier are reconciled.
    /// Returns the workspaces where nothing new starts yet.
    async fn reconcile(
        &self,
        runner: &UdsRunner,
        claim: &RunnerClaim,
        records: &[ProcessRecord],
    ) -> BTreeMap<String, Bar> {
        let mut bars = Bars::default();
        let ours = self.records.runner();
        let lost = self.lose_other_runners(records);
        let mut decisions: Vec<Option<Decision>> = Vec::with_capacity(records.len());
        let mut asked = Vec::new();
        for (index, record) in records.iter().enumerate() {
            decisions.push(match record.phase {
                ProcessPhase::Terminal => Some(Decision::Settled {
                    started: record.outcome != Some(ProcessOutcome::NotStarted),
                }),
                ProcessPhase::Unknown => Some(Decision::Blocked(awaits_release(
                    record,
                    record.lost.unwrap_or(ProcessLoss::RunnerLost),
                ))),
                // Only the Runner that was sent it can prove what became of it, and that Runner
                // is lost (P1R-C05).
                _ if record.runner != *ours => Some(match lost.get(&record.process_id) {
                    Some(Ok(loss)) => Decision::Blocked(awaits_release(record, *loss)),
                    Some(Err(err)) => Decision::Pending(format!(
                        "the Runner that process `{}` was sent to is lost, and its record could \
                         not say so: {err}",
                        record.process_id
                    )),
                    None => Decision::Pending(format!(
                        "the record of process `{}` was not marked lost",
                        record.process_id
                    )),
                }),
                ProcessPhase::Dispatching | ProcessPhase::Running => {
                    asked.push(index);
                    None
                }
            });
        }
        // A running process that no record left to reconcile accounts for.
        let accounted: HashSet<&str> = records
            .iter()
            .filter(|record| record.phase != ProcessPhase::Terminal)
            .map(|record| record.process_id.as_str())
            .collect();
        let suspects: Vec<_> = claim
            .processes
            .iter()
            .filter(|entry| {
                entry.status.state == ProcessState::Running
                    && !accounted.contains(entry.status.process_id.0.as_str())
            })
            .collect();
        let mut ids: Vec<ProcessId> = asked
            .iter()
            .map(|&index| ProcessId(records[index].process_id.clone()))
            .collect();
        ids.extend(suspects.iter().map(|entry| entry.status.process_id.clone()));
        let recalled: Result<HashMap<String, Recalled>, String> = if ids.is_empty() {
            Ok(HashMap::new())
        } else {
            runner
                .recall(&ids)
                .await
                .map(|recalled| {
                    recalled
                        .into_iter()
                        .map(|process| (process.process_id.0, process.recalled))
                        .collect()
                })
                .map_err(|err| err.to_string())
        };
        match &recalled {
            Ok(remembered) => {
                let attempts = look_up(runner, records, &asked).await;
                for &index in &asked {
                    let record = &records[index];
                    decisions[index] = Some(match remembered.get(&record.process_id) {
                        None => Decision::Pending(format!(
                            "the Runner did not answer about process `{}`",
                            record.process_id
                        )),
                        Some(recalled) => match record.participation {
                            RecordedParticipation::Off => decide(record, recalled),
                            RecordedParticipation::Required => {
                                decide_required(record, recalled, &attempts)
                            }
                        },
                    });
                }
            }
            Err(err) => {
                for &index in &asked {
                    decisions[index] = Some(Decision::Pending(format!(
                        "the Runner could not be asked about process `{}`: {err}",
                        records[index].process_id
                    )));
                }
            }
        }

        // The executions of each approval being resumed, oldest first, with its workspace.
        let mut resumed: BTreeMap<&str, (&ApprovalId, &str, Vec<Link>)> = BTreeMap::new();
        let mut acknowledged = Vec::new();
        for (record, decision) in records.iter().zip(decisions) {
            let decision = decision.expect("every record is decided");
            let link = self.apply(record, decision, &mut bars, &mut acknowledged);
            if let Some(approval_id) = &record.approval_id {
                resumed
                    .entry(&approval_id.0)
                    .or_insert_with(|| (approval_id, &record.workspace_id, Vec::new()))
                    .2
                    .push(link);
            }
        }
        let reconciled: HashSet<&str> = records
            .iter()
            .map(|record| record.process_id.as_str())
            .collect();
        for (approval_id, workspace_id, links) in resumed.into_values() {
            if let Err(err) = self.settle(approval_id, &links, &reconciled) {
                bars.pend(
                    workspace_id,
                    format!(
                        "approval `{}` could not be settled: {}",
                        approval_id.0, err.message
                    ),
                );
            }
        }

        for entry in &suspects {
            let process_id = &entry.status.process_id;
            match &recalled {
                Ok(remembered) => match remembered.get(&process_id.0) {
                    Some(Recalled::Held {
                        workspace_id,
                        status,
                    }) if status.state == ProcessState::Running => bars.block(
                        workspace_id,
                        format!(
                            "process `{}` runs there, and no record of this gateway accounts \
                             for it",
                            process_id.0
                        ),
                    ),
                    // It ended: nothing of it is left to record.
                    Some(Recalled::Held { .. }) => acknowledged.push(process_id.clone()),
                    _ => {}
                },
                Err(err) => bars.pend(
                    &entry.workspace_id,
                    format!(
                        "the Runner could not be asked about process `{}`: {err}",
                        process_id.0
                    ),
                ),
            }
        }
        // Ended processes the Runner keeps although their records say how they ended, or have
        // none: an acknowledgement of an earlier gateway that was lost.
        acknowledged.extend(
            claim
                .processes
                .iter()
                .filter(|entry| {
                    entry.status.state == ProcessState::Exited
                        && !accounted.contains(entry.status.process_id.0.as_str())
                })
                .map(|entry| entry.status.process_id.clone()),
        );

        let reconciled: Vec<String> = claim
            .barrier
            .iter()
            .filter(|workspace_id| !bars.0.contains_key(*workspace_id))
            .cloned()
            .collect();
        if !reconciled.is_empty() {
            if let Err(err) = runner.reconciled(&reconciled).await {
                for workspace_id in &reconciled {
                    bars.pend(
                        workspace_id,
                        format!("the Runner could not be told that it is reconciled: {err}"),
                    );
                }
            }
        }
        if !acknowledged.is_empty() {
            if let Err(err) = runner.acknowledge(&acknowledged).await {
                tracing::debug!(
                    error = %err,
                    "the Runner was not told that the ends of the processes it keeps are \
                     recorded"
                );
            }
        }
        bars.0
    }

    /// The Runners other than this gateway's that `records` sent executions to which have not
    /// ended are lost: another Runner answers at their socket (P1R-C05). Each of their records
    /// that had not ended becomes `unknown`, for a restart of the host when the lost Runner ran
    /// on an earlier boot. Returns, for each such record, its loss, or why it could not be
    /// recorded.
    fn lose_other_runners(
        &self,
        records: &[ProcessRecord],
    ) -> HashMap<String, Result<ProcessLoss, String>> {
        let ours = self.records.runner();
        let mut runners: Vec<&RunnerRef> = Vec::new();
        for record in records {
            if matches!(
                record.phase,
                ProcessPhase::Dispatching | ProcessPhase::Running
            ) && record.runner != *ours
                && !runners.contains(&&record.runner)
            {
                runners.push(&record.runner);
            }
        }
        let mut lost = HashMap::new();
        for runner in runners {
            let loss = ProcessLoss::of(runner, ours.boot_id.as_deref());
            let marked = self
                .store
                .record_runner_lost(runner, loss)
                .map_err(|err| err.message);
            if let Ok(processes) = &marked {
                if !processes.is_empty() {
                    tracing::warn!(
                        runner_pid = runner.pid,
                        loss = loss.as_str(),
                        ?processes,
                        "an earlier independent Runner was lost before these processes ended: \
                         another Runner answers at its socket, so what became of them cannot be \
                         learned; their workspaces stay blocked until an operator releases them"
                    );
                }
            }
            for record in records.iter().filter(|record| record.runner == *runner) {
                lost.insert(record.process_id.clone(), marked.clone().map(|_| loss));
            }
        }
        lost
    }

    /// Act on `decision` for `record`, and return what it says of the execution's start.
    fn apply(
        &self,
        record: &ProcessRecord,
        decision: Decision,
        bars: &mut Bars,
        acknowledged: &mut Vec<ProcessId>,
    ) -> Link {
        let process_id = ProcessId(record.process_id.clone());
        let unwritten = || {
            format!(
                "the record of process `{}` could not be updated",
                record.process_id
            )
        };
        match decision {
            Decision::Running => {
                if !self.records.found_running(&process_id) {
                    bars.pend(&record.workspace_id, unwritten());
                }
                if let Err(err) = self
                    .leases
                    .restore(&record.workspace_id, &record.process_id)
                {
                    bars.pend(
                        &record.workspace_id,
                        format!(
                            "process `{}` runs there, and the workspace could not be held for \
                             it: {}",
                            record.process_id, err.message
                        ),
                    );
                }
                Link::Started(process_id)
            }
            Decision::Ended(status) => {
                let written = match &status {
                    Some(status) => self.records.ended(status),
                    None => self.records.dropped(&process_id),
                };
                self.leases.release(&record.process_id);
                if !written {
                    bars.pend(&record.workspace_id, unwritten());
                } else if status.is_some() {
                    acknowledged.push(process_id.clone());
                }
                Link::Started(process_id)
            }
            Decision::NotStarted => {
                let written = self.records.not_started(&process_id);
                self.leases.release(&record.process_id);
                if written {
                    Link::NotStarted
                } else {
                    bars.pend(&record.workspace_id, unwritten());
                    Link::Unknown
                }
            }
            Decision::Settled { started: true } => Link::Started(process_id),
            Decision::Settled { started: false } => Link::NotStarted,
            Decision::Pending(reason) => {
                bars.pend(&record.workspace_id, reason);
                Link::Unknown
            }
            Decision::Blocked(reason) => {
                bars.block(&record.workspace_id, reason);
                Link::Unknown
            }
        }
    }

    /// Settle the approval `approval_id`, which its executions in `links` resumed: consumed
    /// with the first one that started, given back when none did, and left as it is when
    /// whether one started is not known. An approval resumed by an execution that this
    /// reconciliation did not take up, one under the current Claim, is left to that resume.
    fn settle(
        &self,
        approval_id: &ApprovalId,
        links: &[Link],
        reconciled: &HashSet<&str>,
    ) -> Result<(), ErrorBody> {
        if self
            .store
            .records_of_approval(approval_id)?
            .iter()
            .any(|record| !reconciled.contains(record.process_id.as_str()))
        {
            return Ok(());
        }
        let started = links.iter().find_map(|link| match link {
            Link::Started(process_id) => Some(process_id),
            _ => None,
        });
        match started {
            Some(process_id) => {
                let consumed = consumed_by(approval_id, process_id);
                self.store
                    .settle_interrupted_resume(approval_id, Some(&consumed))?;
            }
            None if links.iter().all(|link| matches!(link, Link::NotStarted)) => {
                self.store.settle_interrupted_resume(approval_id, None)?;
            }
            None => {}
        }
        Ok(())
    }
}

/// Reconcile after each Claim of the runner whose Claims `claims` reports, and again while a
/// workspace waits: after each end that `ends` counts, and after a growing pause while one
/// waits for an answer. Ends with the reconciliation.
async fn run(
    recovery: Weak<Recovery>,
    mut claims: watch::Receiver<Option<RunnerClaim>>,
    mut ends: watch::Receiver<u64>,
) {
    let mut claim = claims.borrow_and_update().clone();
    let mut retry = RETRY_START;
    loop {
        ends.borrow_and_update();
        let outcome = match &claim {
            Some(claim) => {
                let Some(recovery) = recovery.upgrade() else {
                    return;
                };
                recovery.take_account(claim);
                let outcome = recovery.pass(claim).await;
                recovery.passes.send_modify(|passes| *passes += 1);
                outcome
            }
            None => Outcome::default(),
        };
        tokio::select! {
            changed = claims.changed() => {
                if changed.is_err() {
                    return;
                }
                let next = claims.borrow_and_update().clone();
                if next.as_ref().map(|next| next.epoch) > claim.as_ref().map(|claim| claim.epoch) {
                    claim = next;
                    retry = RETRY_START;
                }
            }
            changed = ends.changed(), if outcome.barred => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(retry), if outcome.pending => {
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
    }
}

/// What a reconciliation learned of an execution.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    /// The Runner holds it running: its workspace is held for it again.
    Running,
    /// The Runner created it, and it ended: with this status while the Runner holds it.
    Ended(Option<RunnerProcessStatus>),
    /// The Runner proved that it never created it, and the resource authority agrees.
    NotStarted,
    /// Its record already says how it ended, or that it never started.
    Settled { started: bool },
    /// What became of it is not known yet.
    Pending(String),
    /// The record, the Runner and the resource authority do not agree, or this gateway cannot
    /// ask about it.
    Blocked(String),
}

/// What a reconciliation learned of an approval's execution.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    /// The Runner created this process for it.
    Started(ProcessId),
    /// It never started.
    NotStarted,
    /// Whether it started is not known.
    Unknown,
}

/// The workspaces where nothing new starts yet: one an operator decides on stays so whatever
/// else waits there.
#[derive(Default)]
struct Bars(BTreeMap<String, Bar>);

impl Bars {
    fn pend(&mut self, workspace_id: &str, reason: String) {
        self.0
            .entry(workspace_id.to_string())
            .or_insert(Bar::Pending(reason));
    }

    fn block(&mut self, workspace_id: &str, reason: String) {
        match self.0.get(workspace_id) {
            Some(Bar::Blocked(_)) => {}
            _ => {
                self.0
                    .insert(workspace_id.to_string(), Bar::Blocked(reason));
            }
        }
    }
}

/// Why `record`, whose Runner was lost for `loss`, keeps its workspace blocked until an
/// operator releases it (P1R-C05, D8).
fn awaits_release(record: &ProcessRecord, loss: ProcessLoss) -> String {
    let lost = match loss {
        ProcessLoss::RunnerLost => {
            "was sent to a Runner that was lost, so what became of it cannot be learned and it \
             may still run"
        }
        ProcessLoss::HostRebooted => "was sent to a Runner that the host's restart ended",
    };
    format!(
        "process `{}` {lost}; an operator confirms that nothing of it runs and releases its \
         record (`codespace-mcp records`)",
        record.process_id
    )
}

/// What became of an execution that takes no part in the resource authority, as its Runner
/// recalled it.
fn decide(record: &ProcessRecord, recalled: &Recalled) -> Decision {
    match recalled {
        Recalled::Held { workspace_id, .. } if *workspace_id != record.workspace_id => {
            Decision::Blocked(format!(
                "the Runner holds process `{}` in workspace `{workspace_id}`, not where its \
                 record says",
                record.process_id
            ))
        }
        Recalled::Held { status, .. } if status.state == ProcessState::Running => Decision::Running,
        Recalled::Held { status, .. } => Decision::Ended(Some(status.clone())),
        Recalled::Dropped => Decision::Ended(None),
        Recalled::NotReceived | Recalled::Refused if record.phase == ProcessPhase::Dispatching => {
            Decision::NotStarted
        }
        Recalled::NotReceived | Recalled::Refused => Decision::Blocked(format!(
            "the record of process `{}` says the Runner started it, and the Runner {}",
            record.process_id,
            recalled_as(recalled)
        )),
    }
}

/// How the Runner recalled a process, for a message.
fn recalled_as(recalled: &Recalled) -> &'static str {
    match recalled {
        Recalled::NotReceived => "never received it",
        Recalled::Refused => "created nothing for it",
        Recalled::Held { status, .. } if status.state == ProcessState::Running => {
            "holds it running"
        }
        Recalled::Held { .. } => "holds it ended",
        Recalled::Dropped => "created it and no longer holds it",
    }
}

/// The attempts of governed executions, as the Runner looked them up with the resource
/// authority, by process.
#[cfg(feature = "devguard")]
type Attempts = HashMap<String, Attempt>;

/// A gateway without DevGuard looks up no attempt.
#[cfg(not(feature = "devguard"))]
struct Attempts;

/// The attempts of the governed executions among `records` at `asked`.
#[cfg(feature = "devguard")]
async fn look_up(runner: &UdsRunner, records: &[ProcessRecord], asked: &[usize]) -> Attempts {
    use codespace_runner::admission::AttemptRef;
    let attempts: Vec<AttemptRef> = asked
        .iter()
        .map(|&index| &records[index])
        .filter(|record| record.participation == RecordedParticipation::Required)
        .filter_map(|record| {
            record.attempt_id.as_ref().map(|attempt_id| AttemptRef {
                attempt_id: attempt_id.clone(),
                process_id: ProcessId(record.process_id.clone()),
            })
        })
        .collect();
    if attempts.is_empty() {
        return HashMap::new();
    }
    match runner.look_up_attempts(attempts.clone()).await {
        Ok(found) => found
            .into_iter()
            .map(|lookup| (lookup.process_id.0, Attempt::from(lookup.found)))
            .collect(),
        Err(err) => {
            tracing::debug!(error = %err, "the Runner could not look up attempts");
            attempts
                .into_iter()
                .map(|attempt| (attempt.process_id.0, Attempt::Failed))
                .collect()
        }
    }
}

#[cfg(not(feature = "devguard"))]
async fn look_up(_runner: &UdsRunner, _records: &[ProcessRecord], _asked: &[usize]) -> Attempts {
    Attempts
}

/// What became of a governed execution, as its Runner recalled it and the resource authority
/// states its attempt.
#[cfg(feature = "devguard")]
fn decide_required(record: &ProcessRecord, recalled: &Recalled, attempts: &Attempts) -> Decision {
    let (Some(attempt_id), Some(attempt)) = (
        record.attempt_id.as_deref(),
        attempts.get(&record.process_id),
    ) else {
        return Decision::Blocked(format!(
            "process `{}` takes part in the resource authority, and its record names no attempt \
             to look up",
            record.process_id
        ));
    };
    if let Recalled::Held { workspace_id, .. } = recalled {
        if *workspace_id != record.workspace_id {
            return decide(record, recalled);
        }
    }
    let mismatch = || {
        Decision::Blocked(format!(
            "the Runner and the resource authority do not agree on process `{}`: the Runner {}, \
             and attempt `{attempt_id}` is {}",
            record.process_id,
            recalled_as(recalled),
            attempt.as_str()
        ))
    };
    let unsettled = || {
        Decision::Pending(format!(
            "the resource authority has not settled attempt `{attempt_id}` of process `{}` yet",
            record.process_id
        ))
    };
    match (recalled, attempt) {
        (_, Attempt::Failed) => Decision::Pending(format!(
            "the resource authority could not be asked about attempt `{attempt_id}` of process \
             `{}`",
            record.process_id
        )),
        (Recalled::Held { status, .. }, attempt) if status.state == ProcessState::Running => {
            match attempt {
                Attempt::Launched => Decision::Running,
                Attempt::Unsettled => unsettled(),
                _ => mismatch(),
            }
        }
        (Recalled::Held { .. } | Recalled::Dropped, attempt) => match attempt {
            Attempt::ScopeEnded | Attempt::SettledBeforeStart => decide(record, recalled),
            Attempt::Launched | Attempt::Unsettled => unsettled(),
            _ => mismatch(),
        },
        (Recalled::NotReceived | Recalled::Refused, _)
            if record.phase != ProcessPhase::Dispatching =>
        {
            decide(record, recalled)
        }
        (Recalled::NotReceived | Recalled::Refused, attempt) => match attempt {
            Attempt::Absent | Attempt::SettledBeforeStart => Decision::NotStarted,
            Attempt::Unsettled => unsettled(),
            _ => mismatch(),
        },
    }
}

/// A gateway without DevGuard cannot ask about a governed execution.
#[cfg(not(feature = "devguard"))]
fn decide_required(record: &ProcessRecord, _recalled: &Recalled, _attempts: &Attempts) -> Decision {
    Decision::Blocked(format!(
        "process `{}` takes part in the resource authority, which this gateway cannot ask about",
        record.process_id
    ))
}

/// A governed execution's attempt, as the resource authority states it.
#[cfg(feature = "devguard")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// A launch helper claimed it: the authority holds its reservation.
    Launched,
    /// The authority observed its scope end, and released it.
    ScopeEnded,
    /// It ended before it started: denied, cancelled, expired, or released unlaunched.
    SettledBeforeStart,
    /// No attempt exists: the Runner never asked the authority to admit it, or the authority
    /// has no record of it.
    Absent,
    /// The authority holds it in a phase that may still change.
    Unsettled,
    /// The authority could not be asked.
    Failed,
}

#[cfg(feature = "devguard")]
impl Attempt {
    fn as_str(self) -> &'static str {
        match self {
            Self::Launched => "launched",
            Self::ScopeEnded => "released after its scope ended",
            Self::SettledBeforeStart => "settled before it started",
            Self::Absent => "absent",
            Self::Unsettled => "not settled",
            Self::Failed => "not known",
        }
    }
}

#[cfg(feature = "devguard")]
impl From<codespace_runner::admission::LookedUp> for Attempt {
    fn from(found: codespace_runner::admission::LookedUp) -> Self {
        use codespace_runner::admission::{AttemptState, LookedUp};
        match found {
            LookedUp::NotAdmitted | LookedUp::NotFound => Self::Absent,
            LookedUp::Failed => Self::Failed,
            LookedUp::Found(state) => match state {
                AttemptState::Launched => Self::Launched,
                AttemptState::ScopeEnded => Self::ScopeEnded,
                AttemptState::Denied
                | AttemptState::Cancelled
                | AttemptState::Expired
                | AttemptState::Released => Self::SettledBeforeStart,
                AttemptState::NotAsked | AttemptState::NotRecorded => Self::Absent,
                AttemptState::Other | AttemptState::Unknown => Self::Unsettled,
            },
        }
    }
}

/// An approval consumed by the execution `process_id`, which the Runner created for it.
pub(crate) fn consumed_by(
    approval_id: &ApprovalId,
    process_id: &ProcessId,
) -> OperationResumeResult {
    OperationResumeResult {
        approval_id: approval_id.clone(),
        state: ApprovalState::Consumed,
        apply_patch: None,
        exec_command: Some(ExecCommandResult {
            process_id: process_id.clone(),
            dispatch_status: ExecDispatchStatus::Confirmed,
            coordination: None,
        }),
    }
}

/// Whether a record proves that its execution started: the Runner answered that it started
/// it, or reported how it ended.
pub(crate) fn proves_started(record: &ProcessRecord) -> bool {
    match record.phase {
        ProcessPhase::Running => true,
        ProcessPhase::Terminal => record.outcome != Some(ProcessOutcome::NotStarted),
        ProcessPhase::Dispatching | ProcessPhase::Unknown => false,
    }
}

/// Log what changed in the workspaces where nothing new starts.
fn log_changes(before: &BTreeMap<String, Bar>, after: &BTreeMap<String, Bar>) {
    for (workspace_id, bar) in after {
        if before.get(workspace_id) == Some(bar) {
            continue;
        }
        match bar {
            Bar::Pending(reason) => tracing::info!(
                workspace_id,
                reason,
                "this workspace waits for its reconciliation with the independent Runner; it is \
                 tried again"
            ),
            Bar::Blocked(reason) => tracing::warn!(
                workspace_id,
                reason,
                "this workspace cannot be reconciled with the independent Runner, so nothing new \
                 starts there until an operator resolves it"
            ),
        }
    }
    for workspace_id in before.keys().filter(|ws| !after.contains_key(*ws)) {
        tracing::info!(
            workspace_id,
            "reconciled with the independent Runner: new work starts there again"
        );
    }
}
