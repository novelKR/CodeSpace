//! Managed workspace processes. Request lifetime is not process lifetime.
//! Pipe spawn uses `tokio::process::Command`, whose children keep only their
//! standard descriptors (`descriptors`). `tty: true` uses the isolated
//! `codespace-pty` adapter. When the Linux helper probe succeeds, both wrap
//! the same helper argv. UDS dispatch lives in `UdsRunner`.
//!
//! A governed execution (CSRG-U4) goes through the same two spawners: their command is
//! DevGuard's launch helper with its arguments, which becomes the user's executable, and the
//! helper alone also receives its two private descriptors. Process handles, output, timeout,
//! termination and reaping are the same as for any other process.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codespace_domain::{
    ErrorBody, ErrorCode, ProcessId, ProcessSignal, ProcessState, ProcessTermination, Profile,
};
use codespace_linux_sandbox_protocol::SandboxNetwork;
use codespace_policy::{NetworkAxis, Workspace};

use crate::linux_sandbox::{self, sandbox_exec_env};
use codespace_pty::PtySession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::lifecycle::process_start_identity;
use crate::{
    runner_local_exec_env, Recalled, RunnerCwd, RunnerExecRequest, RunnerExecResult,
    RunnerProcessRoot, RunnerProcessStatus, RunnerReadProcess, RunnerReadResult,
    RunnerResizeResult, RunnerWriteStdin,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_PROCESSES: usize = 8;
pub const DEFAULT_COMPLETED_TTL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_MAX_COMPLETED: usize = 64;
/// How many ended processes an independent Runner keeps, after their handles are released,
/// for a controller that has not acknowledged them yet (P1R-C04).
pub const DEFAULT_MAX_UNACKNOWLEDGED: usize = 64;

/// Called with a server-minted `process_id` when that process exits.
pub type ShellRelease = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    pub ttl: Duration,
    pub max_completed: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            ttl: DEFAULT_COMPLETED_TTL,
            max_completed: DEFAULT_MAX_COMPLETED,
        }
    }
}

fn max_processes() -> usize {
    std::env::var("CODESPACE_MAX_PROCESSES")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_PROCESSES)
}

#[derive(Clone)]
pub struct InProcessRunner {
    inner: Arc<Mutex<HashMap<String, Slot>>>,
    /// Slots taken for processes not yet created. Locked after `inner`, never before it.
    reserved: Arc<Mutex<HashSet<String>>>,
    /// What an independent Runner remembers for its controllers. Locked last.
    memory: Arc<Mutex<Memory>>,
    /// The live process limit, when set here instead of by `CODESPACE_MAX_PROCESSES`.
    max_processes: Option<usize>,
    on_release: ShellRelease,
    retention: RetentionPolicy,
    pub(crate) watches: crate::watch::WatchSet,
}

/// What an independent Runner remembers for its controllers (P1R-C04), once
/// [`InProcessRunner::keep_until_acknowledged`] turned it on: for its whole lifetime, whether it
/// created a process for each `process_id` it was asked to start, by its ID alone; and how many
/// ended processes it keeps, after their handles are released, until a controller
/// acknowledges them.
#[derive(Default)]
struct Memory {
    keep: Option<usize>,
    received: HashMap<String, Received>,
}

/// Whether a runner created the process it was asked to start (P1R-C04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Received {
    Executed,
    Refused,
}

/// A process slot, taken before the process is created (CSRG-U3). A slot counts against the
/// live process limit from the moment it is taken, so no process is created without one. It
/// is freed when it is dropped unfilled; once a created process fills it, the process's slot
/// lasts until the process ends. A slot is not a process handle, an admission or a workspace
/// lease.
pub struct SlotReservation {
    reserved: Arc<Mutex<HashSet<String>>>,
    process_id: String,
}

impl SlotReservation {
    pub fn process_id(&self) -> &str {
        &self.process_id
    }

    /// Hand the slot to the created process, under the runner's lock. The reservation is
    /// dropped only after the process holds the slot.
    fn fill(self, map: &mut HashMap<String, Slot>, slot: Slot) {
        map.insert(self.process_id.clone(), slot);
    }
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        if let Ok(mut reserved) = self.reserved.lock() {
            reserved.remove(&self.process_id);
        }
    }
}

struct Slot {
    workspace_id: String,
    io: SessionIo,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    on_kill: Option<KillHook>,
    /// When a controller acknowledged how the ended process ended (P1R-C04).
    acknowledged: Option<Instant>,
}

enum SessionIo {
    Pipe {
        child: Arc<Mutex<Child>>,
        stdin: Arc<Mutex<Option<ChildStdin>>>,
    },
    Pty {
        session: Arc<PtySession>,
        writer: mpsc::Sender<Vec<u8>>,
    },
    /// The ended process's handle was released when it would have been evicted, and its
    /// status and output are kept until a controller acknowledges them (P1R-C04).
    Released,
}

/// Lets a test act inside a managed PTY spawn's window, while the launch's two descriptors are
/// inheritable in this process.
#[cfg(all(test, feature = "devguard"))]
pub(crate) mod inheritable_window {
    type Hook = Box<dyn Fn([libc::c_int; 2]) + Send + Sync>;

    static HOOK: std::sync::Mutex<Option<(String, Hook)>> = std::sync::Mutex::new(None);

    /// Run `hook` with the two descriptors inside the window of `process_id`'s spawn.
    pub(crate) fn set(process_id: &str, hook: Hook) {
        *HOOK.lock().unwrap() = Some((process_id.to_owned(), hook));
    }

    pub(super) fn run(process_id: &str, descriptors: [libc::c_int; 2]) {
        if let Some((id, hook)) = HOOK.lock().unwrap().as_ref() {
            if id == process_id {
                hook(descriptors);
            }
        }
    }
}

/// Lets a test hold a pipe child's reaper, so that an exited child stays unreaped while other
/// paths act on it, and count the reaps.
#[cfg(test)]
pub(crate) mod reaper_gate {
    use std::sync::Mutex;

    static HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static REAPS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Hold `process_id`'s reaper until [`release`].
    pub(crate) fn hold(process_id: &str) {
        HELD.lock().unwrap().push(process_id.to_owned());
    }

    pub(crate) fn release(process_id: &str) {
        HELD.lock().unwrap().retain(|id| id != process_id);
    }

    /// How many times `process_id`'s child was reaped.
    pub(crate) fn reaps(process_id: &str) -> usize {
        REAPS
            .lock()
            .unwrap()
            .iter()
            .filter(|id| *id == process_id)
            .count()
    }

    pub(super) fn held(process_id: &str) -> bool {
        HELD.lock().unwrap().iter().any(|id| id == process_id)
    }

    pub(super) fn reaped(process_id: &str) {
        REAPS.lock().unwrap().push(process_id.to_owned());
    }
}

#[cfg(not(test))]
mod reaper_gate {
    pub(super) fn held(_process_id: &str) -> bool {
        false
    }

    pub(super) fn reaped(_process_id: &str) {}
}

/// What a spawn starts: the request's own command, or a governed execution's launch helper.
enum How {
    Direct,
    #[cfg(feature = "devguard")]
    Managed(Arc<Managed>),
}

impl How {
    /// The spawn has returned, whether or not it succeeded: a launch helper's descriptors are
    /// closed in this process.
    fn spawned(&self) {
        match self {
            How::Direct => {}
            #[cfg(feature = "devguard")]
            How::Managed(managed) => managed.close_descriptors(),
        }
    }
}

/// A launch helper's invocation while it is spawned, then the owner's end of its transcript.
#[cfg(feature = "devguard")]
pub(crate) struct Managed {
    invocation: std::sync::Mutex<Option<codespace_devguard::launch::HelperInvocation>>,
    transcript: std::sync::Mutex<Option<codespace_devguard::launch::Transcript>>,
    helper: PathBuf,
    args: Vec<OsString>,
    descriptors: [libc::c_int; 2],
}

#[cfg(feature = "devguard")]
impl Managed {
    fn new(invocation: codespace_devguard::launch::HelperInvocation) -> Self {
        Self {
            helper: invocation.helper().to_owned(),
            args: invocation.args().to_vec(),
            descriptors: invocation.descriptors(),
            invocation: std::sync::Mutex::new(Some(invocation)),
            transcript: std::sync::Mutex::new(None),
        }
    }

    fn helper(&self) -> &Path {
        &self.helper
    }

    fn args(&self) -> &[OsString] {
        &self.args
    }

    fn descriptors(&self) -> [libc::c_int; 2] {
        self.descriptors
    }

    fn make_inheritable(&self) -> std::io::Result<()> {
        match self.invocation.lock().ok().as_deref() {
            Some(Some(invocation)) => invocation.make_inheritable(),
            _ => Err(std::io::Error::from_raw_os_error(libc::EBADF)),
        }
    }

    /// Close this process's copies of the two descriptors, keeping the transcript.
    fn close_descriptors(&self) {
        let invocation = self.invocation.lock().ok().and_then(|mut held| held.take());
        if let Some(invocation) = invocation {
            if let Ok(mut transcript) = self.transcript.lock() {
                *transcript = Some(invocation.into_transcript());
            }
        }
    }

    fn finish(&self) -> codespace_devguard::launch::Transcript {
        self.close_descriptors();
        self.transcript
            .lock()
            .ok()
            .and_then(|mut transcript| transcript.take())
            .expect("the invocation became its transcript")
    }
}

/// When a process's reaper calls its exit observer (CSRG-U5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitPoint {
    /// The root has exited and is not reaped yet, so it still holds its PID.
    BeforeReap,
    /// The root has been reaped.
    AfterReap,
}

/// A hook that a process's one reaper calls while the exited root is still unreaped, and again
/// once it has reaped it (CSRG-U5): for example to have a resource authority observe the
/// process's scope while its group ID still provably names it. It runs on a blocking thread
/// for at most [`OBSERVE_BOUND`]; when it fails or runs longer the reaper reaps anyway, and
/// nothing else ever waits for the child. A PTY child's one reaper is the pinned Codex PTY's
/// waiter: with an observer, the recorded patch has it offer the same window before the reap
/// (`codespace_pty::spawn_observed`), bounded the same way.
pub type ExitObserver = Arc<dyn Fn(ExitPoint) + Send + Sync>;

/// How long a reaper waits for its exit observer.
pub const OBSERVE_BOUND: Duration = Duration::from_secs(2);

/// Call `observer` at `point`, for at most [`OBSERVE_BOUND`].
async fn observe(observer: &Option<ExitObserver>, point: ExitPoint) {
    let Some(observer) = observer.clone() else {
        return;
    };
    let call = tokio::task::spawn_blocking(move || observer(point));
    let _ = tokio::time::timeout(OBSERVE_BOUND, call).await;
}

/// A hook called when CodeSpace kills a process, on termination or timeout (CSRG-U5): on its
/// own blocking thread and never awaited, so the process's own kill does not depend on it; for
/// example to have a resource authority end the process's whole scope.
pub type KillHook = Arc<dyn Fn() + Send + Sync>;

/// What a spawn adds to its process (CSRG-U5): a governed execution's exit observer and kill
/// hook. An ungoverned process has neither.
#[derive(Clone, Default)]
pub struct ProcessHooks {
    pub observer: Option<ExitObserver>,
    pub on_kill: Option<KillHook>,
}

/// Run `hook` after CodeSpace's own kill, without waiting for it.
fn after_kill(hook: &Option<KillHook>) {
    let Some(hook) = hook.clone() else {
        return;
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn_blocking(move || hook());
    }
}

struct SpawnCtx {
    cwd: PathBuf,
    cap: usize,
    timeout: Duration,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    process_id: String,
    hooks: ProcessHooks,
}

#[derive(Default)]
struct OutputBuf {
    dropped: u64,
    total: u64,
    bytes: Vec<u8>,
    eof: bool,
    timed_out: bool,
}

#[derive(Clone, Copy)]
struct LifecycleSnapshot {
    state: ProcessState,
    exit_code: Option<i32>,
    termination: Option<ProcessTermination>,
    signal: Option<i32>,
}

#[derive(Clone, Copy)]
enum KillIntent {
    Timeout,
    Terminated,
}

struct Lifecycle {
    finished: bool,
    /// A PTY root that has exited and waits for its reaper, which offered its exit window
    /// (CSRG-U5). A pipe root's reaper sees this with `waitid`; a PTY root's is the Codex waiter.
    exited: bool,
    kill_intent: Option<KillIntent>,
    snapshot: LifecycleSnapshot,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            finished: false,
            exited: false,
            kill_intent: None,
            snapshot: LifecycleSnapshot {
                state: ProcessState::Running,
                exit_code: None,
                termination: None,
                signal: None,
            },
        }
    }

    fn note_kill(&mut self, intent: KillIntent) {
        if self.kill_intent.is_none() {
            self.kill_intent = Some(intent);
        }
    }

    /// Records a reaped child: its exit code, or the signal that ended it.
    /// A kill that CodeSpace requested stays `timeout` or `terminated`.
    fn finish_wait(&mut self, code: Option<i32>, signal: Option<i32>) {
        if self.finished {
            return;
        }
        self.finished = true;
        match self.kill_intent {
            Some(KillIntent::Timeout) => {
                self.snapshot = LifecycleSnapshot {
                    state: ProcessState::Exited,
                    exit_code: None,
                    termination: Some(ProcessTermination::Timeout),
                    signal: None,
                };
            }
            Some(KillIntent::Terminated) => {
                self.snapshot = LifecycleSnapshot {
                    state: ProcessState::Exited,
                    exit_code: None,
                    termination: Some(ProcessTermination::Terminated),
                    signal: None,
                };
            }
            None if signal.is_some() => {
                self.snapshot = LifecycleSnapshot {
                    state: ProcessState::Exited,
                    exit_code: None,
                    termination: Some(ProcessTermination::Signaled),
                    signal,
                };
            }
            None => {
                self.snapshot = LifecycleSnapshot {
                    state: ProcessState::Exited,
                    exit_code: code,
                    termination: Some(ProcessTermination::Exited),
                    signal: None,
                };
            }
        }
    }

    fn finish_lost(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let termination = match self.kill_intent {
            Some(KillIntent::Timeout) => ProcessTermination::Timeout,
            Some(KillIntent::Terminated) => ProcessTermination::Terminated,
            None => ProcessTermination::Unknown,
        };
        self.snapshot = LifecycleSnapshot {
            state: ProcessState::Exited,
            exit_code: None,
            termination: Some(termination),
            signal: None,
        };
    }
}

impl InProcessRunner {
    pub fn new(on_release: ShellRelease) -> Self {
        Self::with_retention(on_release, RetentionPolicy::default())
    }

    pub fn with_retention(on_release: ShellRelease, retention: RetentionPolicy) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            reserved: Arc::new(Mutex::new(HashSet::new())),
            memory: Arc::new(Mutex::new(Memory::default())),
            max_processes: None,
            on_release,
            retention,
            watches: crate::watch::WatchSet::default(),
        }
    }

    /// This runner's live process limit, instead of `CODESPACE_MAX_PROCESSES`.
    pub fn with_max_processes(mut self, limit: usize) -> Self {
        self.max_processes = Some(limit.max(1));
        self
    }

    fn limit(&self) -> usize {
        self.max_processes.unwrap_or_else(max_processes)
    }

    /// Take a process slot for `process_id` before creating anything (CSRG-U3). Live
    /// processes and slots already taken count against the limit, so an over-limit request is
    /// refused here, before a process exists.
    pub fn reserve_slot(&self, process_id: &str) -> Result<SlotReservation, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let mut reserved = self.reserved.lock().expect("slots");
        if reserved.contains(process_id) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidCommand,
                format!("process_id `{process_id}` is already in use"),
            ));
        }
        if live_count(&map) + reserved.len() >= self.limit() {
            return Err(ErrorBody::new(
                ErrorCode::WorkspaceBusy,
                "live process limit reached",
            ));
        }
        reserved.insert(process_id.to_string());
        Ok(SlotReservation {
            reserved: self.reserved.clone(),
            process_id: process_id.to_string(),
        })
    }

    /// Live processes and slots taken for processes not yet created.
    pub fn occupied_slots(&self) -> usize {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let reserved = self.reserved.lock().expect("slots").len();
        live_count(&map) + reserved
    }

    /// Evict the ended processes past the retention policy: those that ended more than `ttl`
    /// ago, and the oldest beyond `max_completed`. An independent Runner releases the handle of
    /// such a process at the same moment, but keeps its status and output until a controller
    /// acknowledges them (P1R-C04): releasing the handle earlier would change what its drop does
    /// to the process group of a tty process, so it is never released earlier (D11). Once
    /// acknowledged, a released process stays `ttl` longer; beyond `keep` released processes,
    /// the oldest goes, and the runner remembers only that it ended.
    fn evict_completed(&self, map: &mut HashMap<String, Slot>) {
        let now = Instant::now();
        let ttl = self.retention.ttl;
        let keep = self.memory.lock().expect("memory").keep;
        let mut ended: Vec<(String, Instant)> = map
            .iter()
            .filter(|(_, slot)| !matches!(slot.io, SessionIo::Released))
            .filter_map(|(id, slot)| completed_at(slot).map(|at| (id.clone(), at)))
            .collect();
        ended.sort_by_key(|(_, at)| *at);
        let over = ended.len().saturating_sub(self.retention.max_completed);
        for (index, (id, at)) in ended.into_iter().enumerate() {
            if index >= over && now.duration_since(at) <= ttl {
                continue;
            }
            match map.get_mut(&id) {
                Some(slot) if keep.is_some() && slot.acknowledged.is_none() => {
                    slot.io = SessionIo::Released;
                    slot.on_kill = None;
                }
                _ => {
                    map.remove(&id);
                }
            }
        }
        let Some(keep) = keep else {
            return;
        };
        map.retain(|_, slot| {
            !(matches!(slot.io, SessionIo::Released)
                && slot
                    .acknowledged
                    .is_some_and(|at| now.duration_since(at) > ttl))
        });
        let mut released: Vec<(String, Instant)> = map
            .iter()
            .filter(|(_, slot)| matches!(slot.io, SessionIo::Released))
            .filter_map(|(id, slot)| completed_at(slot).map(|at| (id.clone(), at)))
            .collect();
        if released.len() <= keep {
            return;
        }
        released.sort_by_key(|(_, at)| *at);
        let drop_n = released.len() - keep;
        for (id, _) in released.into_iter().take(drop_n) {
            map.remove(&id);
        }
    }

    /// Hand `reservation`'s slot to the process created for it, under the runner's lock: the
    /// runner remembers that it created it (P1R-C04).
    fn fill(&self, reservation: SlotReservation, slot: Slot) {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let process_id = reservation.process_id.clone();
        reservation.fill(&mut map, slot);
        self.remember(&process_id, Received::Executed);
    }

    fn remember(&self, process_id: &str, received: Received) {
        let mut memory = self.memory.lock().expect("memory");
        if memory.keep.is_none() {
            return;
        }
        let known = memory
            .received
            .entry(process_id.to_string())
            .or_insert(received);
        if received == Received::Executed {
            *known = Received::Executed;
        }
    }

    /// Keep what an independent Runner's controllers need from now on (P1R-C04): whether a
    /// process was created for each `process_id` this runner is asked to start, for its whole
    /// lifetime, and each ended process's status and output until a controller acknowledges
    /// them, at most `keep` of them once their handles are released.
    pub fn keep_until_acknowledged(&self, keep: usize) {
        self.memory.lock().expect("memory").keep = Some(keep.max(1));
    }

    /// A request to start `process_id` reached this runner. Unless it created the process, it
    /// remembers that it refused it (P1R-C04).
    pub(crate) fn note_received(&self, process_id: &str) {
        self.remember(process_id, Received::Refused);
    }

    /// What this runner knows of `process_id` (P1R-C04). Only an independent Runner that keeps
    /// what its controllers need remembers a process it no longer holds.
    pub fn recall(&self, process_id: &ProcessId) -> Recalled {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        if let Some(slot) = map.get(&process_id.0) {
            return Recalled::Held {
                workspace_id: slot.workspace_id.clone(),
                status: slot_status(process_id, slot),
            };
        }
        match self
            .memory
            .lock()
            .expect("memory")
            .received
            .get(&process_id.0)
        {
            Some(Received::Executed) => Recalled::Dropped,
            Some(Received::Refused) => Recalled::Refused,
            None => Recalled::NotReceived,
        }
    }

    /// A controller recorded how these processes ended (P1R-C04): each is evicted as any
    /// other ended process from now on, and one whose handle was already released stays
    /// `ttl` longer. A process that has not ended is left as it is.
    pub fn acknowledge(&self, process_ids: &[ProcessId]) {
        let mut map = self.inner.lock().expect("runner");
        let now = Instant::now();
        for process_id in process_ids {
            if let Some(slot) = map.get_mut(&process_id.0) {
                if slot.acknowledged.is_none() && completed_at(slot).is_some() {
                    slot.acknowledged = Some(now);
                }
            }
        }
        self.evict_completed(&mut map);
    }

    pub async fn spawn_host(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, ErrorBody> {
        ws.require_exec()?;
        if req.argv.is_empty() || req.argv[0].is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidCommand,
                "command must be a non-empty argv (no shell)",
            ));
        }
        // The slot first: no process is created without one.
        let slot = self.reserve_slot(&req.process_id.0)?;
        self.spawn_slotted(ws, req, slot, How::Direct, ProcessHooks::default())
            .await
    }

    /// Start a governed execution's launch helper through this runner's own pipe or PTY spawner
    /// (CSRG-U4), filling the slot its preparation took. The helper alone receives the
    /// invocation's two descriptors, and this process's copies are closed as soon as the spawn
    /// returns. The helper becomes the execution: from here it is a process like any other.
    /// Its reaper calls `hooks.observer` before and after the reap, and CodeSpace's kills call
    /// `hooks.on_kill` (CSRG-U5). Returns the spawn's result and the owner's end of the helper's
    /// transcript.
    #[cfg(feature = "devguard")]
    pub(crate) async fn spawn_managed(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        slot: SlotReservation,
        invocation: codespace_devguard::launch::HelperInvocation,
        hooks: ProcessHooks,
    ) -> (
        Result<RunnerExecResult, ErrorBody>,
        codespace_devguard::launch::Transcript,
    ) {
        let managed = Arc::new(Managed::new(invocation));
        let result = self
            .spawn_slotted(ws, req, slot, How::Managed(managed.clone()), hooks)
            .await;
        (result, managed.finish())
    }

    async fn spawn_slotted(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        slot: SlotReservation,
        how: How,
        hooks: ProcessHooks,
    ) -> Result<RunnerExecResult, ErrorBody> {
        let cap = req.output_bytes_cap.max(1) as usize;
        let timeout = Duration::from_millis(req.timeout_ms.max(1));
        let cwd = match req.cwd {
            RunnerCwd::WorkspaceRoot => ws.root.clone(),
        };
        let ctx = SpawnCtx {
            cwd,
            cap,
            timeout,
            output: Arc::new(Mutex::new(OutputBuf::default())),
            completed_at: Arc::new(Mutex::new(None)),
            lifecycle: Arc::new(Mutex::new(Lifecycle::new())),
            process_id: req.process_id.0.clone(),
            hooks,
        };
        if req.tty {
            self.spawn_pty(ws, req, ctx, slot, how).await
        } else {
            self.spawn_pipe(ws, req, ctx, slot, how)
        }
    }

    fn spawn_pipe(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        ctx: SpawnCtx,
        reservation: SlotReservation,
        how: How,
    ) -> Result<RunnerExecResult, ErrorBody> {
        let SpawnCtx {
            cwd,
            cap,
            timeout,
            output,
            completed_at,
            lifecycle,
            process_id,
            hooks,
        } = ctx;
        let ProcessHooks { observer, on_kill } = hooks;
        let (mut child, launch) = match &how {
            How::Direct => {
                let launch = exec_launch(ws, &req)?;
                let mut child = Command::new(&launch.program);
                child.args(&launch.args);
                (child, Some(launch))
            }
            #[cfg(feature = "devguard")]
            How::Managed(managed) => {
                let mut child = Command::new(managed.helper());
                child.args(managed.args());
                (child, None)
            }
        };
        child
            .current_dir(&cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        match &how {
            How::Direct => {
                crate::descriptors::exclude_unrelated(&mut child);
            }
            #[cfg(feature = "devguard")]
            How::Managed(managed) => {
                crate::descriptors::exclude_unrelated_except(&mut child, managed.descriptors());
            }
        }
        let sandboxed = launch.as_ref().is_some_and(|launch| launch.sandboxed);
        for (key, value) in spawn_env(&cwd, &req, sandboxed) {
            child.env(key, value);
        }
        let spawned = child.spawn();
        how.spawned();
        let mut spawned = spawned.map_err(|err| {
            if let Some(launch) = &launch {
                launch.abort_plan();
            }
            ErrorBody::new(
                ErrorCode::ProcessSpawnFailed,
                format!("failed to spawn process: {err}"),
            )
        })?;
        // Before its reaper exists, so the PID is still this child's (P1R-C02).
        let root = spawned.id().map(|pid| RunnerProcessRoot {
            pid,
            started: process_start_identity(pid),
        });
        let deadline_at_ms = deadline_after(timeout);
        let stdin = spawned.stdin.take();
        let stdout = spawned.stdout.take();
        let stderr = spawned.stderr.take();
        let child = Arc::new(Mutex::new(spawned));
        let slot = Slot {
            workspace_id: ws.id.0.clone(),
            io: SessionIo::Pipe {
                child: child.clone(),
                stdin: Arc::new(Mutex::new(stdin)),
            },
            output: output.clone(),
            completed_at: completed_at.clone(),
            lifecycle: lifecycle.clone(),
            on_kill: on_kill.clone(),
            acknowledged: None,
        };
        self.fill(reservation, slot);

        let out_handle = stdout.map(|out| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(out, buf, cap).await })
        });
        let err_handle = stderr.map(|err| {
            let buf = output.clone();
            tokio::spawn(async move { pump_reader(err, buf, cap).await })
        });

        let wait_child = child.clone();
        let wait_out = output.clone();
        let wait_life = lifecycle.clone();
        let wait_release = self.on_release.clone();
        let wait_process = process_id;
        tokio::spawn(async move {
            reap_child(wait_child, wait_life, &wait_process, observer).await;
            join_pump(out_handle).await;
            join_pump(err_handle).await;
            if let Ok(mut buf) = wait_out.lock() {
                buf.eof = true;
            }
            if let Ok(mut done) = completed_at.lock() {
                *done = Some(Instant::now());
            }
            wait_release(&wait_process);
        });

        let timeout_child = child;
        let timeout_out = output;
        let timeout_life = lifecycle;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let mut ch = timeout_child.lock().expect("child");
            let mut life = timeout_life.lock().expect("lifecycle");
            // Only the reaper waits: a child that has exited ended on its own, and the reaper
            // records how; one that has not is still unreaped, so the kill reaches it alone.
            if life.finished || exited_unreaped(&ch) {
                return;
            }
            life.note_kill(KillIntent::Timeout);
            let _ = ch.start_kill();
            drop(life);
            after_kill(&on_kill);
            if let Ok(mut buf) = timeout_out.lock() {
                buf.timed_out = true;
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
            root,
            deadline_at_ms,
        })
    }

    async fn spawn_pty(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        ctx: SpawnCtx,
        reservation: SlotReservation,
        how: How,
    ) -> Result<RunnerExecResult, ErrorBody> {
        let SpawnCtx {
            cwd,
            cap,
            timeout,
            output,
            completed_at,
            lifecycle,
            process_id,
            hooks,
        } = ctx;
        let ProcessHooks { observer, on_kill } = hooks;
        let launch = match &how {
            How::Direct => Some(exec_launch(ws, &req)?),
            #[cfg(feature = "devguard")]
            How::Managed(_) => None,
        };
        let abort = || {
            if let Some(launch) = &launch {
                launch.abort_plan();
            }
        };
        let sandboxed = launch.as_ref().is_some_and(|launch| launch.sandboxed);
        let mut env = spawn_env(&cwd, &req, sandboxed);
        if req.env.use_runner_defaults {
            env.insert("TERM".into(), "xterm".into());
        }
        let utf8 = match (&launch, &how) {
            (Some(launch), _) => utf8_launch(&launch.program, &launch.args),
            #[cfg(feature = "devguard")]
            (None, How::Managed(managed)) => utf8_launch(managed.helper(), managed.args()),
            (None, _) => Err(ErrorBody::new(
                ErrorCode::Internal,
                "no command to launch on the PTY",
            )),
        };
        let (program, args) = match utf8 {
            Ok(value) => value,
            Err(err) => {
                abort();
                return Err(err);
            }
        };
        // portable-pty and the Codex PTY's descriptor-keeping path fork: see
        // `prepare_fork_spawns`.
        crate::prepare_fork_spawns();
        // A process with an exit observer gets the same window before the reap as a pipe root
        // (CSRG-U5): the patched Codex PTY's waiter offers it, bounded by OBSERVE_BOUND.
        let spawn = |inherited: Vec<std::os::fd::RawFd>| {
            let (program, args, cwd, env) = (&program, &args, &cwd, &env);
            let observed = observer.is_some();
            async move {
                if observed {
                    codespace_pty::spawn_observed(
                        program,
                        args,
                        cwd,
                        env,
                        &inherited,
                        OBSERVE_BOUND,
                    )
                    .await
                    .map(|(session, watch)| (session, Some(watch)))
                } else {
                    codespace_pty::spawn_inheriting(program, args, cwd, env, &inherited)
                        .await
                        .map(|session| (session, None))
                }
            }
        };
        let spawned = match &how {
            How::Direct => spawn(Vec::new()).await,
            #[cfg(feature = "devguard")]
            How::Managed(managed) => {
                // The pinned Codex PTY keeps only descriptors that are already inheritable, so
                // the two are inheritable here from now until `how.spawned()` closes them right
                // after the spawn. Every other spawner in this process excludes unrelated
                // descriptors from its children meanwhile (#79).
                match managed.make_inheritable() {
                    Ok(()) => {
                        #[cfg(test)]
                        inheritable_window::run(&process_id, managed.descriptors());
                        spawn(managed.descriptors().to_vec()).await
                    }
                    Err(err) => Err(format!("cannot pass the launch descriptors: {err}")),
                }
            }
        };
        how.spawned();
        let (mut session, watch) = spawned.map_err(|err| {
            abort();
            ErrorBody::new(
                ErrorCode::ProcessSpawnFailed,
                format!("failed to spawn process: {err}"),
            )
        })?;
        // The pinned Codex PTY does not name its child, so a PTY root has no PID here.
        let deadline_at_ms = deadline_after(timeout);
        let stdout = session
            .take_stdout()
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "PTY stdout missing"))?;
        let exit = session
            .take_exit()
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "PTY exit missing"))?;
        let writer = session.writer();
        let session = Arc::new(session);
        let slot = Slot {
            workspace_id: ws.id.0.clone(),
            io: SessionIo::Pty {
                session: session.clone(),
                writer,
            },
            output: output.clone(),
            completed_at: completed_at.clone(),
            lifecycle: lifecycle.clone(),
            on_kill: on_kill.clone(),
            acknowledged: None,
        };
        self.fill(reservation, slot);

        if let Some(mut watch) = watch {
            let (observer, life) = (observer.clone(), lifecycle.clone());
            tokio::spawn(async move {
                let Some(window) = watch.window().await else {
                    return;
                };
                // The root has exited and still holds its PID until the window is released.
                // Termination and the timeout leave it to its reaper from now on, as a pipe
                // root's, so how it ended stays what it was.
                life.lock().expect("lifecycle").exited = true;
                observe(&observer, ExitPoint::BeforeReap).await;
                let _ = window.release();
            });
        }

        let out_handle = {
            let buf = output.clone();
            Some(tokio::spawn(
                async move { pump_chunks(stdout, buf, cap).await },
            ))
        };
        let wait_out = output.clone();
        let wait_life = lifecycle.clone();
        let wait_release = self.on_release.clone();
        let wait_process = process_id;
        tokio::spawn(async move {
            match exit.await {
                // The pinned Codex PTY reports a signal death as exit code 1, or as 128 plus the
                // signal for a child that inherited descriptors, as a governed one does.
                Ok(code) => wait_life
                    .lock()
                    .expect("lifecycle")
                    .finish_wait(Some(code), None),
                Err(_) => wait_life.lock().expect("lifecycle").finish_lost(),
            }
            // The Codex waiter has reaped the root, which a governed process's observer saw
            // through the window before the reap.
            observe(&observer, ExitPoint::AfterReap).await;
            join_pump(out_handle).await;
            if let Ok(mut buf) = wait_out.lock() {
                buf.eof = true;
            }
            if let Ok(mut done) = completed_at.lock() {
                *done = Some(Instant::now());
            }
            wait_release(&wait_process);
        });

        let timeout_session = session;
        let timeout_out = output;
        let timeout_life = lifecycle;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let mut life = timeout_life.lock().expect("lifecycle");
            if life.finished || life.exited || timeout_session.has_exited() {
                return;
            }
            life.note_kill(KillIntent::Timeout);
            drop(life);
            timeout_session.kill();
            after_kill(&on_kill);
            if let Ok(mut buf) = timeout_out.lock() {
                buf.timed_out = true;
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
            root: None,
            deadline_at_ms,
        })
    }

    pub async fn write_host_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        enum WriteTarget {
            Pipe(Arc<Mutex<Option<ChildStdin>>>),
            Pty(mpsc::Sender<Vec<u8>>),
        }
        let target = {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            let slot = map
                .get(&req.process_id.0)
                .ok_or_else(|| missing(&req.process_id.0))?;
            match &slot.io {
                SessionIo::Pipe { stdin, .. } => WriteTarget::Pipe(stdin.clone()),
                SessionIo::Pty { writer, .. } => WriteTarget::Pty(writer.clone()),
                SessionIo::Released => {
                    return Err(ErrorBody::new(
                        ErrorCode::ProcessNotFound,
                        "stdin is closed",
                    ))
                }
            }
        };
        match target {
            WriteTarget::Pipe(stdin) => {
                let mut pipe =
                    stdin.lock().expect("stdin").take().ok_or_else(|| {
                        ErrorBody::new(ErrorCode::ProcessNotFound, "stdin is closed")
                    })?;
                let result = async {
                    pipe.write_all(req.data.as_bytes()).await?;
                    pipe.flush().await?;
                    Ok::<(), std::io::Error>(())
                }
                .await;
                stdin.lock().expect("stdin").replace(pipe);
                result.map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))
            }
            WriteTarget::Pty(writer) => writer
                .send(req.data.into_bytes())
                .await
                .map_err(|_| ErrorBody::new(ErrorCode::ProcessNotFound, "stdin is closed")),
        }
    }

    pub fn read_host_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&req.process_id.0)
            .ok_or_else(|| missing(&req.process_id.0))?;
        let buf = slot.output.lock().expect("output");
        if buf.timed_out && req.cursor >= buf.total {
            return Err(ErrorBody::new(
                ErrorCode::Timeout,
                "managed process exceeded time limit",
            ));
        }
        let start = req.cursor.max(buf.dropped);
        let skip = (start - buf.dropped) as usize;
        let chunk = if skip >= buf.bytes.len() {
            Vec::new()
        } else {
            buf.bytes[skip..].to_vec()
        };
        let next = start + chunk.len() as u64;
        Ok(RunnerReadResult {
            process_id: req.process_id,
            cursor: next,
            chunk: String::from_utf8_lossy(&chunk).into_owned(),
            eof: buf.eof && next >= buf.total,
            output_lost: buf.dropped > 0,
            retained_from: buf.dropped,
        })
    }

    pub fn host_process_status(
        &self,
        process_id: &ProcessId,
    ) -> Result<RunnerProcessStatus, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&process_id.0)
            .ok_or_else(|| missing(&process_id.0))?;
        Ok(slot_status(process_id, slot))
    }

    /// Every process the runner holds, with its workspace and status, ended ones it still
    /// keeps included, in `process_id` order: what a gateway that claims an independent Runner
    /// inherits (P1R-C03).
    pub fn inventory(&self) -> Vec<(String, RunnerProcessStatus)> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let mut inventory: Vec<_> = map
            .iter()
            .map(|(process_id, slot)| {
                (
                    slot.workspace_id.clone(),
                    slot_status(&ProcessId(process_id.clone()), slot),
                )
            })
            .collect();
        inventory.sort_by(|a, b| a.1.process_id.0.cmp(&b.1.process_id.0));
        inventory
    }

    pub fn host_resize(
        &self,
        process_id: &ProcessId,
        rows: u16,
        cols: u16,
    ) -> Result<RunnerResizeResult, ErrorBody> {
        if rows == 0 || cols == 0 {
            return Err(ErrorBody::new(
                ErrorCode::InvalidCommand,
                "rows and cols must be at least 1",
            ));
        }
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&process_id.0)
            .ok_or_else(|| missing(&process_id.0))?;
        let snap = slot.lifecycle.lock().expect("lifecycle").snapshot;
        if snap.state != ProcessState::Running {
            return Err(ErrorBody::new(
                ErrorCode::ProcessNotRunning,
                "process is not running",
            ));
        }
        match &slot.io {
            SessionIo::Pipe { .. } => Err(ErrorBody::new(
                ErrorCode::ProcessNotTty,
                "process is not attached to a PTY",
            )),
            SessionIo::Released => Err(ErrorBody::new(
                ErrorCode::ProcessNotRunning,
                "process is not running",
            )),
            SessionIo::Pty { session, .. } => {
                if session.has_exited() {
                    return Err(ErrorBody::new(
                        ErrorCode::ProcessNotRunning,
                        "process is not running",
                    ));
                }
                session.resize(rows, cols).map_err(|err| {
                    ErrorBody::new(
                        ErrorCode::InvalidCommand,
                        format!("PTY resize failed: {err}"),
                    )
                })?;
                Ok(RunnerResizeResult { rows, cols })
            }
        }
    }

    pub fn kill_host(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let slot = map
            .get(&process_id.0)
            .ok_or_else(|| missing(&process_id.0))?;
        request_kill(slot)?;
        Ok(())
    }

    pub fn host_workspace_of(&self, process_id: &str) -> Option<String> {
        let mut map = self.inner.lock().ok()?;
        self.evict_completed(&mut map);
        map.get(process_id).map(|slot| slot.workspace_id.clone())
    }

    pub fn kill_host_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        let mut killed = 0u32;
        for slot in map.values() {
            if slot.workspace_id != workspace_id {
                continue;
            }
            if request_kill(slot)? {
                killed += 1;
            }
        }
        Ok(killed)
    }

    /// Terminate every process of every workspace, as `terminate_process` does each one: a pipe
    /// process alone, a PTY process with its process group. How many were still running that
    /// nothing had asked to end yet, through a timeout or a termination. An independent Runner
    /// does this when it stops (P1R-C01).
    pub fn terminate_all(&self) -> u32 {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        map.values()
            .filter(|slot| {
                let unasked = slot
                    .lifecycle
                    .lock()
                    .map(|life| life.kill_intent.is_none())
                    .unwrap_or(false);
                request_kill(slot).unwrap_or(false) && unasked
            })
            .count() as u32
    }

    /// Processes whose output has not ended yet: an independent Runner's next connection
    /// inherits them (P1R-C01).
    pub fn live_processes(&self) -> u32 {
        let mut map = self.inner.lock().expect("runner");
        self.evict_completed(&mut map);
        live_count(&map) as u32
    }
}

/// A pipe child's one reaper (CSRG-U5): no other path waits for it. Timeout and termination
/// record their intent and signal only a child this has not reaped yet, and a dropped child is
/// left to tokio's orphan queue only once this task is gone (shutdown).
async fn reap_child(
    child: Arc<Mutex<Child>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    process_id: &str,
    observer: Option<ExitObserver>,
) {
    loop {
        if !reaper_gate::held(process_id) {
            let exited = {
                let ch = child.lock().expect("child");
                if lifecycle.lock().expect("lifecycle").finished {
                    return;
                }
                exited_unreaped(&ch)
            };
            if exited {
                // The exited root still holds its PID: observed now, then reaped, then
                // observed again.
                observe(&observer, ExitPoint::BeforeReap).await;
                let reaped = {
                    let mut ch = child.lock().expect("child");
                    let mut life = lifecycle.lock().expect("lifecycle");
                    match ch.try_wait() {
                        Ok(Some(status)) => {
                            reaper_gate::reaped(process_id);
                            life.finish_wait(status.code(), status.signal());
                            true
                        }
                        Ok(None) => false,
                        Err(_) => {
                            life.finish_lost();
                            true
                        }
                    }
                };
                if reaped {
                    observe(&observer, ExitPoint::AfterReap).await;
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// When a timeout that starts now ends, in milliseconds since the Unix epoch (P1R-C02).
fn deadline_after(timeout: Duration) -> Option<u64> {
    std::time::SystemTime::now()
        .checked_add(timeout)?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|deadline| u64::try_from(deadline.as_millis()).ok())
}

/// Whether `child` has exited, seen without reaping it. Until its reaper waits, an exited child
/// still holds its PID, so no other process can be reached through that PID.
fn exited_unreaped(child: &Child) -> bool {
    let Some(pid) = child.id() else {
        // tokio has seen its exit.
        return true;
    };
    // SAFETY: an all-zero `siginfo_t` is valid, and zero `si_pid` is how `waitid` with
    // `WNOHANG` reports that no child has exited.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid `siginfo_t`; `WNOWAIT` leaves the child waitable for its
    // reaper and `WNOHANG` returns at once.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        // Not this process's child any more: never signal that PID.
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
    }
    // SAFETY: `waitid` filled `info`, or left the zero it was given.
    unsafe { info.si_pid() != 0 }
}

async fn join_pump(handle: Option<JoinHandle<()>>) {
    if let Some(handle) = handle {
        let _ = handle.await;
    }
}

fn request_kill(slot: &Slot) -> Result<bool, ErrorBody> {
    match &slot.io {
        SessionIo::Pipe { child, .. } => {
            let mut child = child.lock().expect("child");
            let mut life = slot.lifecycle.lock().expect("lifecycle");
            // Only the reaper waits (see `reap_child`): an exited child is left to it.
            if life.finished || exited_unreaped(&child) {
                return Ok(false);
            }
            life.note_kill(KillIntent::Terminated);
            child
                .start_kill()
                .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
            after_kill(&slot.on_kill);
            Ok(true)
        }
        SessionIo::Pty { session, .. } => {
            let mut life = slot.lifecycle.lock().expect("lifecycle");
            if life.finished || life.exited || session.has_exited() {
                return Ok(false);
            }
            life.note_kill(KillIntent::Terminated);
            session.kill();
            after_kill(&slot.on_kill);
            Ok(true)
        }
        // Only an ended process's handle is released.
        SessionIo::Released => Ok(false),
    }
}

async fn pump_chunks(mut rx: mpsc::Receiver<Vec<u8>>, output: Arc<Mutex<OutputBuf>>, cap: usize) {
    while let Some(chunk) = rx.recv().await {
        let mut out = output.lock().expect("output");
        out.total += chunk.len() as u64;
        out.bytes.extend_from_slice(&chunk);
        if out.bytes.len() > cap {
            let extra = out.bytes.len() - cap;
            out.bytes.drain(..extra);
            out.dropped += extra as u64;
        }
    }
}

async fn pump_reader<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    output: Arc<Mutex<OutputBuf>>,
    cap: usize,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let mut out = output.lock().expect("output");
                out.total += n as u64;
                out.bytes.extend_from_slice(&buf[..n]);
                if out.bytes.len() > cap {
                    let extra = out.bytes.len() - cap;
                    out.bytes.drain(..extra);
                    out.dropped += extra as u64;
                }
            }
            Err(_) => break,
        }
    }
}

/// When the process of `slot` ended: it was reaped and its output ended.
fn completed_at(slot: &Slot) -> Option<Instant> {
    slot.completed_at.lock().ok().and_then(|guard| *guard)
}

/// A process's status, from its slot.
fn slot_status(process_id: &ProcessId, slot: &Slot) -> RunnerProcessStatus {
    let snap = slot.lifecycle.lock().expect("lifecycle").snapshot;
    let buf = slot.output.lock().expect("output");
    RunnerProcessStatus {
        process_id: process_id.clone(),
        state: snap.state,
        exit_code: snap.exit_code,
        termination: snap.termination,
        signal: snap.signal.map(process_signal),
        output_total: buf.total,
        output_retained_from: buf.dropped,
        eof: buf.eof,
    }
}

fn live_count(map: &HashMap<String, Slot>) -> usize {
    map.values()
        .filter(|slot| slot.output.lock().map(|buf| !buf.eof).unwrap_or(false))
        .count()
}

/// A signal number of this host, named when it is one of the standard signals.
pub fn process_signal(number: i32) -> ProcessSignal {
    const NAMES: [(i32, &str); 21] = [
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
        (libc::SIGVTALRM, "SIGVTALRM"),
        (libc::SIGPROF, "SIGPROF"),
        (libc::SIGIO, "SIGIO"),
        (libc::SIGSYS, "SIGSYS"),
    ];
    ProcessSignal {
        number,
        name: NAMES
            .iter()
            .find(|(known, _)| *known == number)
            .map(|(_, name)| (*name).to_owned()),
    }
}

fn missing(id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ProcessNotFound,
        format!("unknown process_id `{id}`"),
    )
}

struct ExecLaunch {
    program: PathBuf,
    args: Vec<OsString>,
    sandboxed: bool,
    plan_path: Option<PathBuf>,
}

impl ExecLaunch {
    fn abort_plan(&self) {
        if let Some(path) = &self.plan_path {
            linux_sandbox::discard_plan(path);
        }
    }
}

/// Wrap user argv with the Linux helper when [`linux_sandbox::probe`]
/// succeeded. Probe failure keeps direct user argv. Probe success never
/// unsandboxes on a later setup/spawn error. Managed argv is
/// `helper run --plan`; Codex translation happens inside that process.
fn exec_launch(ws: &Workspace, req: &RunnerExecRequest) -> Result<ExecLaunch, ErrorBody> {
    if !linux_sandbox::probe() {
        if matches!(req.policy.network, NetworkAxis::Enabled) {
            return Err(ErrorBody::new(
                ErrorCode::ProcessSpawnFailed,
                "Enabled network requires the Linux command sandbox helper",
            ));
        }
        return Ok(ExecLaunch {
            program: PathBuf::from(&req.argv[0]),
            args: req.argv.iter().skip(1).map(OsString::from).collect(),
            sandboxed: false,
            plan_path: None,
        });
    }
    let network = sandbox_network(req.policy.network);
    let writable_workspace = matches!(req.policy.workspace_profile, Profile::WorkspaceWrite);
    let launch =
        linux_sandbox::prepare_run(&ws.root, &ws.root, writable_workspace, network, &req.argv)?;
    Ok(ExecLaunch {
        program: launch.program,
        args: launch.args,
        sandboxed: true,
        plan_path: Some(launch.plan_path),
    })
}

fn sandbox_network(network: NetworkAxis) -> SandboxNetwork {
    match network {
        NetworkAxis::Restricted => SandboxNetwork::Restricted,
        NetworkAxis::Enabled => SandboxNetwork::Enabled,
    }
}

pub(crate) fn spawn_env(
    cwd: &Path,
    req: &RunnerExecRequest,
    sandboxed: bool,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if req.env.use_runner_defaults {
        let defaults = if sandboxed {
            sandbox_exec_env(cwd)
        } else {
            runner_local_exec_env(cwd)
        };
        env.extend(defaults);
    }
    for (key, value) in &req.env.overrides {
        env.insert(key.clone(), value.clone());
    }
    env
}

fn utf8_launch(program: &Path, args: &[OsString]) -> Result<(String, Vec<String>), ErrorBody> {
    let program = program.to_str().ok_or_else(|| {
        ErrorBody::new(
            ErrorCode::ProcessSpawnFailed,
            "sandbox helper path is not UTF-8",
        )
    })?;
    let args = args
        .iter()
        .map(|arg| {
            arg.to_str().map(str::to_string).ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::ProcessSpawnFailed,
                    "sandbox helper argument is not UTF-8",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((program.to_string(), args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runner;
    use codespace_domain::{ProcessId, Profile, WorkspaceId};
    use codespace_policy::Workspace;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    fn isatty_argv() -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "if [ -t 0 ]; then echo ISATTY; else echo NOTTY; fi".into(),
        ]
    }

    async fn wait_chunk(runner: &InProcessRunner, process_id: &ProcessId) -> String {
        let mut chunk = String::new();
        for _ in 0..50 {
            let result = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            chunk = result.chunk;
            if result.eof {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        chunk
    }

    async fn wait_exited(runner: &InProcessRunner, process_id: &ProcessId) -> RunnerProcessStatus {
        for _ in 0..250 {
            let status = runner.process_status(process_id).await.unwrap();
            if status.state == ProcessState::Exited {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process did not exit: {}", process_id.0)
    }

    /// The marker file `n`'s command would create, and that command.
    fn marker(root: &std::path::Path, n: usize) -> (std::path::PathBuf, RunnerExecRequest) {
        let path = root.join(format!("started-{n}"));
        let req = RunnerExecRequest::for_host(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("touch '{}'; sleep 30", path.display()),
            ],
            ProcessId(format!("proc-limit-{n}")),
            Profile::WorkspaceWrite,
        );
        (path, req)
    }

    /// Whether `path` appears within half a second.
    async fn appears(path: &std::path::Path) -> bool {
        for _ in 0..50 {
            if path.exists() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_request_over_the_limit_creates_no_child() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {})).with_max_processes(8);
        for n in 0..8 {
            let (path, req) = marker(dir.path(), n);
            runner.spawn_host(&ws, req).await.unwrap();
            assert!(appears(&path).await, "process {n} did not start");
        }
        // The ninth is refused before anything is created: it never runs, even briefly.
        let (ninth, req) = marker(dir.path(), 8);
        let refused = runner.spawn_host(&ws, req).await.unwrap_err();
        assert_eq!(
            (refused.code, refused.message.as_str()),
            (ErrorCode::WorkspaceBusy, "live process limit reached")
        );
        assert!(
            !appears(&ninth).await,
            "the refused request created a child"
        );
        assert!(runner.host_workspace_of("proc-limit-8").is_none());
        assert_eq!(runner.occupied_slots(), 8);
        // A slot held for a process not yet created counts too.
        runner.kill_host_workspace("demo").unwrap();
        for _ in 0..200 {
            if runner.occupied_slots() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let small = InProcessRunner::new(Arc::new(|_| {})).with_max_processes(1);
        let held = small.reserve_slot("proc-held").unwrap();
        let (blocked, req) = marker(dir.path(), 9);
        assert_eq!(
            small.spawn_host(&ws, req).await.unwrap_err().code,
            ErrorCode::WorkspaceBusy
        );
        assert!(!appears(&blocked).await);
        // A slot dropped unfilled frees itself.
        drop(held);
        assert_eq!(small.occupied_slots(), 0);
        let (freed, req) = marker(dir.path(), 10);
        small.spawn_host(&ws, req).await.unwrap();
        assert!(appears(&freed).await);
        small.kill_host_workspace("demo").unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_requests_over_the_limit_create_only_the_children_allowed() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {})).with_max_processes(3);
        let spawns: Vec<_> = (0..12)
            .map(|n| {
                let (runner, ws) = (runner.clone(), ws.clone());
                let (_, req) = marker(dir.path(), n);
                tokio::spawn(async move { runner.spawn_host(&ws, req).await })
            })
            .collect();
        let mut started = 0;
        for spawn in spawns {
            match spawn.await.unwrap() {
                Ok(_) => started += 1,
                Err(err) => assert_eq!(err.code, ErrorCode::WorkspaceBusy),
            }
        }
        assert_eq!(started, 3);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let created = (0..12)
            .filter(|n| dir.path().join(format!("started-{n}")).exists())
            .count();
        assert_eq!(created, 3, "a refused request created a child");
        runner.kill_host_workspace("demo").unwrap();
    }

    fn workspace(root: &std::path::Path) -> Workspace {
        Workspace::new(
            WorkspaceId("demo".into()),
            root.to_path_buf(),
            Profile::WorkspaceWrite,
        )
    }

    const REQUIRE_ENV: &str = "CODESPACE_REQUIRE_LINUX_SANDBOX";

    fn require_linux_sandbox() -> bool {
        std::env::var_os(REQUIRE_ENV).is_some_and(|value| value == "1")
    }

    #[test]
    fn require_env_defaults_off() {
        assert!(!require_linux_sandbox());
    }

    #[tokio::test]
    async fn enabled_network_is_not_silently_restricted() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let process_id = ProcessId("proc-enabled-net".into());
        let mut req = RunnerExecRequest::for_host(
            vec!["/bin/echo".into(), "ok".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.policy.network = NetworkAxis::Enabled;
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        if !crate::linux_sandbox_available() {
            let err = runner.exec(&ws, req).await.unwrap_err();
            assert_eq!(
                err.as_execution().map(|body| body.code),
                Some(ErrorCode::ProcessSpawnFailed)
            );
            let restricted = exec_launch(
                &ws,
                &RunnerExecRequest::for_host(
                    vec!["/bin/true".into()],
                    ProcessId("proc-restricted-fallback".into()),
                    Profile::WorkspaceWrite,
                ),
            )
            .unwrap();
            assert!(!restricted.sandboxed);
            return;
        }
        runner.exec(&ws, req).await.unwrap();
        let mut chunk = String::new();
        for _ in 0..200 {
            let result = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            chunk = result.chunk;
            if result.eof || chunk.contains("ok") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            chunk.contains("ok"),
            "Enabled network must spawn through the helper, got {chunk:?}"
        );
        assert_eq!(
            sandbox_network(NetworkAxis::Enabled),
            SandboxNetwork::Enabled
        );
    }

    #[test]
    fn linux_ci_requires_sandbox_probe() {
        if !require_linux_sandbox() {
            return;
        }

        #[cfg(not(target_os = "linux"))]
        panic!("CODESPACE_REQUIRE_LINUX_SANDBOX=1 is Linux CI only");

        #[cfg(target_os = "linux")]
        assert!(
            crate::linux_sandbox_available(),
            "CODESPACE_REQUIRE_LINUX_SANDBOX=1 but linux sandbox helper probe failed"
        );
    }

    #[tokio::test]
    async fn completed_handles_evict_after_ttl() {
        let dir = tempdir().unwrap();
        let released = Arc::new(AtomicUsize::new(0));
        let flag = released.clone();
        let runner = InProcessRunner::with_retention(
            Arc::new(move |_| {
                flag.fetch_add(1, Ordering::SeqCst);
            }),
            RetentionPolicy {
                ttl: Duration::from_millis(150),
                max_completed: 64,
            },
        );
        let ws = workspace(dir.path());
        let process_id = ProcessId("proc-ttl".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "hi".into()],
                    process_id.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if released.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(released.load(Ordering::SeqCst) >= 1);
        runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let err = runner
            .read_process(RunnerReadProcess {
                process_id,
                cursor: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
    }

    #[tokio::test]
    async fn completed_handles_evict_by_max_count() {
        let dir = tempdir().unwrap();
        let runner = InProcessRunner::with_retention(
            Arc::new(|_| {}),
            RetentionPolicy {
                ttl: Duration::from_secs(60),
                max_completed: 1,
            },
        );
        let ws = workspace(dir.path());
        let first = ProcessId("proc-old".into());
        let second = ProcessId("proc-new".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "one".into()],
                    first.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(RunnerReadProcess {
                    process_id: first.clone(),
                    cursor: 0,
                })
                .await
                .unwrap()
                .eof
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "two".into()],
                    second.clone(),
                    codespace_domain::Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        for _ in 0..50 {
            if runner
                .read_process(RunnerReadProcess {
                    process_id: second.clone(),
                    cursor: 0,
                })
                .await
                .unwrap()
                .eof
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Trigger eviction of the oldest completed slot.
        let _ = runner
            .read_process(RunnerReadProcess {
                process_id: second,
                cursor: 0,
            })
            .await;
        let err = runner
            .read_process(RunnerReadProcess {
                process_id: first,
                cursor: 0,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
    }

    #[tokio::test]
    async fn linux_container_environment_is_closed_failure() {
        let dir = tempdir().unwrap();
        let mut ws = workspace(dir.path());
        ws.environment_kind = codespace_policy::EnvironmentKind::LinuxContainer;
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let err = runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into()],
                    ProcessId("proc-box".into()),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[tokio::test]
    async fn empty_argv_is_invalid_command() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let err = runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec![],
                    ProcessId("proc-empty".into()),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::InvalidCommand)
        );
    }

    #[tokio::test]
    async fn missing_executable_is_process_spawn_failed() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-missing".into());
        let result = runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/no/such/codespace-exec".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await;
        if crate::linux_sandbox_available() {
            // Helper spawn succeeded; the inner command failed inside bwrap.
            result.expect("sandboxed helper spawn");
            let _ = wait_chunk(&runner, &process_id).await;
        } else {
            let err = result.unwrap_err();
            assert_eq!(
                err.as_execution().map(|body| body.code),
                Some(ErrorCode::ProcessSpawnFailed)
            );
        }
    }

    #[tokio::test]
    async fn tty_missing_executable_is_process_spawn_failed() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-tty-missing".into());
        let mut req = RunnerExecRequest::for_host(
            vec!["/no/such/codespace-exec".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        let result = runner.exec(&ws, req).await;
        if crate::linux_sandbox_available() {
            result.expect("sandboxed helper spawn");
            let _ = wait_chunk(&runner, &process_id).await;
        } else {
            let err = result.unwrap_err();
            assert_eq!(
                err.as_execution().map(|body| body.code),
                Some(ErrorCode::ProcessSpawnFailed)
            );
        }
    }

    #[tokio::test]
    async fn tty_true_stdin_is_a_tty() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-tty".into());
        let mut req =
            RunnerExecRequest::for_host(isatty_argv(), process_id.clone(), Profile::WorkspaceWrite);
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("ISATTY"),
            "tty:true should see a TTY, got {chunk:?}"
        );
        assert!(!chunk.contains("NOTTY"), "chunk={chunk:?}");
    }

    #[tokio::test]
    async fn tty_false_stdin_is_a_pipe() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pipe".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    isatty_argv(),
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("NOTTY"),
            "omitted/false tty should be a pipe, got {chunk:?}"
        );
    }

    #[tokio::test]
    async fn tty_write_stdin_roundtrip() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pty-io".into());
        let mut req = RunnerExecRequest::for_host(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "IFS= read -r line; printf 'got:%s\\n' \"$line\"".into(),
            ],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        runner
            .write_stdin(RunnerWriteStdin {
                process_id: process_id.clone(),
                data: "hello\n".into(),
            })
            .await
            .unwrap();
        let chunk = wait_chunk(&runner, &process_id).await;
        assert!(
            chunk.contains("hello"),
            "PTY write_stdin/read_process roundtrip, got {chunk:?}"
        );
    }

    #[tokio::test]
    async fn pty_resize_updates_stty_size() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pty-resize".into());
        let mut req = RunnerExecRequest::for_host(
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "stty -echo; printf 'start:%s\\n' \"$(stty size)\"; IFS= read _line; printf 'after:%s\\n' \"$(stty size)\"".into(),
            ],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        let mut chunk = String::new();
        for _ in 0..50 {
            let result = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            chunk = result.chunk.replace("\r\n", "\n");
            if chunk.contains("start:24 80") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert!(
            chunk.contains("start:24 80"),
            "initial PTY size, got {chunk:?}"
        );
        let resized = runner.resize(&process_id, 40, 120).await.expect("resize");
        assert_eq!(resized.rows, 40);
        assert_eq!(resized.cols, 120);
        runner
            .write_stdin(RunnerWriteStdin {
                process_id: process_id.clone(),
                data: "go\n".into(),
            })
            .await
            .unwrap();
        chunk = wait_chunk(&runner, &process_id).await.replace("\r\n", "\n");
        assert!(
            chunk.contains("after:40 120"),
            "resized PTY size, got {chunk:?}"
        );
    }

    #[tokio::test]
    async fn pipe_resize_is_process_not_tty() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pipe-resize".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/sleep".into(), "30".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let err = runner.resize(&process_id, 40, 120).await.unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotTty)
        );
        runner.kill_host(&process_id).unwrap();
        let _ = wait_exited(&runner, &process_id).await;
    }

    #[tokio::test]
    async fn resize_unknown_and_exited_handles() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let missing = runner
            .resize(&ProcessId("proc-missing-resize".into()), 24, 80)
            .await
            .unwrap_err();
        assert_eq!(
            missing.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
        let zero = runner
            .resize(&ProcessId("proc-missing-resize".into()), 0, 80)
            .await
            .unwrap_err();
        assert_eq!(
            zero.as_execution().map(|body| body.code),
            Some(ErrorCode::InvalidCommand)
        );
        let process_id = ProcessId("proc-exited-resize".into());
        let mut req = RunnerExecRequest::for_host(
            vec!["/bin/echo".into(), "done".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        let _ = wait_exited(&runner, &process_id).await;
        let err = runner.resize(&process_id, 40, 120).await.unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::ProcessNotRunning)
        );
    }

    #[tokio::test]
    async fn terminate_reaps_sandboxed_sleep() {
        if !crate::linux_sandbox_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-sandbox-sleep".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/sleep".into(), "30".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        runner.kill_host(&process_id).unwrap();
        let mut eof = false;
        for _ in 0..50 {
            let result = runner
                .read_process(RunnerReadProcess {
                    process_id: process_id.clone(),
                    cursor: 0,
                })
                .await
                .unwrap();
            if result.eof {
                eof = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(eof, "SIGTERM on the helper must reap the sandbox tree");
    }

    #[tokio::test]
    async fn pipe_child_keeps_only_its_standard_descriptors() {
        // A descriptor that another thread's creation window could leave inheritable (#79).
        let held = crate::descriptors::tests::inheritable_descriptor();
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-descriptors".into());
        let script =
            crate::descriptors::tests::report_script(std::os::fd::AsRawFd::as_raw_fd(&held));
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/sh".into(), "-c".into(), script],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        assert_eq!(wait_chunk(&runner, &process_id).await, "held\nclear\n");
    }

    #[tokio::test]
    async fn echo_status_is_exited_zero() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-echo-status".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/echo".into(), "hi".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.state, ProcessState::Exited);
        assert_eq!(status.termination, Some(ProcessTermination::Exited));
        assert_eq!(status.exit_code, Some(0));
        assert!(status.signal.is_none());
        assert!(status.eof);
        let read = runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        assert!(!read.output_lost);
        assert_eq!(read.retained_from, 0);
    }

    #[tokio::test]
    async fn false_status_is_exited_one() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-false-status".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/usr/bin/false".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.state, ProcessState::Exited);
        assert_eq!(status.termination, Some(ProcessTermination::Exited));
        assert_eq!(status.exit_code, Some(1));
        assert!(status.signal.is_none());
    }

    #[tokio::test]
    async fn timeout_status_has_no_exit_code() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-timeout-status".into());
        let mut req = RunnerExecRequest::for_host(
            vec!["/bin/sleep".into(), "30".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.timeout_ms = 50;
        runner.exec(&ws, req).await.unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.state, ProcessState::Exited);
        assert_eq!(status.termination, Some(ProcessTermination::Timeout));
        assert!(status.exit_code.is_none());
        assert!(status.signal.is_none());
        let err = runner
            .read_process(RunnerReadProcess {
                process_id,
                cursor: status.output_total,
            })
            .await
            .unwrap_err();
        assert_eq!(
            err.as_execution().map(|body| body.code),
            Some(ErrorCode::Timeout)
        );
    }

    /// Wait until `process_id`'s pipe child has exited, while its reaper is held.
    async fn exited_but_unreaped(runner: &InProcessRunner, process_id: &ProcessId) {
        for _ in 0..250 {
            let exited = {
                let map = runner.inner.lock().unwrap();
                match &map[&process_id.0].io {
                    SessionIo::Pipe { child, .. } => exited_unreaped(&child.lock().unwrap()),
                    _ => panic!("{} is not a pipe child", process_id.0),
                }
            };
            if exited {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{} did not exit", process_id.0);
    }

    fn held_request(argv: &[&str], process_id: &ProcessId, timeout_ms: u64) -> RunnerExecRequest {
        reaper_gate::hold(&process_id.0);
        let mut req = RunnerExecRequest::for_host(
            argv.iter().map(|arg| arg.to_string()).collect(),
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.timeout_ms = timeout_ms;
        req
    }

    /// Spawn `argv` with `observer`, as a governed execution's spawn does.
    async fn spawn_observed(
        runner: &InProcessRunner,
        ws: &Workspace,
        process_id: &ProcessId,
        argv: &[&str],
        tty: bool,
        observer: ExitObserver,
    ) {
        let mut req = RunnerExecRequest::for_host(
            argv.iter().map(|arg| arg.to_string()).collect(),
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = tty;
        let slot = runner.reserve_slot(&process_id.0).unwrap();
        runner
            .spawn_slotted(
                ws,
                req,
                slot,
                How::Direct,
                ProcessHooks {
                    observer: Some(observer),
                    on_kill: None,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_observer_sees_the_exited_root_unreaped_then_reaped() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-observed-pipe".into());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer: ExitObserver = {
            let (runner, process_id, seen) = (runner.clone(), process_id.clone(), seen.clone());
            Arc::new(move |point| {
                let unreaped = {
                    let map = runner.inner.lock().unwrap();
                    match &map[&process_id.0].io {
                        SessionIo::Pipe { child, .. } => {
                            let child = child.lock().unwrap();
                            child.id().is_some() && exited_unreaped(&child)
                        }
                        _ => panic!("not a pipe child"),
                    }
                };
                seen.lock().unwrap().push((point, unreaped));
            })
        };
        spawn_observed(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "exit 4"],
            false,
            observer,
        )
        .await;
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.exit_code, Some(4));
        for _ in 0..250 {
            if seen.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [(ExitPoint::BeforeReap, true), (ExitPoint::AfterReap, false)]
        );
    }

    #[tokio::test]
    async fn an_observer_that_does_not_return_does_not_keep_the_child_unreaped() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-observed-slow".into());
        let observer: ExitObserver = Arc::new(|_| std::thread::sleep(Duration::from_secs(3)));
        let started = Instant::now();
        spawn_observed(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "exit 5"],
            false,
            observer,
        )
        .await;
        let status = wait_exited(&runner, &process_id).await;
        // The reaper waited at most its bound for the observer, then reaped, once.
        assert!(started.elapsed() < OBSERVE_BOUND + Duration::from_millis(900));
        assert_eq!(status.exit_code, Some(5));
        assert_eq!(reaper_gate::reaps(&process_id.0), 1);
    }

    /// Spawn `argv` with `hooks` and a timeout of `timeout_ms`.
    async fn spawn_hooked(
        runner: &InProcessRunner,
        ws: &Workspace,
        process_id: &ProcessId,
        argv: &[&str],
        tty: bool,
        timeout_ms: u64,
        hooks: ProcessHooks,
    ) {
        let mut req = RunnerExecRequest::for_host(
            argv.iter().map(|arg| arg.to_string()).collect(),
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = tty;
        req.timeout_ms = timeout_ms;
        let slot = runner.reserve_slot(&process_id.0).unwrap();
        runner
            .spawn_slotted(ws, req, slot, How::Direct, hooks)
            .await
            .unwrap();
    }

    /// Whether the pinned Codex PTY's waiter has reaped `process_id`'s root.
    fn pty_reaped(runner: &InProcessRunner, process_id: &ProcessId) -> bool {
        let map = runner.inner.lock().unwrap();
        match &map[&process_id.0].io {
            SessionIo::Pty { session, .. } => session.has_exited(),
            _ => panic!("{} is not a PTY child", process_id.0),
        }
    }

    /// A PTY child with an observer gets a pipe root's window before the reap (CSRG-U5): the
    /// patched Codex waiter offers it while the exited root is unreaped, and reaps it once the
    /// observer has returned.
    #[tokio::test]
    async fn a_pty_childs_observer_sees_the_exited_root_unreaped_then_reaped() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-observed-pty".into());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer: ExitObserver = {
            let (runner, process_id, seen) = (runner.clone(), process_id.clone(), seen.clone());
            Arc::new(move |point| {
                let reaped = pty_reaped(&runner, &process_id);
                seen.lock().unwrap().push((point, reaped));
            })
        };
        spawn_observed(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "exit 4"],
            true,
            observer,
        )
        .await;
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.exit_code, Some(4));
        for _ in 0..250 {
            if seen.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // (where, reaped): unreaped in the window, reaped after it.
        assert_eq!(
            *seen.lock().unwrap(),
            [(ExitPoint::BeforeReap, false), (ExitPoint::AfterReap, true)]
        );
    }

    /// As a pipe root's: an observer that does not return holds a PTY root at most the bound.
    #[tokio::test]
    async fn a_pty_observer_that_does_not_return_does_not_keep_the_child_unreaped() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-observed-pty-slow".into());
        let observer: ExitObserver = Arc::new(|_| std::thread::sleep(Duration::from_secs(3)));
        let started = Instant::now();
        spawn_observed(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "exit 5"],
            true,
            observer,
        )
        .await;
        let status = wait_exited(&runner, &process_id).await;
        assert!(started.elapsed() < OBSERVE_BOUND + Duration::from_millis(900));
        assert_eq!(status.exit_code, Some(5));
    }

    /// An observer that fails releases the exited root at once, on a pipe and on a PTY.
    #[tokio::test]
    async fn a_failing_observer_does_not_hold_the_child() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        for tty in [false, true] {
            let process_id = ProcessId(format!("proc-observer-fails-{tty}"));
            let observer: ExitObserver = Arc::new(|point| {
                if point == ExitPoint::BeforeReap {
                    panic!("the observer failed");
                }
            });
            let started = Instant::now();
            spawn_observed(
                &runner,
                &ws,
                &process_id,
                &["/bin/sh", "-c", "exit 6"],
                tty,
                observer,
            )
            .await;
            let status = wait_exited(&runner, &process_id).await;
            assert!(started.elapsed() < Duration::from_secs(1), "tty={tty}");
            assert_eq!(status.exit_code, Some(6), "tty={tty}");
        }
    }

    /// As a pipe root's (CSRG-U5): termination and the timeout while a PTY root waits in its
    /// window neither kill it nor change how it ended, and its reaper reaps it once released.
    #[tokio::test]
    async fn an_exited_pty_child_keeps_how_it_ended_whatever_else_acts_on_it() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-window-race".into());
        let (opened, window_open) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(released);
        let observer: ExitObserver = Arc::new(move |point| {
            if point == ExitPoint::BeforeReap {
                let _ = opened.send(());
                let _ = released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            }
        });
        let kills = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = kills.clone();
        spawn_hooked(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "exit 3"],
            true,
            300,
            ProcessHooks {
                observer: Some(observer),
                on_kill: Some(Arc::new(move || {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })),
            },
        )
        .await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while window_open.try_recv().is_err() {
            assert!(Instant::now() < deadline, "the window did not open");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runner.terminate(&process_id).await.unwrap();
        // The 300 ms timeout passes while the root is in its window.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!pty_reaped(&runner, &process_id));
        let status = runner.process_status(&process_id).await.unwrap();
        assert_eq!(status.state, ProcessState::Running);
        release.send(()).unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(
            (status.exit_code, status.termination),
            (Some(3), Some(ProcessTermination::Exited))
        );
        assert_eq!(kills.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A PTY root's output drains in full while the exited root waits in its window.
    #[tokio::test]
    async fn a_pty_childs_output_drains_while_it_is_unreaped() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-window-output".into());
        let observer: ExitObserver = Arc::new(|point| {
            if point == ExitPoint::BeforeReap {
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        spawn_observed(
            &runner,
            &ws,
            &process_id,
            &["/bin/sh", "-c", "head -c 200000 /dev/zero | tr '\\0' x"],
            true,
            observer,
        )
        .await;
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.exit_code, Some(0));
        let output = wait_chunk(&runner, &process_id).await;
        assert_eq!(output.matches('x').count(), 200_000);
        let status = runner.process_status(&process_id).await.unwrap();
        assert_eq!(status.output_retained_from, 0);
    }

    /// Spawn `argv` with a kill hook that counts its calls.
    async fn spawn_counting_kills(
        runner: &InProcessRunner,
        ws: &Workspace,
        process_id: &ProcessId,
        argv: &[&str],
        tty: bool,
        timeout_ms: u64,
    ) -> Arc<std::sync::atomic::AtomicUsize> {
        let kills = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut req = RunnerExecRequest::for_host(
            argv.iter().map(|arg| arg.to_string()).collect(),
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = tty;
        req.timeout_ms = timeout_ms;
        let slot = runner.reserve_slot(&process_id.0).unwrap();
        let counted = kills.clone();
        runner
            .spawn_slotted(
                ws,
                req,
                slot,
                How::Direct,
                ProcessHooks {
                    observer: None,
                    on_kill: Some(Arc::new(move || {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    })),
                },
            )
            .await
            .unwrap();
        kills
    }

    async fn counted(kills: &std::sync::atomic::AtomicUsize, expected: usize) -> usize {
        for _ in 0..100 {
            if kills.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Long enough for a call that should not happen.
        tokio::time::sleep(Duration::from_millis(100)).await;
        kills.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn the_kill_hook_runs_once_for_each_kill_codespace_makes() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        for tty in [false, true] {
            // A termination kills it once; a second one finds it gone.
            let process_id = ProcessId(format!("proc-kill-hook-terminated-{tty}"));
            let kills = spawn_counting_kills(
                &runner,
                &ws,
                &process_id,
                &["/bin/sleep", "30"],
                tty,
                30_000,
            )
            .await;
            runner.terminate(&process_id).await.unwrap();
            let status = wait_exited(&runner, &process_id).await;
            assert_eq!(status.termination, Some(ProcessTermination::Terminated));
            runner.terminate(&process_id).await.unwrap();
            assert_eq!(counted(&kills, 1).await, 1, "tty={tty}");
            // The timeout kills it once.
            let process_id = ProcessId(format!("proc-kill-hook-timeout-{tty}"));
            let kills =
                spawn_counting_kills(&runner, &ws, &process_id, &["/bin/sleep", "30"], tty, 100)
                    .await;
            let status = wait_exited(&runner, &process_id).await;
            assert_eq!(status.termination, Some(ProcessTermination::Timeout));
            assert_eq!(counted(&kills, 1).await, 1, "tty={tty}");
            // A process that ended on its own is not killed.
            let process_id = ProcessId(format!("proc-kill-hook-exited-{tty}"));
            let kills = spawn_counting_kills(
                &runner,
                &ws,
                &process_id,
                &["/bin/sh", "-c", "exit 0"],
                tty,
                30_000,
            )
            .await;
            wait_exited(&runner, &process_id).await;
            runner.terminate(&process_id).await.unwrap();
            assert_eq!(counted(&kills, 0).await, 0, "tty={tty}");
        }
    }

    /// At shutdown each child's reaper and handles go with the runtime (CSRG-U5): a running
    /// child is killed, and it, like a child that exited just before and that CodeSpace has not
    /// reaped yet, is left to tokio's orphan queue, which reaps it once. Nothing outlives the
    /// runner. A governed child is then not observed before that reap.
    #[test]
    fn a_shutdown_leaves_each_child_to_one_reaper() {
        for (name, argv, exited) in [
            ("proc-shutdown-running", &["/bin/sleep", "30"][..], false),
            (
                "proc-shutdown-exited",
                &["/bin/sh", "-c", "exit 0"][..],
                true,
            ),
        ] {
            let dir = tempdir().unwrap();
            let ws = workspace(dir.path());
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let process_id = ProcessId(name.into());
            let pid = runtime.block_on(async {
                let runner = InProcessRunner::new(Arc::new(|_| {}));
                let req = RunnerExecRequest::for_host(
                    argv.iter().map(|arg| arg.to_string()).collect(),
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                );
                if exited {
                    // Its reaper is held, so the runtime goes while it is exited and unreaped.
                    reaper_gate::hold(&process_id.0);
                }
                runner.exec(&ws, req).await.unwrap();
                if exited {
                    exited_but_unreaped(&runner, &process_id).await;
                }
                pipe_child_pid(&runner, &process_id)
            });
            drop(runtime);
            reaper_gate::release(&process_id.0);
            assert_eq!(reaper_gate::reaps(&process_id.0), 0, "{name}");
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // SAFETY: an all-zero `siginfo_t` is valid; `WNOWAIT` leaves the child waitable.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                // SAFETY: `info` is a valid `siginfo_t` for `waitid` to fill.
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                // Exited and not yet reaped, or already reaped by the orphan queue.
                // SAFETY: `waitid` filled `info`, or left the zero it was given.
                if (result == 0 && unsafe { info.si_pid() } != 0) || result == -1 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{name}: {pid} outlived the runner"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// Root exit, output EOF and the workspace's release are separate events (CSRG-U5): a
    /// descendant that keeps the output open delays the EOF and the release, not the root's
    /// exit.
    #[tokio::test]
    async fn root_exit_output_eof_and_release_are_separate_events() {
        // The Linux command sandbox's helper outlives the command's descendants.
        if crate::linux_sandbox_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let released = Arc::new(Mutex::new(Vec::<String>::new()));
        let runner = {
            let released = released.clone();
            InProcessRunner::new(Arc::new(move |id: &str| {
                released.lock().unwrap().push(id.to_owned())
            }))
        };
        let process_id = ProcessId("proc-separate-events".into());
        let req = RunnerExecRequest::for_host(
            vec!["/bin/sh".into(), "-c".into(), "sleep 1 & echo root".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        runner.exec(&ws, req).await.unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.exit_code, Some(0));
        // The root has exited; its descendant still holds the output open.
        let read = runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        assert!(!read.eof);
        assert!(released.lock().unwrap().is_empty());
        // Once the descendant has ended, the output ends and the workspace is released.
        for _ in 0..250 {
            if !released.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let read = runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        assert!(read.eof);
        assert_eq!(read.chunk, "root\n");
        assert_eq!(*released.lock().unwrap(), [process_id.0]);
    }

    #[tokio::test]
    async fn an_exited_child_keeps_its_one_reaper_whatever_else_acts_on_it() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-one-reaper-exit".into());
        let req = held_request(&["/bin/sh", "-c", "exit 3"], &process_id, 300);
        runner.exec(&ws, req).await.unwrap();
        exited_but_unreaped(&runner, &process_id).await;
        // Termination and the timeout come while the exited child is still unreaped: neither
        // waits for it, kills it or changes how it ended.
        runner.terminate(&process_id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(reaper_gate::reaps(&process_id.0), 0);
        let status = runner.process_status(&process_id).await.unwrap();
        assert_eq!(status.state, ProcessState::Running);
        reaper_gate::release(&process_id.0);
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(
            (status.exit_code, status.termination),
            (Some(3), Some(ProcessTermination::Exited))
        );
        assert_eq!(reaper_gate::reaps(&process_id.0), 1);
    }

    #[tokio::test]
    async fn a_terminated_child_keeps_its_one_reaper() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-one-reaper-terminated".into());
        let req = held_request(&["/bin/sleep", "30"], &process_id, 30_000);
        runner.exec(&ws, req).await.unwrap();
        runner.terminate(&process_id).await.unwrap();
        exited_but_unreaped(&runner, &process_id).await;
        // A second termination finds it exited and leaves it to the reaper.
        runner.terminate(&process_id).await.unwrap();
        assert_eq!(reaper_gate::reaps(&process_id.0), 0);
        reaper_gate::release(&process_id.0);
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.termination, Some(ProcessTermination::Terminated));
        assert_eq!(reaper_gate::reaps(&process_id.0), 1);
    }

    #[tokio::test]
    async fn a_timed_out_child_keeps_its_one_reaper() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-one-reaper-timeout".into());
        let req = held_request(&["/bin/sleep", "30"], &process_id, 100);
        runner.exec(&ws, req).await.unwrap();
        exited_but_unreaped(&runner, &process_id).await;
        // Termination after the timeout's kill finds it exited and leaves it to the reaper.
        runner.terminate(&process_id).await.unwrap();
        assert_eq!(reaper_gate::reaps(&process_id.0), 0);
        reaper_gate::release(&process_id.0);
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.termination, Some(ProcessTermination::Timeout));
        assert_eq!(reaper_gate::reaps(&process_id.0), 1);
    }

    #[tokio::test]
    async fn terminate_status_has_no_exit_code() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-term-status".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/sleep".into(), "30".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        runner.kill_host(&process_id).unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.state, ProcessState::Exited);
        assert_eq!(status.termination, Some(ProcessTermination::Terminated));
        assert!(status.exit_code.is_none());
        // CodeSpace's own SIGKILL is not reported as a signal.
        assert!(status.signal.is_none());
    }

    fn pipe_child_pid(runner: &InProcessRunner, process_id: &ProcessId) -> libc::pid_t {
        let map = runner.inner.lock().unwrap();
        match &map[&process_id.0].io {
            SessionIo::Pipe { child, .. } => {
                let pid = child.lock().unwrap().id().expect("running child");
                libc::pid_t::try_from(pid).unwrap()
            }
            _ => panic!("{} is not a pipe child", process_id.0),
        }
    }

    #[tokio::test]
    async fn external_signal_status_names_the_signal() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-signal-status".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec!["/bin/sleep".into(), "30".into()],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        // A kill that CodeSpace did not request, as from another process. The
        // managed child is the one that dies, with or without the Linux helper.
        let pid = pipe_child_pid(&runner, &process_id);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let status = wait_exited(&runner, &process_id).await;
        assert_eq!(status.state, ProcessState::Exited);
        assert_eq!(status.termination, Some(ProcessTermination::Signaled));
        assert!(status.exit_code.is_none());
        assert_eq!(
            status.signal,
            Some(ProcessSignal {
                number: libc::SIGKILL,
                name: Some("SIGKILL".into()),
            })
        );
    }

    #[tokio::test]
    async fn pty_signal_death_is_reported_as_exit_code_one() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-pty-signal".into());
        let mut req = RunnerExecRequest::for_host(
            vec!["/bin/sh".into(), "-c".into(), "kill -KILL $$".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = true;
        runner.exec(&ws, req).await.unwrap();
        let status = wait_exited(&runner, &process_id).await;
        // The pinned Codex PTY turns a signal death into exit code 1, so the
        // signal is not visible on this path (#85).
        assert_eq!(status.termination, Some(ProcessTermination::Exited));
        assert!(status.signal.is_none());
        if !crate::linux_sandbox_available() {
            assert_eq!(status.exit_code, Some(1));
        }
    }

    #[test]
    fn signal_names_follow_the_host_numbers() {
        assert_eq!(
            process_signal(libc::SIGSEGV).name.as_deref(),
            Some("SIGSEGV")
        );
        assert_eq!(process_signal(libc::SIGBUS).name.as_deref(), Some("SIGBUS"));
        assert_eq!(process_signal(libc::SIGTERM).number, libc::SIGTERM);
        assert_eq!(process_signal(0).name, None);
        assert_eq!(process_signal(1000).name, None);
    }

    #[tokio::test]
    async fn overflow_read_exposes_output_loss() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        let process_id = ProcessId("proc-overflow".into());
        runner
            .exec(
                &ws,
                RunnerExecRequest::for_host(
                    vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "dd if=/dev/zero bs=1024 count=300 2>/dev/null".into(),
                    ],
                    process_id.clone(),
                    Profile::WorkspaceWrite,
                ),
            )
            .await
            .unwrap();
        let status = wait_exited(&runner, &process_id).await;
        assert!(status.output_total > crate::MAX_OUTPUT_BYTES as u64);
        assert!(status.output_retained_from > 0);
        let read = runner
            .read_process(RunnerReadProcess {
                process_id: process_id.clone(),
                cursor: 0,
            })
            .await
            .unwrap();
        assert!(read.output_lost);
        assert_eq!(read.retained_from, status.output_retained_from);
        assert!(read.cursor >= read.retained_from);
    }

    /// A spawn reports its root process and when its timeout ends (P1R-C02): a pipe root by its
    /// PID and start identity, read before its reaper runs, so a command that exits at once is
    /// named too; a PTY root, which the pinned Codex PTY does not name, by its deadline only.
    #[tokio::test]
    async fn a_spawn_reports_its_root_and_its_deadline() {
        fn now_ms() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
        }
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        for (process_id, script, tty) in [
            ("proc-root-pipe", "sleep 30", false),
            ("proc-root-exits", "exit 0", false),
            ("proc-root-pty", "sleep 30", true),
        ] {
            let mut req = RunnerExecRequest::for_host(
                vec!["/bin/sh".into(), "-c".into(), script.into()],
                ProcessId(process_id.into()),
                Profile::WorkspaceWrite,
            );
            req.tty = tty;
            req.timeout_ms = 60_000;
            let before = now_ms();
            let spawned = runner.spawn_host(&ws, req).await.unwrap();
            let after = now_ms();
            let deadline = spawned.deadline_at_ms.expect("a deadline");
            assert!(
                (before + 60_000..=after + 60_000).contains(&deadline),
                "{process_id}: {before} {deadline} {after}"
            );
            if tty {
                assert_eq!(spawned.root, None);
                continue;
            }
            let root = spawned.root.expect("a pipe root");
            if cfg!(any(target_os = "linux", target_os = "macos")) {
                assert!(root.started.is_some(), "{process_id}: {root:?}");
            }
            if script == "sleep 30" {
                assert_eq!(process_start_identity(root.pid), root.started);
            }
        }
        runner.kill_host_workspace("demo").unwrap();
    }

    /// A quick process `process_id` that ended, run by `runner`: its root exited and its output
    /// ended, which is when the runner counts it as ended. The output can end well after the
    /// exit: a child that another test spawns while the pipe or terminal is open holds a copy
    /// until its exec, and on macOS the first exec of a new executable is slow.
    async fn ended_process(
        runner: &InProcessRunner,
        ws: &Workspace,
        process_id: &ProcessId,
        tty: bool,
    ) {
        let mut req = RunnerExecRequest::for_host(
            vec!["/bin/sh".into(), "-c".into(), "echo done".into()],
            process_id.clone(),
            Profile::WorkspaceWrite,
        );
        req.tty = tty;
        runner.spawn_host(ws, req).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = runner.process_status(process_id).await.unwrap();
            if status.state == ProcessState::Exited && status.eof {
                return;
            }
            assert!(Instant::now() < deadline, "{status:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn released(runner: &InProcessRunner, process_id: &ProcessId) -> bool {
        matches!(
            runner
                .inner
                .lock()
                .unwrap()
                .get(&process_id.0)
                .map(|slot| &slot.io),
            Some(SessionIo::Released)
        )
    }

    /// An independent Runner releases an ended process's handle when it would have evicted it,
    /// a tty one included, and no earlier (D11), but keeps its status and output until a
    /// controller acknowledges them; acknowledged, it goes after the retention time, and the
    /// runner remembers only that it created it (P1R-C04).
    #[tokio::test]
    async fn an_independent_runner_keeps_an_ended_process_until_it_is_acknowledged() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let ttl = Duration::from_millis(300);
        let runner = InProcessRunner::with_retention(
            Arc::new(|_| {}),
            RetentionPolicy {
                ttl,
                max_completed: 64,
            },
        );
        runner.keep_until_acknowledged(DEFAULT_MAX_UNACKNOWLEDGED);
        for (process_id, tty) in [("proc-pipe", false), ("proc-tty", true)] {
            let process_id = ProcessId(process_id.into());
            ended_process(&runner, &ws, &process_id, tty).await;
            // Within the retention time the handle is held, as always.
            assert!(!released(&runner, &process_id));
            tokio::time::sleep(ttl + Duration::from_millis(100)).await;
            // Past it the handle goes, and the status and the output stay.
            assert_eq!(runner.occupied_slots(), 0);
            assert!(released(&runner, &process_id), "{process_id:?}");
            let status = runner.process_status(&process_id).await.unwrap();
            assert_eq!(
                (status.state, status.termination, status.eof),
                (ProcessState::Exited, Some(ProcessTermination::Exited), true)
            );
            assert!(wait_chunk(&runner, &process_id).await.contains("done"));
            match runner.recall(&process_id) {
                Recalled::Held {
                    workspace_id,
                    status: recalled,
                } => assert_eq!((workspace_id.as_str(), recalled), ("demo", status)),
                other => panic!("unexpected {other:?}"),
            }
            assert_eq!(
                runner
                    .write_stdin(RunnerWriteStdin {
                        process_id: process_id.clone(),
                        data: "x".into(),
                    })
                    .await
                    .unwrap_err()
                    .as_execution()
                    .map(|body| body.code),
                Some(ErrorCode::ProcessNotFound)
            );
            assert_eq!(
                runner
                    .resize(&process_id, 30, 90)
                    .await
                    .unwrap_err()
                    .as_execution()
                    .map(|body| body.code),
                Some(ErrorCode::ProcessNotRunning)
            );
            runner.terminate(&process_id).await.unwrap();
            // Acknowledged, it stays the retention time once more, then goes.
            runner.acknowledge(std::slice::from_ref(&process_id));
            assert!(runner.process_status(&process_id).await.is_ok());
            tokio::time::sleep(ttl + Duration::from_millis(100)).await;
            assert_eq!(
                runner
                    .process_status(&process_id)
                    .await
                    .unwrap_err()
                    .as_execution()
                    .map(|body| body.code),
                Some(ErrorCode::ProcessNotFound)
            );
            assert_eq!(runner.recall(&process_id), Recalled::Dropped);
        }
    }

    /// An acknowledged process is evicted as any ended process always was: its handle is not
    /// kept past the retention time (P1R-C04).
    #[tokio::test]
    async fn an_independent_runner_evicts_an_acknowledged_process_as_before() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let ttl = Duration::from_millis(300);
        let runner = InProcessRunner::with_retention(
            Arc::new(|_| {}),
            RetentionPolicy {
                ttl,
                max_completed: 64,
            },
        );
        runner.keep_until_acknowledged(DEFAULT_MAX_UNACKNOWLEDGED);
        let process_id = ProcessId("proc-acked".into());
        ended_process(&runner, &ws, &process_id, false).await;
        runner.acknowledge(std::slice::from_ref(&process_id));
        assert!(runner.process_status(&process_id).await.is_ok());
        tokio::time::sleep(ttl + Duration::from_millis(100)).await;
        assert!(runner.process_status(&process_id).await.is_err());
        assert_eq!(runner.recall(&process_id), Recalled::Dropped);
    }

    /// Beyond its bound of released processes an independent Runner drops the oldest first,
    /// remembering that it created it (P1R-C04).
    #[tokio::test]
    async fn an_independent_runner_drops_the_oldest_released_process_beyond_its_bound() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::with_retention(
            Arc::new(|_| {}),
            RetentionPolicy {
                ttl: Duration::from_secs(60),
                max_completed: 1,
            },
        );
        runner.keep_until_acknowledged(1);
        let ids: Vec<_> = (1..=3).map(|n| ProcessId(format!("proc-{n}"))).collect();
        for process_id in &ids {
            ended_process(&runner, &ws, process_id, false).await;
        }
        // The newest holds its handle; the next one is released and kept; the oldest went.
        assert_eq!(runner.occupied_slots(), 0);
        assert!(!released(&runner, &ids[2]));
        assert!(released(&runner, &ids[1]));
        assert_eq!(runner.recall(&ids[0]), Recalled::Dropped);
        assert!(matches!(runner.recall(&ids[1]), Recalled::Held { .. }));
        assert!(matches!(runner.recall(&ids[2]), Recalled::Held { .. }));
        assert_eq!(
            runner
                .read_process(RunnerReadProcess {
                    process_id: ids[0].clone(),
                    cursor: 0,
                })
                .await
                .unwrap_err()
                .as_execution()
                .map(|body| body.code),
            Some(ErrorCode::ProcessNotFound)
        );
    }

    /// An independent Runner remembers, for each `process_id` it was asked to start, whether it
    /// created the process; a runner that keeps nothing for a controller remembers nothing of a
    /// process it does not hold (P1R-C04).
    #[tokio::test]
    async fn an_independent_runner_remembers_each_process_it_was_asked_to_start() {
        let dir = tempdir().unwrap();
        let ws = workspace(dir.path());
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        runner.keep_until_acknowledged(DEFAULT_MAX_UNACKNOWLEDGED);
        let refused = ProcessId("proc-refused".into());
        let created = ProcessId("proc-created".into());
        let never = ProcessId("proc-never".into());
        // Refused before any process is created, with or without the Linux sandbox, under
        // which a missing executable is a sandbox helper that fails.
        let no_program = RunnerExecRequest::for_host(
            vec![String::new()],
            refused.clone(),
            Profile::WorkspaceWrite,
        );
        assert_eq!(
            runner.spawn_host(&ws, no_program).await.unwrap_err().code,
            ErrorCode::InvalidCommand
        );
        runner.note_received(&refused.0);
        ended_process(&runner, &ws, &created, false).await;
        // Noting the request after the answer never hides a created process.
        runner.note_received(&created.0);
        assert_eq!(runner.recall(&refused), Recalled::Refused);
        assert!(matches!(
            runner.recall(&created),
            Recalled::Held { ref workspace_id, ref status }
                if workspace_id == "demo" && status.state == ProcessState::Exited
        ));
        assert_eq!(runner.recall(&never), Recalled::NotReceived);

        let managed = InProcessRunner::new(Arc::new(|_| {}));
        managed.note_received(&refused.0);
        assert_eq!(managed.recall(&refused), Recalled::NotReceived);
    }
}
