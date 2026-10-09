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

use crate::{
    runner_local_exec_env, RunnerCwd, RunnerExecRequest, RunnerExecResult, RunnerProcessStatus,
    RunnerReadProcess, RunnerReadResult, RunnerResizeResult, RunnerWriteStdin,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_PROCESSES: usize = 8;
pub const DEFAULT_COMPLETED_TTL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_MAX_COMPLETED: usize = 64;

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
    /// The live process limit, when set here instead of by `CODESPACE_MAX_PROCESSES`.
    max_processes: Option<usize>,
    on_release: ShellRelease,
    retention: RetentionPolicy,
    pub(crate) watches: crate::watch::WatchSet,
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

struct SpawnCtx {
    cwd: PathBuf,
    cap: usize,
    timeout: Duration,
    output: Arc<Mutex<OutputBuf>>,
    completed_at: Arc<Mutex<Option<Instant>>>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    process_id: String,
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
    kill_intent: Option<KillIntent>,
    snapshot: LifecycleSnapshot,
}

impl Lifecycle {
    fn new() -> Self {
        Self {
            finished: false,
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

    fn evict_completed(&self, map: &mut HashMap<String, Slot>) {
        let now = Instant::now();
        let ttl = self.retention.ttl;
        map.retain(|_, slot| {
            let completed = slot.completed_at.lock().ok().and_then(|guard| *guard);
            !matches!(completed, Some(at) if now.duration_since(at) > ttl)
        });
        let mut completed: Vec<(String, Instant)> = map
            .iter()
            .filter_map(|(id, slot)| {
                slot.completed_at
                    .lock()
                    .ok()
                    .and_then(|guard| guard.map(|at| (id.clone(), at)))
            })
            .collect();
        if completed.len() <= self.retention.max_completed {
            return;
        }
        completed.sort_by_key(|(_, at)| *at);
        let drop_n = completed.len() - self.retention.max_completed;
        for (id, _) in completed.into_iter().take(drop_n) {
            map.remove(&id);
        }
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
        self.spawn_slotted(ws, req, slot, How::Direct).await
    }

    /// Start a governed execution's launch helper through this runner's own pipe or PTY spawner
    /// (CSRG-U4), filling the slot its preparation took. The helper alone receives the
    /// invocation's two descriptors, and this process's copies are closed as soon as the spawn
    /// returns. The helper becomes the execution: from here it is a process like any other.
    /// Returns the spawn's result and the owner's end of the helper's transcript.
    #[cfg(feature = "devguard")]
    pub(crate) async fn spawn_managed(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        slot: SlotReservation,
        invocation: codespace_devguard::launch::HelperInvocation,
    ) -> (
        Result<RunnerExecResult, ErrorBody>,
        codespace_devguard::launch::Transcript,
    ) {
        let managed = Arc::new(Managed::new(invocation));
        let result = self
            .spawn_slotted(ws, req, slot, How::Managed(managed.clone()))
            .await;
        (result, managed.finish())
    }

    async fn spawn_slotted(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
        slot: SlotReservation,
        how: How,
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
        } = ctx;
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
        };
        {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            reservation.fill(&mut map, slot);
        }

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
            reap_child(wait_child, wait_life).await;
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
            if life.finished {
                return;
            }
            match ch.try_wait() {
                Ok(Some(status)) => life.finish_wait(status.code(), status.signal()),
                Ok(None) => {
                    life.note_kill(KillIntent::Timeout);
                    let _ = ch.start_kill();
                    drop(life);
                    if let Ok(mut buf) = timeout_out.lock() {
                        buf.timed_out = true;
                    }
                }
                Err(_) => {
                    life.note_kill(KillIntent::Timeout);
                    life.finish_lost();
                    drop(life);
                    if let Ok(mut buf) = timeout_out.lock() {
                        buf.timed_out = true;
                    }
                }
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
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
        } = ctx;
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
        let spawned = match &how {
            How::Direct => codespace_pty::spawn(&program, &args, &cwd, &env).await,
            #[cfg(feature = "devguard")]
            How::Managed(managed) => {
                // The pinned Codex PTY keeps only descriptors that are already inheritable, so
                // the two are inheritable here from now until `how.spawned()` closes them right
                // after the spawn. Every other spawner in this process excludes unrelated
                // descriptors from its children meanwhile (#79).
                match managed.make_inheritable() {
                    Ok(()) => {
                        codespace_pty::spawn_inheriting(
                            &program,
                            &args,
                            &cwd,
                            &env,
                            &managed.descriptors(),
                        )
                        .await
                    }
                    Err(err) => Err(format!("cannot pass the launch descriptors: {err}")),
                }
            }
        };
        how.spawned();
        let mut session = spawned.map_err(|err| {
            abort();
            ErrorBody::new(
                ErrorCode::ProcessSpawnFailed,
                format!("failed to spawn process: {err}"),
            )
        })?;
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
        };
        {
            let mut map = self.inner.lock().expect("runner");
            self.evict_completed(&mut map);
            reservation.fill(&mut map, slot);
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
                // The pinned Codex PTY reports a signal death as exit code 1.
                Ok(code) => wait_life
                    .lock()
                    .expect("lifecycle")
                    .finish_wait(Some(code), None),
                Err(_) => wait_life.lock().expect("lifecycle").finish_lost(),
            }
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
            if life.finished || timeout_session.has_exited() {
                return;
            }
            life.note_kill(KillIntent::Timeout);
            drop(life);
            timeout_session.kill();
            if let Ok(mut buf) = timeout_out.lock() {
                buf.timed_out = true;
            }
        });

        Ok(RunnerExecResult {
            process_id: req.process_id,
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
        let snap = slot.lifecycle.lock().expect("lifecycle").snapshot;
        let buf = slot.output.lock().expect("output");
        Ok(RunnerProcessStatus {
            process_id: process_id.clone(),
            state: snap.state,
            exit_code: snap.exit_code,
            termination: snap.termination,
            signal: snap.signal.map(process_signal),
            output_total: buf.total,
            output_retained_from: buf.dropped,
            eof: buf.eof,
        })
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
}

async fn reap_child(child: Arc<Mutex<Child>>, lifecycle: Arc<Mutex<Lifecycle>>) {
    loop {
        {
            let mut ch = child.lock().expect("child");
            let mut life = lifecycle.lock().expect("lifecycle");
            if life.finished {
                return;
            }
            match ch.try_wait() {
                Ok(Some(status)) => {
                    life.finish_wait(status.code(), status.signal());
                    return;
                }
                Ok(None) => {}
                Err(_) => {
                    life.finish_lost();
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
            if life.finished {
                return Ok(false);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    life.finish_wait(status.code(), status.signal());
                    Ok(false)
                }
                Ok(None) => {
                    life.note_kill(KillIntent::Terminated);
                    child
                        .start_kill()
                        .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
                    Ok(true)
                }
                Err(_) => {
                    life.finish_lost();
                    Ok(false)
                }
            }
        }
        SessionIo::Pty { session, .. } => {
            let mut life = slot.lifecycle.lock().expect("lifecycle");
            if life.finished || session.has_exited() {
                return Ok(false);
            }
            life.note_kill(KillIntent::Terminated);
            session.kill();
            Ok(true)
        }
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

fn live_count(map: &HashMap<String, Slot>) -> usize {
    map.values()
        .filter(|slot| slot.output.lock().map(|buf| !buf.eof).unwrap_or(false))
        .count()
}

/// A signal number of this host, named when it is one of the standard signals.
fn process_signal(number: i32) -> ProcessSignal {
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
            SessionIo::Pty { .. } => panic!("{} is not a pipe child", process_id.0),
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
}
