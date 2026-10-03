//! Stress harness for children that die inside `fork` before `exec` on macOS.
//!
//! On macOS, `fork` runs libSystem's atfork child handlers in the child. libnotify's handler,
//! `_notify_fork_child`, reaches libnotify's globals through a once gate. If another thread of
//! the parent was inside libnotify's one-time initialization when the parent forked, the child
//! finds the gate held by a thread it does not have and aborts before `exec`. std reads the
//! exec-error pipe closing without data as a successful exec, so the spawn succeeds and the
//! runner reports a child that ended without an exit code.
//!
//! The initialization happens once per process, so every trial runs in a fresh copy of this
//! binary. A trial reads a file through `InProcessRunner` for the first time while pipe spawns
//! of `/usr/bin/true` are in flight, then waits for every child. It fails if any child ended
//! without an exit code; nothing here kills a child.
//!
//! Like the gateway and the worker, `main` first calls `prepare_fork_spawns`, before any thread
//! exists, so the trials measure the product's mitigation.
//!
//! An ordinary `cargo test` runs nothing. Modes:
//! - `--trials N [--require-zero]`: N trials, each in a new process, and a summary. On macOS it
//!   also counts the crash reports written during the run that name `_notify_fork_child`. With
//!   `CODESPACE_FORK_RACE_OUT` set, the summary, the trial lines and those reports are written
//!   there.
//! - `--trial`: one trial in this process.
//! - `--probe-read`: one read through `InProcessRunner` and no spawn, for a debugger to watch.
//! - `--probe-slots OPERATION`: run one operation in this process and list the libSystem
//!   one-time initializations (`_os_alloc_once` slots) it started (macOS). OPERATION is `none`,
//!   `watch` (start the workspace watcher only), `fs-read` (read through the file-system
//!   adapter only), `read`, `find`, `pipe`, `pty` or `patch` (needs CODESPACE_PATCH_BIN).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use codespace_domain::{ProcessId, ProcessState, Profile, WorkspaceId};
use codespace_policy::Workspace;
use codespace_runner::{InProcessRunner, RetentionPolicy, Runner, RunnerExecRequest};

/// Tasks that keep spawning until the first read has returned.
const SPAWNERS: usize = 4;
/// Spawns per task after the read returned, so initialization the read leaves to other threads
/// still meets forks.
const SPAWNS_AFTER_READ: usize = 6;
/// Spawns per task at most, should the read never return.
const SPAWN_LIMIT: usize = 500;
const CHILD: &str = "/usr/bin/true";
const OUT_ENV: &str = "CODESPACE_FORK_RACE_OUT";

fn main() -> ExitCode {
    codespace_runner::prepare_fork_spawns();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--trial") {
        return trial_main();
    }
    if args.iter().any(|arg| arg == "--probe-read") {
        return probe_read_main();
    }
    if let Some(operation) = flag_value(&args, "--probe-slots") {
        return probe_slots_main(operation);
    }
    if let Some(count) = flag_value(&args, "--trials") {
        let Ok(count) = count.parse::<usize>() else {
            eprintln!("fork_race: --trials needs a number");
            return ExitCode::from(2);
        };
        let require_zero = args.iter().any(|arg| arg == "--require-zero");
        return trials_main(count, require_zero);
    }
    println!("fork_race: stress harness; nothing runs without --trials N (see the module docs)");
    ExitCode::SUCCESS
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

#[derive(Default)]
struct Trial {
    spawned: usize,
    exited_zero: usize,
    /// Ended without an exit code and without a kill: the pre-exec death this harness looks for.
    no_exit_code: Vec<String>,
    /// Any other ending, for example a nonzero code.
    other: Vec<String>,
    read: Option<String>,
    read_ms: u128,
}

fn trial_main() -> ExitCode {
    // The live-process limit would refuse spawns that the trial needs; this runs before any
    // thread exists.
    if std::env::var_os("CODESPACE_MAX_PROCESSES").is_none() {
        std::env::set_var("CODESPACE_MAX_PROCESSES", "100000");
    }
    let slot_at_start = notify_slot();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("fork_race: runtime: {err}");
            return ExitCode::from(2);
        }
    };
    let result = runtime.block_on(trial());
    let slot_at_end = notify_slot();
    let trial = match result {
        Ok(trial) => trial,
        Err(err) => {
            println!(
                "{}",
                json_line(&[
                    ("result", quote("error")),
                    ("error", quote(&err)),
                    ("notify_slot_at_start", quote(&slot_at_start)),
                ])
            );
            return ExitCode::from(2);
        }
    };
    let failed = !trial.no_exit_code.is_empty();
    println!(
        "{}",
        json_line(&[
            (
                "result",
                quote(if failed { "pre_exec_death" } else { "clean" })
            ),
            ("spawned", trial.spawned.to_string()),
            ("exited_zero", trial.exited_zero.to_string()),
            ("no_exit_code", list(&trial.no_exit_code)),
            ("other", list(&trial.other)),
            ("read", quote(trial.read.as_deref().unwrap_or("not run"))),
            ("read_ms", trial.read_ms.to_string()),
            ("notify_slot_at_start", quote(&slot_at_start)),
            ("notify_slot_at_end", quote(&slot_at_end)),
        ])
    );
    if failed {
        ExitCode::from(1)
    } else if !trial.other.is_empty() || trial.read.as_deref() != Some("ok") {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn workspace(root: &Path) -> Workspace {
    Workspace::new(
        WorkspaceId("fork-race".into()),
        root.to_path_buf(),
        Profile::WorkspaceWrite,
    )
}

fn runner() -> InProcessRunner {
    InProcessRunner::with_retention(
        Arc::new(|_| {}),
        RetentionPolicy {
            ttl: Duration::from_secs(600),
            max_completed: 100_000,
        },
    )
}

async fn trial() -> Result<Trial, String> {
    let dir = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
    std::fs::write(dir.path().join("probe.txt"), b"probe").map_err(|err| err.to_string())?;
    let ws = workspace(dir.path());
    let runner = runner();
    let read_done = Arc::new(AtomicBool::new(false));
    let forked = Arc::new(AtomicUsize::new(0));
    let mut spawners = Vec::new();
    for task in 0..SPAWNERS {
        let runner = runner.clone();
        let ws = ws.clone();
        let read_done = read_done.clone();
        let forked = forked.clone();
        spawners.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            let mut after_read = 0;
            for n in 0..SPAWN_LIMIT {
                if read_done.load(Ordering::SeqCst) {
                    after_read += 1;
                    if after_read > SPAWNS_AFTER_READ {
                        break;
                    }
                }
                let id = ProcessId(format!("race-{task}-{n}"));
                let request = RunnerExecRequest::for_host(
                    vec![CHILD.into()],
                    id.clone(),
                    Profile::WorkspaceWrite,
                );
                runner
                    .exec(&ws, request)
                    .await
                    .map_err(|err| format!("exec {}: {err:?}", id.0))?;
                ids.push(id);
                if n == 0 {
                    forked.fetch_add(1, Ordering::SeqCst);
                }
                // A spawn completes without suspending; let the runtime run its other tasks.
                tokio::task::yield_now().await;
            }
            Ok::<_, String>(ids)
        }));
    }
    // Start the read once every spawner has forked. This thread drives `block_on`, not a
    // runtime worker, so it waits without a timer, which busy spawners could starve.
    let deadline = Instant::now() + Duration::from_secs(30);
    while forked.load(Ordering::SeqCst) < SPAWNERS
        && !spawners.iter().any(|spawner| spawner.is_finished())
        && Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    let read_started = Instant::now();
    let read = runner.read(&ws, "probe.txt", None, None).await;
    read_done.store(true, Ordering::SeqCst);
    let mut trial = Trial {
        read_ms: read_started.elapsed().as_millis(),
        read: Some(match read {
            Ok(result) if result.content == "probe" => "ok".into(),
            Ok(result) => format!("unexpected content {:?}", result.content),
            Err(err) => format!("{err:?}"),
        }),
        ..Trial::default()
    };
    let mut ids = Vec::new();
    for spawner in spawners {
        ids.extend(spawner.await.map_err(|err| format!("spawner: {err}"))??);
    }
    trial.spawned = ids.len();
    let deadline = Instant::now() + Duration::from_secs(30);
    for id in ids {
        loop {
            let status = runner
                .process_status(&id)
                .await
                .map_err(|err| format!("status {}: {err:?}", id.0))?;
            if status.state == ProcessState::Exited {
                match status.exit_code {
                    Some(0) => trial.exited_zero += 1,
                    None => trial
                        .no_exit_code
                        .push(format!("{}: {:?}", id.0, status.termination)),
                    Some(code) => trial.other.push(format!("{}: exit {code}", id.0)),
                }
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("{} did not exit", id.0));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    Ok(trial)
}

fn probe_read_main() -> ExitCode {
    println!(
        "probe-read: notify slot before the runtime: {}",
        notify_slot()
    );
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("probe-read: runtime: {err}");
            return ExitCode::from(2);
        }
    };
    let outcome = runtime.block_on(async {
        let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        std::fs::write(dir.path().join("probe.txt"), b"probe").map_err(|err| err.to_string())?;
        let ws = workspace(dir.path());
        let runner = runner();
        println!("probe-read: notify slot before the read: {}", notify_slot());
        let read = runner.read(&ws, "probe.txt", None, None).await;
        println!("probe-read: notify slot after the read: {}", notify_slot());
        // The watcher's own thread may finish starting after the read returns.
        tokio::time::sleep(Duration::from_millis(500)).await;
        println!("probe-read: notify slot 500 ms later: {}", notify_slot());
        read.map(|_| ()).map_err(|err| format!("{err:?}"))
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("probe-read: {err}");
            ExitCode::from(2)
        }
    }
}

/// Keys of libplatform's `_os_alloc_once_table` in Libsystem's `alloc_once_private.h`.
const SLOT_NAMES: [&str; 23] = [
    "LIBSYSTEM_NOTIFY",
    "LIBXPC",
    "LIBSYSTEM_C",
    "LIBSYSTEM_INFO",
    "LIBSYSTEM_NETWORK",
    "LIBCACHE",
    "LIBCOMMONCRYPTO",
    "LIBDISPATCH",
    "LIBDYLD",
    "LIBKEYMGR",
    "LIBLAUNCH",
    "LIBMACHO",
    "OS_TRACE",
    "LIBSYSTEM_BLOCKS",
    "LIBSYSTEM_MALLOC",
    "LIBSYSTEM_PLATFORM",
    "LIBSYSTEM_PTHREAD",
    "LIBSYSTEM_STATS",
    "LIBSECINIT",
    "LIBSYSTEM_CORESERVICES",
    "LIBSYSTEM_SYMPTOMS",
    "LIBSYSTEM_PLATFORM_ASL",
    "LIBSYSTEM_FEATUREFLAGS",
];

/// Slots of `_os_alloc_once_table` (`OS_ALLOC_ONCE_KEY_MAX` = 100) whose initialization has
/// started, for reporting only.
#[cfg(target_os = "macos")]
fn started_slots() -> BTreeSet<usize> {
    // SAFETY: as in `notify_slot`; the table has 100 (once, pointer) pairs.
    let table = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_os_alloc_once_table".as_ptr()) };
    if table.is_null() {
        return BTreeSet::new();
    }
    let words = table as *const usize;
    (0..100)
        // SAFETY: slot `n` starts at word `2 * n` of the table.
        .filter(|n| unsafe { std::ptr::read_volatile(words.add(2 * n)) } != 0)
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn started_slots() -> BTreeSet<usize> {
    BTreeSet::new()
}

fn slot_names(slots: &BTreeSet<usize>) -> String {
    let names: Vec<String> = slots
        .iter()
        .map(|&n| format!("{n} {}", SLOT_NAMES.get(n).copied().unwrap_or("?")))
        .collect();
    format!("[{}]", names.join(", "))
}

fn probe_slots_main(operation: &str) -> ExitCode {
    let at_start = started_slots();
    println!(
        "probe-slots {operation}: started before the runtime: {}",
        slot_names(&at_start)
    );
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("probe-slots: runtime: {err}");
            return ExitCode::from(2);
        }
    };
    let outcome: Result<(), String> = runtime.block_on(async {
        let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        std::fs::write(dir.path().join("probe.txt"), b"probe").map_err(|err| err.to_string())?;
        let ws = workspace(dir.path());
        let runner = runner();
        let before = started_slots();
        println!(
            "probe-slots {operation}: started before the operation: {}",
            slot_names(&before.difference(&at_start).copied().collect())
        );
        let spawn = |tty: bool| {
            let runner = runner.clone();
            let ws = ws.clone();
            async move {
                let id = ProcessId(format!("probe-{tty}"));
                let mut request = RunnerExecRequest::for_host(
                    vec![CHILD.into()],
                    id.clone(),
                    Profile::WorkspaceWrite,
                );
                request.tty = tty;
                runner
                    .exec(&ws, request)
                    .await
                    .map_err(|err| format!("{err:?}"))?;
                for _ in 0..500 {
                    let status = runner
                        .process_status(&id)
                        .await
                        .map_err(|err| format!("{err:?}"))?;
                    if status.state == ProcessState::Exited {
                        return Ok(format!("exit {:?}", status.exit_code));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err("the child did not exit".to_string())
            }
        };
        let result = match operation {
            "none" => Ok("nothing".to_string()),
            "watch" => runner
                .subscribe_watch(&ws)
                .map(|_| "watching".to_string())
                .map_err(|err| format!("{err:?}")),
            "fs-read" => codespace_runner::PathSandbox::new(ws.clone())
                .read_file("probe.txt")
                .await
                .map(|read| read.content)
                .map_err(|err| format!("{err:?}")),
            "read" => runner
                .read(&ws, "probe.txt", None, None)
                .await
                .map(|read| read.content)
                .map_err(|err| format!("{err:?}")),
            "find" => runner
                .find(&ws, None, None, None)
                .await
                .map(|found| format!("{:?}", found.paths))
                .map_err(|err| format!("{err:?}")),
            "pipe" => spawn(false).await,
            "pty" => spawn(true).await,
            "patch" => runner
                .apply_patch(
                    &ws,
                    codespace_runner::RunnerApplyPatchRequest {
                        patch: "*** Begin Patch\n*** Add File: added.txt\n+added\n*** End Patch\n"
                            .into(),
                        expected_versions: Default::default(),
                        check_only: false,
                    },
                )
                .await
                .map(|applied| format!("{:?}", applied.status))
                .map_err(|err| format!("{err:?}")),
            other => Err(format!("unknown operation {other}")),
        };
        // Threads the operation started may finish their own setup a little later.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let after = started_slots();
        println!(
            "probe-slots {operation}: {result:?}; started by the operation: {}",
            slot_names(&after.difference(&before).copied().collect())
        );
        result.map(|_| ())
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("probe-slots: {err}");
            ExitCode::from(2)
        }
    }
}

fn trials_main(count: usize, require_zero: bool) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("fork_race: current_exe: {err}");
            return ExitCode::from(2);
        }
    };
    let out = std::env::var_os(OUT_ENV).map(PathBuf::from);
    let started = SystemTime::now();
    let known = crash_reports(None);
    let mut failed = 0usize;
    let mut errors = 0usize;
    let mut no_exit_code = 0usize;
    let mut spawned = 0usize;
    let mut lines = Vec::new();
    for n in 0..count {
        let output = Command::new(&exe)
            .arg("--trial")
            .env("CODESPACE_MAX_PROCESSES", "100000")
            .output();
        let output = match output {
            Ok(output) => output,
            Err(err) => {
                eprintln!("fork_race: trial {n}: {err}");
                errors += 1;
                continue;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        spawned += field_number(&stdout, "spawned");
        no_exit_code += count_list(&stdout, "no_exit_code");
        match output.status.code() {
            Some(0) => {}
            Some(1) => failed += 1,
            _ => {
                errors += 1;
                eprintln!(
                    "fork_race: trial {n} ended {:?}: {stdout} {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        lines.push(format!("{{\"trial\":{n},\"line\":{stdout}}}"));
    }
    let reports = settle_crash_reports(&known, started);
    let notify_reports: Vec<&PathBuf> = reports
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("_notify_fork_child"))
        })
        .collect();
    let summary = json_line(&[
        ("trials", count.to_string()),
        ("failed_trials", failed.to_string()),
        ("error_trials", errors.to_string()),
        ("children_spawned", spawned.to_string()),
        ("children_without_exit_code", no_exit_code.to_string()),
        ("new_crash_reports", reports.len().to_string()),
        (
            "notify_fork_child_crash_reports",
            notify_reports.len().to_string(),
        ),
        ("require_zero", require_zero.to_string()),
        ("platform", quote(std::env::consts::OS)),
    ]);
    println!("fork_race summary: {summary}");
    if let Some(out) = out {
        if let Err(err) = write_evidence(&out, &summary, &lines, &reports) {
            eprintln!("fork_race: writing {}: {err}", out.display());
            errors += 1;
        }
    }
    if errors > 0 || (require_zero && (failed > 0 || !notify_reports.is_empty())) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn write_evidence(
    out: &Path,
    summary: &str,
    lines: &[String],
    reports: &[PathBuf],
) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("summary.json"), format!("{summary}\n"))?;
    std::fs::write(out.join("trials.jsonl"), lines.join("\n") + "\n")?;
    if !reports.is_empty() {
        let dir = out.join("crash-reports");
        std::fs::create_dir_all(&dir)?;
        for report in reports {
            if let Some(name) = report.file_name() {
                std::fs::copy(report, dir.join(name))?;
            }
        }
    }
    Ok(())
}

/// Crash reports present now, or only those modified since `since`.
fn crash_reports(since: Option<SystemTime>) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    if !cfg!(target_os = "macos") {
        return found;
    }
    let mut dirs = vec![PathBuf::from("/Library/Logs/DiagnosticReports")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Library/Logs/DiagnosticReports"));
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let report = path
                .extension()
                .is_some_and(|ext| ext == "ips" || ext == "crash");
            let recent = since.is_none_or(|since| {
                entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| modified >= since)
            });
            if report && recent {
                found.insert(path);
            }
        }
    }
    found
}

/// Reports written during the run. ReportCrash writes them asynchronously, so wait until the
/// set stops changing.
fn settle_crash_reports(known: &BTreeSet<PathBuf>, since: SystemTime) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let fresh = || -> BTreeSet<PathBuf> {
        crash_reports(Some(since))
            .into_iter()
            .filter(|path| !known.contains(path))
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = fresh();
    let mut stable_since = Instant::now();
    while Instant::now() < deadline && stable_since.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_secs(1));
        let now = fresh();
        if now != last {
            last = now;
            stable_since = Instant::now();
        }
    }
    last.into_iter().collect()
}

/// State of libnotify's once slot (`_os_alloc_once_table[OS_ALLOC_ONCE_KEY_LIBSYSTEM_NOTIFY]`,
/// key 0 in Libsystem's `alloc_once_private.h`), for reporting only.
#[cfg(target_os = "macos")]
fn notify_slot() -> String {
    // SAFETY: dlsym looks up a data symbol that libsystem_platform exports for its inline
    // `os_alloc_once`. The table is a static array of (once, pointer) word pairs.
    let table = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_os_alloc_once_table".as_ptr()) };
    if table.is_null() {
        return "unavailable".into();
    }
    let slot = table as *const usize;
    // SAFETY: slot 0 is the first two words of the table. Another thread may be writing it; a
    // torn report is acceptable here.
    let (once, ptr) = unsafe {
        (
            std::ptr::read_volatile(slot),
            std::ptr::read_volatile(slot.add(1)),
        )
    };
    let state = match once {
        0 => "uninitialized",
        usize::MAX => "initialized",
        // libplatform's quiescing generation: the initializer has returned.
        value if value & 3 == 1 => "initialized (quiescing)",
        _ => "in progress",
    };
    format!("{state} once={once:#x} ptr={ptr:#x}")
}

#[cfg(not(target_os = "macos"))]
fn notify_slot() -> String {
    "not macOS".into()
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
    format!("[{}]", quoted.join(","))
}

fn json_line(fields: &[(&str, String)]) -> String {
    let body: Vec<String> = fields
        .iter()
        .map(|(key, value)| format!("{}:{value}", quote(key)))
        .collect();
    format!("{{{}}}", body.join(","))
}

/// A number field of a trial line written by `json_line`.
fn field_number(line: &str, key: &str) -> usize {
    let pattern = format!("\"{key}\":");
    line.find(&pattern)
        .map(|at| &line[at + pattern.len()..])
        .and_then(|rest| {
            rest.split(|ch: char| !ch.is_ascii_digit())
                .next()
                .and_then(|digits| digits.parse().ok())
        })
        .unwrap_or(0)
}

/// Entries of a list field of a trial line written by `json_line`.
fn count_list(line: &str, key: &str) -> usize {
    let pattern = format!("\"{key}\":[");
    let Some(at) = line.find(&pattern) else {
        return 0;
    };
    let rest = &line[at + pattern.len()..];
    let Some(end) = rest.find(']') else {
        return 0;
    };
    let body = &rest[..end];
    if body.is_empty() {
        0
    } else {
        body.matches("\",\"").count() + 1
    }
}
