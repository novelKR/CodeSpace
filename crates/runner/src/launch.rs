//! Managed launch of a prepared execution by its owner (CSRG-U4).
//!
//! A [`PreparedExecution`] (CSRG-U3) is launched once, in this order:
//!
//! 1. **Local checks** before anything is committed: the prepared command, its deadline, a
//!    usable launch helper ([`Launcher`]), and the program resolved to an absolute path on the
//!    execution's own `PATH`. A failure here cancels the unstarted attempt, as U3 does.
//! 2. **`BeginLaunch`, exactly once**, in a bounded session of the registered owner. Only its
//!    first answer carries the attempt's one-time permit. An error answer committed nothing, so
//!    the attempt is cancelled. A lost or unusable answer is never replaced by another
//!    `BeginLaunch`: the attempt is looked up once, cancelled if it is still prepared, reported
//!    as never received (`AbandonLaunch`) if it is committed and unclaimed, and otherwise
//!    reported as found or unknown.
//! 3. **The helper**, DevGuard's `devguard-launch`, started by CodeSpace's own pipe or PTY
//!    spawner as this process's direct child, filling the slot the preparation took. Only the
//!    helper receives the permit carrier and its transcript writer; this process's copies close
//!    as soon as the spawn returns. A failed spawn means no helper exists, which is reported
//!    (`AbandonLaunch`), so DevGuard releases the grant as never started.
//! 4. **The transcript**, read to its end on its own thread. The helper reports READY only after
//!    DevGuard has bound its process group as the attempt's scope, applied the policy and
//!    authorized the run; it then execs the executable, which keeps its PID, and the transcript
//!    closes. READY then close is a running payload; READY then `exec_failed` is a payload that
//!    could not be started; a refusal, a failure or an end before READY means the executable never
//!    ran, and once the helper is reaped this owner reports that it holds no helper.
//!
//! The process is CodeSpace's from the spawn on, exactly as an ungoverned one: output, stdin,
//! resize, timeout, termination and reaping are unchanged. CodeSpace waits at most
//! [`Launcher::ready_bound`] for the transcript; when it has not ended by then, the launch is
//! reported with an unknown dispatch, as a spawn whose result could not be confirmed, and the
//! transcript is still read to its end in the background.
//!
//! Not here (CSRG-U5): observation of the scope before CodeSpace reaps the process. DevGuard
//! releases a scope whose members all ended with its root; a descendant that outlives the root
//! keeps the attempt charged, and one CodeSpace reaped before DevGuard saw it leaves the attempt
//! suspect.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use codespace_devguard::admission::{self as dg, Abandon, Answer};
use codespace_devguard::launch::{
    check_helper, HelperProblem, HelperStage, LaunchAnswer, TranscriptEnd,
};
use codespace_domain::{ProcessId, ProcessState, ResourceAuthorityErrorCode};
use codespace_policy::{NetworkAxis, Workspace};
use serde::{Deserialize, Serialize};

use crate::admission::{
    abandoned, attempt_state, call, cancelled, AdmissionAccount, Applied, AppliedControl,
    AttemptState, Call, ManagedPreparation, PreparationOutcome, PreparedExecution,
};
use crate::process::spawn_env;
use crate::registration::error_code;
use crate::{RunnerCwd, RunnerExecRequest};

/// How long a launch waits for the helper's transcript by default.
pub const DEFAULT_READY_BOUND: Duration = Duration::from_secs(10);
/// How long a helper that never ran the executable is given to exit before it is killed.
const SETTLE_BOUND: Duration = Duration::from_secs(5);

/// Where the owner's launch helper is, and how long a launch waits for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    helper: PathBuf,
    ready_bound: Duration,
    /// Refuse when the Linux command sandbox would wrap the command. Only tests of the launch
    /// flow itself turn this off.
    sandbox_check: bool,
}

impl Launcher {
    /// DevGuard's `devguard-launch` at `helper`, an absolute path. Whether the file can serve is
    /// checked before each launch ([`check_helper`]), so a helper replaced or removed later is
    /// never handed a permit.
    pub fn new(helper: PathBuf) -> Result<Self, &'static str> {
        if !helper.is_absolute() {
            return Err(HelperProblem::NotAbsolute.describe());
        }
        Ok(Self {
            helper,
            ready_bound: DEFAULT_READY_BOUND,
            sandbox_check: true,
        })
    }

    /// Launch even where the Linux command sandbox is available, unsandboxed, as only a test
    /// of the launch flow may.
    #[cfg(test)]
    pub(crate) fn ignoring_the_sandbox(mut self) -> Self {
        self.sandbox_check = false;
        self
    }

    /// Wait at most `bound` for the helper's transcript.
    pub fn with_ready_bound(mut self, bound: Duration) -> Self {
        self.ready_bound = bound;
        self
    }

    pub fn helper(&self) -> &Path {
        &self.helper
    }

    pub fn ready_bound(&self) -> Duration {
        self.ready_bound
    }

    /// Whether the helper can be used now.
    pub fn usable(&self) -> Result<(), HelperProblem> {
        check_helper(&self.helper)
    }

    /// What the helper will be asked to run, or why this owner cannot launch `req` in `ws`.
    fn plan(&self, ws: &Workspace, req: &RunnerExecRequest) -> Result<Plan, PreparationOutcome> {
        if let Err(problem) = self.usable() {
            return Err(PreparationOutcome::HelperUnusable {
                problem: helper_problem(problem),
            });
        }
        // The Linux command sandbox wraps the command in its own helper, whose process the
        // resource authority would govern instead; DevGuard cannot register there anyway.
        if self.sandbox_check && crate::linux_sandbox_available() {
            return Err(PreparationOutcome::SandboxUnsupported);
        }
        if matches!(req.policy.network, NetworkAxis::Enabled) {
            return Err(PreparationOutcome::NetworkUnsupported);
        }
        let cwd = match req.cwd {
            RunnerCwd::WorkspaceRoot => ws.root.clone(),
        };
        let env = spawn_env(&cwd, req, false);
        let name = req.argv.first().map(String::as_str).unwrap_or_default();
        let Some(program) = resolve_program(name, &cwd, env.get("PATH").map(String::as_str)) else {
            return Err(PreparationOutcome::ProgramUnavailable);
        };
        Ok(Plan {
            program,
            args: req.argv.iter().skip(1).map(OsString::from).collect(),
        })
    }
}

/// The helper's command: the program by absolute path, and its arguments.
struct Plan {
    program: PathBuf,
    args: Vec<OsString>,
}

/// The search path of `execvp` when the environment has none: the C library's default.
#[cfg(target_os = "macos")]
const DEFAULT_PATH: &str = "/usr/bin:/bin";
/// The search path of `execvp` when the environment has none: glibc's default.
#[cfg(not(target_os = "macos"))]
const DEFAULT_PATH: &str = "/bin:/usr/bin";

/// `name` resolved as the spawn would resolve it: a name with a slash is a path, relative to the
/// working directory; a bare name is searched on `path` (the C library's default when unset,
/// as `execvp` does), an empty entry meaning the working directory. The first regular file this
/// user may execute wins. The helper runs the executable by this absolute path, which is
/// therefore its `argv[0]`.
pub fn resolve_program(name: &str, cwd: &Path, path: Option<&str>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let anchored = |dir: &str| -> PathBuf {
        match dir {
            "" => cwd.to_owned(),
            dir if Path::new(dir).is_absolute() => PathBuf::from(dir),
            dir => cwd.join(dir),
        }
    };
    if name.contains('/') {
        let candidate = anchored(name);
        return executable(&candidate).then_some(candidate);
    }
    path.unwrap_or(DEFAULT_PATH)
        .split(':')
        .map(|dir| anchored(dir).join(name))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if !std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access reads a NUL-terminated path.
    unsafe { libc::access(name.as_ptr(), libc::X_OK) == 0 }
}

/// A launch's phases, in the order they are reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPhase {
    /// Admitted and prepared (CSRG-U3).
    Prepared,
    /// `BeginLaunch` answered with the permit.
    LaunchCommitted,
    /// The helper process exists, as this process's child.
    HelperCreated,
    /// DevGuard bound the helper's process group as the attempt's scope.
    ScopeBound,
    /// DevGuard applied the scope's policy and read it back.
    PoliciesApplied,
    /// DevGuard authorized the run and the helper reported READY. Not evidence that the
    /// executable started.
    HelperReady,
    /// The helper attempted the executable.
    PayloadExecAttempted,
    /// The executable replaced the helper.
    PayloadRunning,
    /// The executable could not be started.
    PayloadFailed,
    /// DevGuard fenced the launch after it was committed; it settles through its scope.
    Draining,
    /// DevGuard returned the attempt's reservation.
    Released,
}

/// Whether the transcript confirmed the launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchDispatch {
    /// READY, then the transcript closed: the executable started.
    Confirmed,
    /// The helper exists and its transcript did not settle in time, or not well formed: the
    /// executable may or may not have started.
    Unknown,
}

/// A launched governed execution: a CodeSpace process like any other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedLaunch {
    pub attempt_id: String,
    pub process_id: ProcessId,
    pub dispatch: LaunchDispatch,
    pub phases: Vec<LaunchPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AdmissionAccount>,
    /// The scope's root as DevGuard bound it: the helper's PID, which the executable kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_root_pid: Option<u32>,
}

/// How a governed execution's launch ended for its request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "launch", rename_all = "snake_case")]
pub enum LaunchReport {
    Launched(ManagedLaunch),
    /// The command was not started; the report says why and what is known of the attempt.
    NotLaunched(ManagedPreparation),
}

impl LaunchReport {
    pub fn attempt_id(&self) -> &str {
        match self {
            Self::Launched(launch) => &launch.attempt_id,
            Self::NotLaunched(report) => &report.attempt_id,
        }
    }

    pub fn process_id(&self) -> &ProcessId {
        match self {
            Self::Launched(launch) => &launch.process_id,
            Self::NotLaunched(report) => &report.process_id,
        }
    }
}

fn helper_problem(problem: HelperProblem) -> crate::admission::HelperProblem {
    use crate::admission::HelperProblem as Problem;
    match problem {
        HelperProblem::NotAbsolute => Problem::NotAbsolute,
        HelperProblem::NotAFile => Problem::NotAFile,
        HelperProblem::NotExecutable => Problem::NotExecutable,
        HelperProblem::NotPrivate => Problem::NotPrivate,
    }
}

fn helper_stage(stage: HelperStage) -> crate::admission::HelperStage {
    use crate::admission::HelperStage as Stage;
    match stage {
        HelperStage::Scope => Stage::Scope,
        HelperStage::Clamp => Stage::Clamp,
        HelperStage::Report => Stage::Report,
        HelperStage::Permit => Stage::Permit,
        HelperStage::Connect => Stage::Connect,
        HelperStage::Other => Stage::Other,
    }
}

impl PreparedExecution {
    /// Launch the prepared execution once under the resource authority, as the module states,
    /// or report why its command was not started. Without a `launcher` the owner cannot launch:
    /// the unstarted attempt is cancelled and the outcome is `launch_unavailable`, as in U3.
    pub async fn launch_managed(
        mut self,
        request: &RunnerExecRequest,
        launcher: Option<&Launcher>,
    ) -> LaunchReport {
        if let Some(outcome) = self.refusal(request) {
            let attempt = self.settle().await;
            return LaunchReport::NotLaunched(self.report(outcome, attempt));
        }
        let Some(launcher) = launcher else {
            let attempt = self.settle().await;
            return LaunchReport::NotLaunched(
                self.report(PreparationOutcome::LaunchUnavailable, attempt),
            );
        };
        let plan = match launcher.plan(&self.ws, request) {
            Ok(plan) => plan,
            Err(outcome) => {
                let attempt = self.settle().await;
                return LaunchReport::NotLaunched(self.report(outcome, attempt));
            }
        };
        // From here the launch may be committed: a drop must not cancel it.
        self.settled = true;
        let mut phases = vec![LaunchPhase::Prepared];
        let permit = match begin_launch(&self).await {
            LaunchAnswer::Granted { permit, .. } => permit,
            LaunchAnswer::Refused(code) => {
                // An error answer committed nothing; the attempt is as it was.
                let attempt = cancelled(call(&self.authority, &self.admission, Call::Cancel).await);
                return self.not_launched(
                    PreparationOutcome::LaunchRefused {
                        error_code: error_code(code),
                    },
                    attempt,
                );
            }
            LaunchAnswer::NotSent(registration) => {
                let attempt = cancelled(call(&self.authority, &self.admission, Call::Cancel).await);
                return self.not_launched(
                    PreparationOutcome::AuthorityUnavailable {
                        registration: crate::registration::registration_state(registration.state),
                        error_code: registration.error_code.map(error_code),
                    },
                    attempt,
                );
            }
            LaunchAnswer::Unknown => {
                let attempt = resolve_launch(&self).await;
                return self.not_launched(PreparationOutcome::LaunchUnknown, attempt);
            }
        };
        phases.push(LaunchPhase::LaunchCommitted);
        let invocation = match self.authority.helper(
            launcher.helper(),
            &self.admission,
            permit,
            &plan.program,
            &plan.args,
        ) {
            Ok(invocation) => invocation,
            Err(_) => {
                let attempt = self.abandon(Abandon::SpawnFailed).await;
                return self.not_launched(PreparationOutcome::HelperNotCreated, attempt);
            }
        };
        let Some(slot) = self.slot.take() else {
            drop(invocation);
            let attempt = self.abandon(Abandon::SpawnFailed).await;
            return self.not_launched(PreparationOutcome::HelperNotCreated, attempt);
        };
        let (spawned, transcript) = self
            .runner
            .spawn_managed(&self.ws, request.clone(), slot, invocation)
            .await;
        if spawned.is_err() {
            // No helper exists, and none will be created for this grant.
            let attempt = self.abandon(Abandon::SpawnFailed).await;
            return self.not_launched(PreparationOutcome::HelperNotCreated, attempt);
        }
        phases.push(LaunchPhase::HelperCreated);
        let (sender, ended) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = sender.send(transcript.read());
        });
        let mut ended = ended;
        let end = match tokio::time::timeout(launcher.ready_bound(), &mut ended).await {
            Ok(Ok(end)) => end,
            Ok(Err(_)) => TranscriptEnd::Uncertain,
            Err(_) => {
                // Not settled in time: the process is CodeSpace's either way. The transcript
                // is still read to its end; then a helper that never ran the executable is
                // reported once it is reaped, and one whose transcript says nothing certain
                // once its process has ended.
                let settle = self.settler();
                tokio::spawn(async move {
                    match ended.await {
                        Ok(TranscriptEnd::Started | TranscriptEnd::ExecFailed { .. }) => {}
                        Ok(end) if never_ran(end) => {
                            settle.after_reap().await;
                        }
                        _ => {
                            settle.after_exit().await;
                        }
                    }
                });
                return self.launched(phases, LaunchDispatch::Unknown).await;
            }
        };
        match end {
            TranscriptEnd::Started => {
                phases.extend([
                    LaunchPhase::ScopeBound,
                    LaunchPhase::PoliciesApplied,
                    LaunchPhase::HelperReady,
                    LaunchPhase::PayloadExecAttempted,
                    LaunchPhase::PayloadRunning,
                ]);
                self.launched(phases, LaunchDispatch::Confirmed).await
            }
            TranscriptEnd::Uncertain => {
                // Whether the executable started is not known. Once the process has ended this
                // owner holds no helper: a grant the helper never claimed is then released as
                // never started, and a claimed one still settles through its scope.
                let settle = self.settler();
                tokio::spawn(async move {
                    settle.after_exit().await;
                });
                self.launched(phases, LaunchDispatch::Unknown).await
            }
            TranscriptEnd::ExecFailed { errno } => {
                // The helper claimed the grant before READY, so its scope settles it; the
                // helper exits 126 or 127 at once.
                self.settler().reaped().await;
                let attempt = self.looked_up().await;
                self.not_launched(PreparationOutcome::ExecFailed { errno }, attempt)
            }
            end => {
                let outcome = match end {
                    TranscriptEnd::Refused { code } => PreparationOutcome::HelperRefused {
                        error_code: error_code(code),
                    },
                    TranscriptEnd::Failed { stage } => PreparationOutcome::HelperFailed {
                        stage: helper_stage(stage),
                    },
                    _ => PreparationOutcome::HelperEnded,
                };
                let attempt = self.settler().after_reap().await;
                self.not_launched(outcome, attempt)
            }
        }
    }

    /// The prepared command's own refusal: another command than the prepared one, or past its
    /// deadline (CSRG-U3).
    fn refusal(&self, request: &RunnerExecRequest) -> Option<PreparationOutcome> {
        if crate::admission::meaning(&self.ws, request)
            .digest()
            .as_deref()
            != Some(self.identity.digest.as_str())
            || request.process_id != self.identity.process_id
        {
            Some(PreparationOutcome::DigestMismatch)
        } else if std::time::Instant::now() >= self.deadline {
            Some(PreparationOutcome::Expired)
        } else {
            None
        }
    }

    fn not_launched(&mut self, outcome: PreparationOutcome, attempt: AttemptState) -> LaunchReport {
        self.slot = None;
        LaunchReport::NotLaunched(self.report(outcome, attempt))
    }

    /// The launched report, with what DevGuard's record shows of the run.
    async fn launched(
        &mut self,
        phases: Vec<LaunchPhase>,
        dispatch: LaunchDispatch,
    ) -> LaunchReport {
        let mut account = self.account;
        let mut scope_root_pid = None;
        if dispatch == LaunchDispatch::Confirmed {
            if let Answer::Attempt(attempt) =
                call(&self.authority, &self.admission, Call::Lookup).await
            {
                scope_root_pid = attempt.scope.map(|scope| scope.root_pid);
                account.applied = match attempt.applied {
                    Some(applied) => Applied::Launched {
                        cpu: AppliedControl::from(applied.cpu),
                        memory: AppliedControl::from(applied.memory),
                        pids: AppliedControl::from(applied.pids),
                    },
                    None => Applied::Unknown,
                };
            } else {
                account.applied = Applied::Unknown;
            }
        } else {
            account.applied = Applied::Unknown;
        }
        LaunchReport::Launched(ManagedLaunch {
            attempt_id: self.identity.attempt_id.clone(),
            process_id: self.identity.process_id.clone(),
            dispatch,
            phases,
            account: Some(account),
            scope_root_pid,
        })
    }

    async fn abandon(&self, reason: Abandon) -> AttemptState {
        abandoned(call(&self.authority, &self.admission, Call::Abandon(reason)).await)
    }

    async fn looked_up(&self) -> AttemptState {
        match call(&self.authority, &self.admission, Call::Lookup).await {
            Answer::Attempt(attempt) => attempt_state(&attempt),
            _ => AttemptState::Unknown,
        }
    }

    fn settler(&self) -> Settler {
        Settler {
            authority: self.authority.clone(),
            admission: self.admission.clone(),
            runner: self.runner.clone(),
            process_id: self.identity.process_id.clone(),
        }
    }
}

/// Whether a transcript end proves the executable never ran.
fn never_ran(end: TranscriptEnd) -> bool {
    matches!(
        end,
        TranscriptEnd::Refused { .. }
            | TranscriptEnd::Failed { .. }
            | TranscriptEnd::EndedBeforeReady
    )
}

/// Settles a launch whose helper never ran the executable, once CodeSpace has reaped it.
struct Settler {
    authority: Arc<dyn crate::admission::AttemptAuthority>,
    admission: dg::Admission,
    runner: crate::InProcessRunner,
    process_id: ProcessId,
}

impl Settler {
    /// Wait until CodeSpace's reaper has reaped the helper, killing it if it has not exited
    /// within its bound. Whether it was reaped.
    async fn reaped(&self) -> bool {
        if self.exited_within(SETTLE_BOUND).await {
            return true;
        }
        let _ = self.runner.kill_host(&self.process_id);
        self.exited_within(SETTLE_BOUND).await
    }

    async fn exited_within(&self, bound: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            match self.runner.host_process_status(&self.process_id) {
                Ok(status) if status.state == ProcessState::Exited => return true,
                Ok(_) => {}
                // An evicted handle was reaped before it could be evicted.
                Err(_) => return true,
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Once the helper is reaped, report that this owner holds no helper for the grant: an
    /// unclaimed grant is then released as never started; a claimed one settles through its
    /// scope. Without the reap there is nothing to report, and the attempt stays unknown.
    async fn after_reap(&self) -> AttemptState {
        if !self.reaped().await {
            return AttemptState::Unknown;
        }
        self.abandon_exited().await
    }

    /// [`Self::after_reap`] for a process that may be the executable: however long it runs, it
    /// is reported once CodeSpace has reaped it, and it is not killed for it.
    async fn after_exit(&self) -> AttemptState {
        loop {
            match self.runner.host_process_status(&self.process_id) {
                Ok(status) if status.state != ProcessState::Exited => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                _ => return self.abandon_exited().await,
            }
        }
    }

    async fn abandon_exited(&self) -> AttemptState {
        abandoned(
            call(
                &self.authority,
                &self.admission,
                Call::Abandon(Abandon::HelperExited),
            )
            .await,
        )
    }
}

/// `BeginLaunch`, once, on its own thread.
async fn begin_launch(prepared: &PreparedExecution) -> LaunchAnswer {
    let (authority, admission) = (prepared.authority.clone(), prepared.admission.clone());
    tokio::task::spawn_blocking(move || authority.begin_launch(&admission))
        .await
        .unwrap_or(LaunchAnswer::Unknown)
}

/// After a `BeginLaunch` without a usable answer: look the attempt up once, and settle it where
/// its precondition holds. Still prepared: the launch was not committed, so it is cancelled.
/// Committed and unclaimed: the permit never arrived and no helper exists, so this owner says
/// so. Anything else is reported as found; no answer leaves it unknown.
async fn resolve_launch(prepared: &PreparedExecution) -> AttemptState {
    let (authority, admission) = (&prepared.authority, &prepared.admission);
    match call(authority, admission, Call::Lookup).await {
        Answer::Attempt(attempt) if attempt.phase == dg::Phase::Prepared => {
            cancelled(call(authority, admission, Call::Cancel).await)
        }
        Answer::Attempt(attempt)
            if attempt.phase == dg::Phase::LaunchCommitted && attempt.scope.is_none() =>
        {
            abandoned(
                call(
                    authority,
                    admission,
                    Call::Abandon(Abandon::GrantNotReceived),
                )
                .await,
            )
        }
        Answer::Attempt(attempt) => attempt_state(&attempt),
        Answer::Refused(_) | Answer::NotSent(_) | Answer::Unknown => AttemptState::Unknown,
    }
}

/// The resource authority's error code for a launch refusal.
pub(crate) fn launch_refusal(code: ResourceAuthorityErrorCode) -> codespace_domain::ErrorCode {
    use codespace_domain::ErrorCode;
    match code {
        ResourceAuthorityErrorCode::ResourceUnavailable => ErrorCode::ResourceUnavailable,
        ResourceAuthorityErrorCode::ResourcePolicyUnsupported => {
            ErrorCode::ResourcePolicyUnsupported
        }
        _ => ErrorCode::ResourceAuthorityUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::tests::{
        attempt, governed, prepared, record, request, runner, Fake, Grant, ABANDON, ADMIT, BEGIN,
        CANCEL, LOOKUP,
    };
    use crate::admission::Preparer;
    use crate::{InProcessRunner, Runner, RunnerReadProcess};
    use codespace_devguard as devguard;
    use codespace_domain::ErrorCode;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::PermissionsExt;

    /// A private directory that [`check_helper`] accepts for a stand-in helper.
    fn helper_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cs-launch-")
            .tempdir_in(if cfg!(target_os = "macos") {
                "/private/tmp"
            } else {
                "/tmp"
            })
            .unwrap()
    }

    /// A stand-in for DevGuard's launch helper. It takes the helper's arguments, runs `entry`,
    /// consumes the permit, then runs `behavior`, which writes transcript lines with `report`
    /// as the real helper does. It cannot present a grant, so it tests CodeSpace's side only.
    /// It runs under bash: the carriers' numbers can exceed 9, which dash, Linux's `sh`, cannot
    /// redirect. On macOS it is run once when written, with `--warm`: macOS checks a new
    /// executable file on its first exec, which can take seconds while other tests start theirs,
    /// and that time must not count against the launches it serves. Not on Linux, where the
    /// check does not exist and running a file this process has just written can fail with
    /// `ETXTBSY` (see `write_script` in `linux_sandbox`).
    fn stand_in(dir: &Path, name: &str, entry: &str, behavior: &str) -> PathBuf {
        let script = format!(
            r#"#!/bin/bash
if [ "$1" = --warm ]; then exit 0; fi
while [ "$1" != "--" ]; do
  case "$1" in
    --permit-fd) permit="$2" ;;
    --report-fd) report="$2" ;;
  esac
  shift 2
done
shift
{entry}
eval "IFS= read -r grant <&$permit"
eval "exec $permit<&-"
report() {{ eval "printf '%s\n' \"\$1\" >&$report"; }}
{behavior}
"#
        );
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        if cfg!(target_os = "macos") {
            let warmed = std::process::Command::new(&path)
                .arg("--warm")
                .status()
                .unwrap();
            assert!(warmed.success(), "{warmed}");
        }
        path
    }

    const READY: &str = r#"report '{"phase":"ready"}'; eval "exec $report>&-"; exec "$@""#;

    /// A launcher for these tests, which test the launch flow on hosts with or without the
    /// Linux command sandbox (see `the_linux_command_sandbox_is_not_launched_under_devguard`).
    fn launcher(helper: PathBuf) -> Launcher {
        Launcher::new(helper)
            .unwrap()
            .with_ready_bound(Duration::from_secs(5))
            .ignoring_the_sandbox()
    }

    async fn launch(
        fake: &Arc<Fake>,
        runner: &InProcessRunner,
        ws: &Workspace,
        req: RunnerExecRequest,
        launcher: Option<Launcher>,
    ) -> LaunchReport {
        Preparer::new(fake.clone(), runner.clone())
            .with_launcher(launcher)
            .prepare_and_launch(ws.clone(), req, "cs-attempt-u4".into())
            .await
    }

    /// The process's output once it has ended.
    async fn finished_output(runner: &InProcessRunner, process_id: &ProcessId) -> String {
        let mut chunk = String::new();
        for _ in 0..250 {
            let read = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            chunk = read.chunk.replace("\r\n", "\n");
            if read.eof {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        chunk
    }

    async fn settled_slots(runner: &InProcessRunner) -> usize {
        for _ in 0..250 {
            if runner.occupied_slots() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        runner.occupied_slots()
    }

    fn marker_request(root: &Path, process: &str) -> (PathBuf, RunnerExecRequest) {
        let marker = root.join(format!("ran-{process}"));
        let req = request(&["/usr/bin/touch", marker.to_str().unwrap()], process);
        (marker, req)
    }

    fn launched(report: LaunchReport) -> ManagedLaunch {
        match report {
            LaunchReport::Launched(launch) => launch,
            LaunchReport::NotLaunched(report) => panic!("not launched: {report:?}"),
        }
    }

    fn not_launched(report: LaunchReport) -> ManagedPreparation {
        match report {
            LaunchReport::NotLaunched(report) => report,
            LaunchReport::Launched(launch) => panic!("launched: {launch:?}"),
        }
    }

    const STARTED: [LaunchPhase; 8] = [
        LaunchPhase::Prepared,
        LaunchPhase::LaunchCommitted,
        LaunchPhase::HelperCreated,
        LaunchPhase::ScopeBound,
        LaunchPhase::PoliciesApplied,
        LaunchPhase::HelperReady,
        LaunchPhase::PayloadExecAttempted,
        LaunchPhase::PayloadRunning,
    ];

    #[tokio::test]
    async fn a_granted_launch_starts_the_command_once_through_each_spawner() {
        let helpers = helper_directory();
        let helper = stand_in(helpers.path(), "ready", "", READY);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        for tty in [false, true] {
            let runner = runner();
            let applied = dg::Attempt {
                scope: Some(dg::Scope {
                    kind: dg::ScopeKind::ObservedProcessGroup,
                    root_pid: 4242,
                }),
                applied: Some(dg::Applied {
                    cpu: dg::Application::Applied,
                    memory: dg::Application::Applied,
                    pids: dg::Application::Applied,
                }),
                ..record(dg::Phase::RunAuthorized, None)
            };
            let fake = Fake::admitting([prepared()]).then_lookup(Answer::Attempt(applied));
            let process = format!("proc-ready-{tty}");
            let mut req = request(
                &[
                    "/bin/sh",
                    "-c",
                    "if [ -t 0 ]; then echo tty:$0; else echo pipe:$0; fi",
                ],
                &process,
            );
            req.tty = tty;
            let launch =
                launched(launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await);
            assert_eq!(
                (launch.dispatch, launch.phases.as_slice()),
                (LaunchDispatch::Confirmed, STARTED.as_slice())
            );
            assert_eq!(launch.process_id.0, process);
            assert_eq!(launch.scope_root_pid, Some(4242));
            assert_eq!(
                launch.account.unwrap().applied,
                Applied::Launched {
                    cpu: AppliedControl::Applied,
                    memory: AppliedControl::Applied,
                    pids: AppliedControl::Applied,
                }
            );
            // The helper exec'd the command, which runs on CodeSpace's own pipe or PTY, with
            // its resolved path as argv[0].
            let expected = if tty {
                "tty:/bin/sh\n"
            } else {
                "pipe:/bin/sh\n"
            };
            assert_eq!(finished_output(&runner, &launch.process_id).await, expected);
            let status = runner.process_status(&launch.process_id).await.unwrap();
            assert_eq!(
                (status.state, status.exit_code),
                (ProcessState::Exited, Some(0))
            );
            // Asked once to admit, once to launch and once for the record; never cancelled.
            assert_eq!(
                (
                    fake.count(ADMIT),
                    fake.count(BEGIN),
                    fake.count(LOOKUP),
                    fake.count(CANCEL),
                    fake.count(ABANDON)
                ),
                (1, 1, 1, 0, 0)
            );
            assert_eq!(settled_slots(&runner).await, 0);
        }
    }

    #[tokio::test]
    async fn a_helper_that_never_runs_the_command_is_reported_once_it_is_reaped() {
        let helpers = helper_directory();
        let refused = r#"report '{"phase":"refused","code":"invalid_transition","message":"fenced"}'; exit 125"#;
        let failed =
            r#"report '{"phase":"failed","stage":"connect","message":"absent"}'; exit 125"#;
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let cases = [
            (
                stand_in(helpers.path(), "refused", "", refused),
                PreparationOutcome::HelperRefused {
                    error_code: ResourceAuthorityErrorCode::InvalidTransition,
                },
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            (
                stand_in(helpers.path(), "failed", "", failed),
                PreparationOutcome::HelperFailed {
                    stage: crate::admission::HelperStage::Connect,
                },
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            (
                // Killed before it reported anything.
                stand_in(helpers.path(), "dies", "", "kill -KILL $$"),
                PreparationOutcome::HelperEnded,
                ErrorCode::ProcessSpawnFailed,
            ),
        ];
        for (helper, outcome, code) in cases {
            for tty in [false, true] {
                let runner = runner();
                let fake = Fake::admitting([prepared()]);
                let (marker, mut req) = marker_request(dir.path(), &format!("proc-never-{tty}"));
                req.tty = tty;
                let report = not_launched(
                    launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await,
                );
                assert_eq!(
                    (report.outcome, report.attempt),
                    (outcome, AttemptState::Released),
                    "{helper:?} tty={tty}"
                );
                assert!(!marker.exists(), "the command ran");
                // The helper was reaped first; then this owner said it holds no helper.
                assert_eq!(fake.count(ABANDON), 1);
                assert_eq!(*fake.abandoned_for.lock().unwrap(), [Abandon::HelperExited]);
                assert_eq!(fake.count(CANCEL), 0);
                assert_eq!(settled_slots(&runner).await, 0);
                let body = report.into_error_body("gov");
                assert_eq!(body.code, code);
                assert!(
                    body.message.ends_with(
                        "attempt `cs-attempt-u4` for process_id `proc-never-false`: released; \
                         the command was not started"
                    ) || body.message.ends_with(
                        "attempt `cs-attempt-u4` for process_id `proc-never-true`: released; \
                         the command was not started"
                    ),
                    "{}",
                    body.message
                );
            }
        }
    }

    #[tokio::test]
    async fn a_report_for_a_claimed_grant_leaves_it_to_its_scope() {
        // Refused after the helper claimed the grant, as when DevGuard cannot read the policy
        // back: the owner's report cannot release it, and the attempt stays launched.
        let helpers = helper_directory();
        let refused = r#"report '{"phase":"refused","code":"resource_control_unavailable","message":"readback"}'; exit 125"#;
        let helper = stand_in(helpers.path(), "refused-claimed", "", refused);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()])
            .then_abandon(Answer::Attempt(record(dg::Phase::ScopeBound, None)));
        let (marker, req) = marker_request(dir.path(), "proc-claimed");
        let report = not_launched(launch(&fake, &runner, &ws, req, Some(launcher(helper))).await);
        assert_eq!(
            (report.outcome, report.attempt),
            (
                PreparationOutcome::HelperRefused {
                    error_code: ResourceAuthorityErrorCode::ResourceControlUnavailable,
                },
                AttemptState::Launched
            )
        );
        assert_eq!(*fake.abandoned_for.lock().unwrap(), [Abandon::HelperExited]);
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn an_exec_failure_after_ready_is_not_reported_as_unstarted() {
        let helpers = helper_directory();
        let helper = stand_in(
            helpers.path(),
            "exec-failed",
            "",
            r#"report '{"phase":"ready"}'; report '{"phase":"exec_failed","errno":13}'; exit 126"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()])
            .then_lookup(Answer::Attempt(record(dg::Phase::RunAuthorized, None)));
        let (marker, req) = marker_request(dir.path(), "proc-exec");
        let report = not_launched(launch(&fake, &runner, &ws, req, Some(launcher(helper))).await);
        // The helper claimed the grant, so only its scope settles it: no owner report.
        assert_eq!(
            (report.outcome, report.attempt),
            (
                PreparationOutcome::ExecFailed { errno: 13 },
                AttemptState::Launched
            )
        );
        assert_eq!(fake.count(ABANDON), 0);
        assert!(!marker.exists());
        let body = report.into_error_body("gov");
        assert_eq!(body.code, ErrorCode::ProcessSpawnFailed);
        assert!(body.message.contains("errno 13"), "{}", body.message);
    }

    #[tokio::test]
    async fn a_helper_that_cannot_be_started_is_reported_and_nothing_runs() {
        let helpers = helper_directory();
        // Executable and private, but its interpreter does not exist: the spawn fails.
        let helper = helpers.path().join("broken");
        std::fs::write(&helper, "#!/nonexistent/interpreter\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        for tty in [false, true] {
            let runner = runner();
            let fake = Fake::admitting([prepared()]);
            let (marker, mut req) = marker_request(dir.path(), &format!("proc-spawn-{tty}"));
            req.tty = tty;
            let report = not_launched(
                launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await,
            );
            assert_eq!(
                (report.outcome, report.attempt),
                (PreparationOutcome::HelperNotCreated, AttemptState::Released)
            );
            assert_eq!(*fake.abandoned_for.lock().unwrap(), [Abandon::SpawnFailed]);
            assert!(!marker.exists());
            assert_eq!(runner.occupied_slots(), 0);
            assert!(runner
                .host_workspace_of(&format!("proc-spawn-{tty}"))
                .is_none());
            assert_eq!(
                report.into_error_body("gov").code,
                ErrorCode::ProcessSpawnFailed
            );
        }
    }

    #[tokio::test]
    async fn a_transcript_that_does_not_settle_in_time_is_an_unknown_dispatch() {
        let helpers = helper_directory();
        // It becomes `sleep`, as the real helper becomes the command, so the transcript writer
        // ends with it.
        let slow = stand_in(helpers.path(), "slow", "", "exec sleep 30");
        let garbage = stand_in(
            helpers.path(),
            "garbage",
            "",
            r#"report 'not json'; eval "exec $report>&-"; exec "$@""#,
        );
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        // Never READY within the bound: the process is CodeSpace's either way.
        let fake = Fake::admitting([prepared()]);
        let req = request(&["/bin/echo", "late"], "proc-slow");
        let launch_report = launched(
            launch(
                &fake,
                &runner,
                &ws,
                req,
                Some(launcher(slow).with_ready_bound(Duration::from_millis(300))),
            )
            .await,
        );
        assert_eq!(launch_report.dispatch, LaunchDispatch::Unknown);
        assert_eq!(
            launch_report.phases,
            [
                LaunchPhase::Prepared,
                LaunchPhase::LaunchCommitted,
                LaunchPhase::HelperCreated
            ]
        );
        assert_eq!(launch_report.account.unwrap().applied, Applied::Unknown);
        let status = runner
            .process_status(&launch_report.process_id)
            .await
            .unwrap();
        assert_eq!(status.state, ProcessState::Running);
        // Terminated before READY: once it is reaped, the owner reports it holds no helper.
        runner.terminate(&launch_report.process_id).await.unwrap();
        for _ in 0..250 {
            if fake.count(ABANDON) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(*fake.abandoned_for.lock().unwrap(), [Abandon::HelperExited]);
        let status = runner
            .process_status(&launch_report.process_id)
            .await
            .unwrap();
        assert_eq!(
            status.termination,
            Some(codespace_domain::ProcessTermination::Terminated)
        );

        // A transcript that is not well formed: whether the command started is not known. Once
        // the process has ended the owner holds no helper, which it reports: DevGuard releases
        // an unclaimed grant, and settles a claimed one through its scope.
        let fake = Fake::admitting([prepared()]);
        let req = request(&["/bin/echo", "maybe"], "proc-garbage");
        let launch_report =
            launched(launch(&fake, &runner, &ws, req, Some(launcher(garbage))).await);
        assert_eq!(launch_report.dispatch, LaunchDispatch::Unknown);
        assert_eq!(
            finished_output(&runner, &launch_report.process_id).await,
            "maybe\n"
        );
        for _ in 0..250 {
            if fake.count(ABANDON) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(*fake.abandoned_for.lock().unwrap(), [Abandon::HelperExited]);
    }

    #[tokio::test]
    async fn begin_launch_is_sent_once_and_never_after_an_unknown_answer() {
        let helpers = helper_directory();
        let helper = stand_in(helpers.path(), "ready", "", READY);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let committed = Answer::Attempt(record(dg::Phase::LaunchCommitted, None));
        let cases = [
            // A refusal committed nothing: cancelled.
            (
                Fake::admitting([prepared()])
                    .then_grant(Grant::Refused(devguard::ErrorCode::InvalidTransition)),
                PreparationOutcome::LaunchRefused {
                    error_code: ResourceAuthorityErrorCode::InvalidTransition,
                },
                AttemptState::Cancelled,
                (1, 0, 0),
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            (
                Fake::admitting([prepared()])
                    .then_grant(Grant::Refused(devguard::ErrorCode::ResourceUnavailable)),
                PreparationOutcome::LaunchRefused {
                    error_code: ResourceAuthorityErrorCode::ResourceUnavailable,
                },
                AttemptState::Cancelled,
                (1, 0, 0),
                ErrorCode::ResourceUnavailable,
            ),
            // The session never sent it: cancelled.
            (
                Fake::admitting([prepared()]).then_grant(Grant::NotSent),
                PreparationOutcome::AuthorityUnavailable {
                    registration: codespace_domain::ResourceRegistrationState::Unavailable,
                    error_code: Some(ResourceAuthorityErrorCode::ResourceControlUnavailable),
                },
                AttemptState::Cancelled,
                (1, 0, 0),
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            // No answer, and still prepared: not committed, so cancelled.
            (
                Fake::admitting([prepared()])
                    .then_grant(Grant::Unknown)
                    .then_lookup(prepared()),
                PreparationOutcome::LaunchUnknown,
                AttemptState::Cancelled,
                (1, 1, 0),
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            // No answer, committed and unclaimed: the permit never arrived.
            (
                Fake::admitting([prepared()])
                    .then_grant(Grant::Unknown)
                    .then_lookup(committed),
                PreparationOutcome::LaunchUnknown,
                AttemptState::Released,
                (0, 1, 1),
                ErrorCode::ResourceAuthorityUnavailable,
            ),
            // No answer, and no record either: unknown.
            (
                Fake::admitting([prepared()]).then_grant(Grant::Unknown),
                PreparationOutcome::LaunchUnknown,
                AttemptState::Unknown,
                (0, 1, 0),
                ErrorCode::AdmissionUnknown,
            ),
        ];
        for (fake, outcome, state, (cancels, lookups, abandons), code) in cases {
            let runner = runner();
            let (marker, req) = marker_request(dir.path(), "proc-begin");
            let report = not_launched(
                launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await,
            );
            assert_eq!((report.outcome, report.attempt), (outcome, state));
            assert_eq!(fake.count(BEGIN), 1, "BeginLaunch is never sent again");
            assert_eq!(
                (fake.count(CANCEL), fake.count(LOOKUP), fake.count(ABANDON)),
                (cancels, lookups, abandons),
                "{outcome:?}"
            );
            if abandons == 1 {
                assert_eq!(
                    *fake.abandoned_for.lock().unwrap(),
                    [Abandon::GrantNotReceived]
                );
            }
            assert!(!marker.exists(), "no helper may start");
            assert_eq!(runner.occupied_slots(), 0);
            let body = report.into_error_body("gov");
            assert_eq!(body.code, code, "{outcome:?}");
            assert!(
                body.message.ends_with("nothing was started"),
                "{}",
                body.message
            );
        }
    }

    #[tokio::test]
    async fn nothing_is_committed_when_the_owner_cannot_launch() {
        let helpers = helper_directory();
        let ready = stand_in(helpers.path(), "ready", "", READY);
        // Writable by a group other than root's (a file made here takes the directory's group,
        // root's on macOS, so it is moved to this user's own group).
        let shared = helpers.path().join("shared");
        std::fs::copy(&ready, &shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o770)).unwrap();
        // SAFETY: getgid has no preconditions.
        std::os::unix::fs::chown(&shared, None, Some(unsafe { libc::getgid() })).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let mut network = request(&["/bin/echo"], "proc-local");
        network.policy.network = NetworkAxis::Enabled;
        let cases: [(
            Option<Launcher>,
            RunnerExecRequest,
            PreparationOutcome,
            ErrorCode,
        ); 4] = [
            // No launch helper: as in U3.
            (
                None,
                request(&["/bin/echo"], "proc-local"),
                PreparationOutcome::LaunchUnavailable,
                ErrorCode::ManagedLaunchUnavailable,
            ),
            // A helper others could replace is never handed a permit.
            (
                Some(launcher(shared)),
                request(&["/bin/echo"], "proc-local"),
                PreparationOutcome::HelperUnusable {
                    problem: crate::admission::HelperProblem::NotPrivate,
                },
                ErrorCode::ManagedLaunchUnavailable,
            ),
            (
                Some(launcher(ready.clone())),
                request(&["no-such-codespace-program"], "proc-local"),
                PreparationOutcome::ProgramUnavailable,
                ErrorCode::ProcessSpawnFailed,
            ),
            (
                Some(launcher(ready.clone())),
                network,
                PreparationOutcome::NetworkUnsupported,
                ErrorCode::ProcessSpawnFailed,
            ),
        ];
        for (launcher, req, outcome, code) in cases {
            let runner = runner();
            let fake = Fake::admitting([prepared()]);
            let report = not_launched(launch(&fake, &runner, &ws, req, launcher).await);
            assert_eq!(
                (report.outcome, report.attempt),
                (outcome, AttemptState::Cancelled)
            );
            assert_eq!((fake.count(BEGIN), fake.count(CANCEL)), (0, 1));
            assert_eq!(runner.occupied_slots(), 0);
            assert_eq!(report.into_error_body("gov").code, code);
        }
        // Past its deadline, or another command: cancelled, never launched.
        let runner = runner();
        let fake = Fake::admitting([prepared()]).then_cancel(attempt(dg::Phase::Expired, None));
        let req = request(&["/bin/echo"], "proc-late");
        let late = Preparer::new(fake.clone(), runner.clone())
            .with_ttl_bound(Duration::from_millis(20))
            .prepare(&ws, req.clone(), "cs-late".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let report = not_launched(
            late.launch_managed(&req, Some(&launcher(ready.clone())))
                .await,
        );
        assert_eq!(
            (report.outcome, report.attempt),
            (PreparationOutcome::Expired, AttemptState::Expired)
        );
        let fake2 = Fake::admitting([prepared()]);
        let mismatched = Preparer::new(fake2.clone(), runner.clone())
            .prepare(&ws, req.clone(), "cs-other".into())
            .await
            .unwrap();
        let mut other = req.clone();
        other.argv.push("again".into());
        let report = not_launched(
            mismatched
                .launch_managed(&other, Some(&launcher(ready)))
                .await,
        );
        assert_eq!(report.outcome, PreparationOutcome::DigestMismatch);
        assert_eq!((fake.count(BEGIN), fake2.count(BEGIN)), (0, 0));
    }

    /// An inheritable descriptor at or above `at`, as another thread's creation window leaves.
    fn inheritable_at(at: libc::c_int) -> OwnedFd {
        let null = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD returns a new descriptor without close-on-exec, owned below.
        let fd = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, at) };
        assert!(fd >= at);
        // SAFETY: as above.
        unsafe { OwnedFd::from_raw_fd(fd) }
    }

    /// Every open descriptor above 2 below 1024, as `name:<number>` lines.
    const HELD: &str = r#"n=3; while [ $n -lt 1024 ]; do if [ -e /dev/fd/$n ]; then echo "$0:$n"; fi; n=$((n + 1)); done"#;

    #[tokio::test]
    async fn only_the_helper_receives_its_two_descriptors_and_the_command_none() {
        let helpers = helper_directory();
        // The helper lists what it holds on entry, before it consumes the permit. A child shell
        // lists them, as the stand-in's own shell keeps its script open (close-on-exec).
        let entry = r#"held=$(/bin/sh -c 'n=3; while [ $n -lt 1024 ]; do if [ -e /dev/fd/$n ]; then printf " %s" $n; fi; n=$((n + 1)); done'); echo "helper:$held carriers: $permit $report""#;
        let helper = stand_in(helpers.path(), "fds", entry, READY);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        // What another thread's creation window would leave inheritable in this process.
        let unrelated = inheritable_at(700);
        for tty in [false, true] {
            let runner = runner();
            let fake = Fake::admitting([prepared()]);
            let mut req = request(
                &["/bin/sh", "-c", &format!("{HELD}; echo end"), "payload"],
                &format!("proc-fds-{tty}"),
            );
            req.tty = tty;
            let launch =
                launched(launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await);
            let output = finished_output(&runner, &launch.process_id).await;
            let mut lines = output.lines();
            let helper_line = lines.next().unwrap();
            let (held, carriers) = helper_line
                .strip_prefix("helper: ")
                .and_then(|rest| rest.split_once(" carriers: "))
                .unwrap_or_else(|| panic!("{output:?}"));
            let mut carriers: Vec<&str> = carriers.split(' ').collect();
            carriers.sort_by_key(|fd| fd.parse::<i32>().unwrap());
            assert_eq!(held, carriers.join(" "), "tty={tty}: {output:?}");
            assert!(!held
                .split(' ')
                .any(|fd| fd == unrelated.as_raw_fd().to_string()));
            // The command holds nothing beyond its standard descriptors.
            assert_eq!(lines.collect::<Vec<_>>(), ["end"], "tty={tty}: {output:?}");
        }
    }

    #[tokio::test]
    async fn the_linux_command_sandbox_is_not_launched_under_devguard() {
        if !crate::linux_sandbox_available() {
            return;
        }
        let helpers = helper_directory();
        let helper = stand_in(helpers.path(), "ready", "", READY);
        let dir = tempfile::tempdir().unwrap();
        let runner = runner();
        let fake = Fake::admitting([prepared()]);
        let (marker, req) = marker_request(dir.path(), "proc-sandboxed");
        let launcher = Launcher::new(helper)
            .unwrap()
            .with_ready_bound(Duration::from_secs(5));
        let report =
            not_launched(launch(&fake, &runner, &governed(dir.path()), req, Some(launcher)).await);
        assert_eq!(
            (report.outcome, report.attempt),
            (
                PreparationOutcome::SandboxUnsupported,
                AttemptState::Cancelled
            )
        );
        assert_eq!(fake.count(BEGIN), 0);
        assert!(!marker.exists());
    }

    /// The PTY path makes the two descriptors inheritable in this process for the length of its
    /// spawn. A child another thread spawns meanwhile holds them unless its spawner excludes
    /// unrelated descriptors, as CodeSpace's do: the unguarded control holds both; the guarded
    /// child holds neither.
    #[tokio::test]
    async fn during_a_pty_spawn_only_guarded_children_are_safe() {
        let helpers = helper_directory();
        let helper = stand_in(helpers.path(), "ready", "", READY);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        let runner = runner();
        let fake = Fake::admitting([prepared()]);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        crate::process::inheritable_window::set(
            "proc-window",
            Box::new(move |descriptors| {
                let script = format!(
                    "for n in {} {}; do if [ -e /dev/fd/$n ]; then echo held; else echo clear; fi; done",
                    descriptors[0], descriptors[1]
                );
                let child = |guarded: bool| {
                    let mut command = std::process::Command::new("/bin/sh");
                    command
                        .args(["-c", &script])
                        .stdin(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                    if guarded {
                        crate::descriptors::exclude_unrelated_std(&mut command);
                    }
                    String::from_utf8(command.output().unwrap().stdout).unwrap()
                };
                record.lock().unwrap().push((child(false), child(true)));
            }),
        );
        let mut req = request(&["/bin/echo", "window"], "proc-window");
        req.tty = true;
        let launch = launched(launch(&fake, &runner, &ws, req, Some(launcher(helper))).await);
        assert_eq!(
            finished_output(&runner, &launch.process_id).await,
            "window\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.as_slice(),
            [("held\nheld\n".to_owned(), "clear\nclear\n".to_owned())]
        );
    }

    /// A governed process times out as any other: CodeSpace's own timeout kills it, and its
    /// status says so.
    #[tokio::test]
    async fn a_governed_process_times_out_like_any_other() {
        let helpers = helper_directory();
        let helper = stand_in(helpers.path(), "ready", "", READY);
        let dir = tempfile::tempdir().unwrap();
        let ws = governed(dir.path());
        for tty in [false, true] {
            let runner = runner();
            let fake = Fake::admitting([prepared()]);
            let mut req = request(&["/bin/sleep", "30"], &format!("proc-timeout-{tty}"));
            req.tty = tty;
            // The timeout runs from the helper's spawn, so it must leave the helper time to
            // report READY under load; one that ends first ends the launch before the command.
            req.timeout_ms = 2_000;
            let launch =
                launched(launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await);
            assert_eq!(launch.dispatch, LaunchDispatch::Confirmed);
            let mut status = runner.process_status(&launch.process_id).await.unwrap();
            for _ in 0..500 {
                if status.state == ProcessState::Exited {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                status = runner.process_status(&launch.process_id).await.unwrap();
            }
            assert_eq!(
                (status.state, status.termination),
                (
                    ProcessState::Exited,
                    Some(codespace_domain::ProcessTermination::Timeout)
                ),
                "tty={tty}"
            );
            assert_eq!(fake.count(ABANDON), 0);
        }
    }

    /// Once the helper has started, the owner holds neither of its descriptors: they close when
    /// the spawn returns, and the transcript's reader when the transcript ends. It runs in a
    /// process of its own, where no other test reuses the numbers.
    #[test]
    fn the_owner_keeps_no_carrier_once_the_helper_started() {
        use crate::fork_handlers::tests::{in_isolated_copy, run_isolated};
        if !in_isolated_copy() {
            run_isolated("launch::tests::the_owner_keeps_no_carrier_once_the_helper_started");
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let helpers = helper_directory();
            let entry = r#"echo "carriers: $permit $report""#;
            let helper = stand_in(helpers.path(), "carriers", entry, READY);
            let dir = tempfile::tempdir().unwrap();
            let ws = governed(dir.path());
            for tty in [false, true] {
                let runner = runner();
                let fake = Fake::admitting([prepared()]);
                let mut req = request(&["/bin/echo", "done"], &format!("proc-carriers-{tty}"));
                req.tty = tty;
                let launch = launched(
                    launch(&fake, &runner, &ws, req, Some(launcher(helper.clone()))).await,
                );
                let output = finished_output(&runner, &launch.process_id).await;
                let carriers: Vec<libc::c_int> = output
                    .lines()
                    .find_map(|line| line.strip_prefix("carriers: "))
                    .unwrap_or_else(|| panic!("{output:?}"))
                    .split(' ')
                    .map(|fd| fd.parse().unwrap())
                    .collect();
                assert_eq!(carriers.len(), 2, "{output:?}");
                for fd in carriers {
                    // SAFETY: F_GETFD only reads a descriptor's flags.
                    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                    let error = std::io::Error::last_os_error().raw_os_error();
                    assert_eq!(
                        (flags, error),
                        (-1, Some(libc::EBADF)),
                        "tty={tty}: the owner still holds {fd}"
                    );
                }
            }
        });
    }

    #[test]
    fn programs_resolve_as_the_spawn_would() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let data = bin.join("data");
        std::fs::write(&data, "x").unwrap();
        let cwd = dir.path();
        let path = Some("/nonexistent:bin:/bin");
        assert_eq!(
            resolve_program("tool", cwd, path),
            Some(cwd.join("bin").join("tool"))
        );
        assert_eq!(
            resolve_program("sh", cwd, path),
            Some(PathBuf::from("/bin/sh"))
        );
        assert_eq!(resolve_program("data", cwd, path), None);
        assert_eq!(
            resolve_program("bin/tool", cwd, None),
            Some(cwd.join("bin/tool"))
        );
        assert_eq!(
            resolve_program("/bin/sh", cwd, Some("")),
            Some(PathBuf::from("/bin/sh"))
        );
        // Without `PATH`, the C library's default: macOS has no `/usr/bin/sh`, and glibc
        // searches `/bin` first.
        assert_eq!(
            resolve_program("sh", cwd, None),
            Some(PathBuf::from("/bin/sh"))
        );
        // An empty entry is the working directory.
        assert_eq!(
            resolve_program("tool", &bin, Some(":/bin")),
            Some(bin.join("tool"))
        );
        assert_eq!(resolve_program("", cwd, path), None);
        assert_eq!(resolve_program("bin", cwd, Some(".")), None);
    }
}
