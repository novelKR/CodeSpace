//! Ending a Linux sandbox launch with every process it started (#109).
//!
//! A sandboxed pipe process's root is the sandbox helper, which becomes bubblewrap, directly or
//! through a process of its own. The sandbox process that bubblewrap creates is the first
//! process of a new PID namespace, and it sets bubblewrap's parent-death signal
//! (`--die-with-parent`) only just before it runs the command. Killing the root alone while the
//! sandbox is being set up can therefore leave that process running without its parent, and
//! with it the command it then starts, which keeps the Runner's output pipes open until it ends
//! on its own.
//!
//! [`kill_descendants`] stops the root, then each process that it finds below a stopped one,
//! from the top down, and then kills every process it found, children before their parents.
//! A stopped process can neither start a process nor reap one, so each process ID found stays
//! that process's own until the process is killed. Only a parent that ignores SIGCHLD has its
//! children reaped by the kernel, and the kernel hands out a freed process ID again only once
//! its counter has gone round every other one. Killing a sandbox's first process ends its
//! PID namespace, so every process in the sandbox ends with it, also one the walk did not find.
//! SIGSTOP and SIGKILL reach that process from the Runner's PID namespace whatever signal
//! handlers it has set. The caller then kills the root, as it kills any other.
//!
//! The walk reads `/proc`. Where `/proc` does not show the root as this process's child, as when
//! it belongs to another PID namespace, the walk does nothing and the root is killed alone.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long the walk waits for the processes it stops, in all. Past it, the walk goes on
/// without waiting, so a process that has not stopped yet may start one that it misses.
const STOP_BUDGET: Duration = Duration::from_millis(500);
/// The pause between two looks at processes that are stopping.
const STOP_POLL: Duration = Duration::from_micros(250);
/// The most levels of processes the walk goes down.
const MAX_DEPTH: usize = 64;

/// Stop every process below `root`, the caller's child that its reaper has not reaped, from the
/// top down, then kill them, children before their parents. `root` itself is left stopped for
/// the caller to kill.
pub(crate) fn kill_descendants(root: u32) {
    // `/proc` shows `root` as this process's child only when it is this PID namespace's.
    if parent_of(root) != Some(std::process::id()) || !signal(root, libc::SIGSTOP) {
        return;
    }
    let deadline = Instant::now() + STOP_BUDGET;
    let mut found = vec![root];
    let mut known = HashSet::from([root]);
    let mut stopping = vec![root];
    for _ in 0..MAX_DEPTH {
        // Once the processes found so far have stopped, none can appear below them.
        wait_stopped(stopping, deadline);
        stopping = processes()
            .into_iter()
            .filter(|(pid, parent)| known.contains(parent) && !known.contains(pid))
            .map(|(pid, _)| pid)
            .filter(|pid| signal(*pid, libc::SIGSTOP))
            .collect();
        if stopping.is_empty() {
            break;
        }
        known.extend(stopping.iter().copied());
        found.extend(stopping.iter().copied());
    }
    for pid in found[1..].iter().rev() {
        signal(*pid, libc::SIGKILL);
    }
}

/// Wait until each of `pids` has stopped or exited, or is gone, or until `deadline`.
fn wait_stopped(mut pids: Vec<u32>, deadline: Instant) {
    loop {
        pids.retain(|pid| !stopped(*pid));
        if pids.is_empty() || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(STOP_POLL);
    }
}

/// Whether every thread of process `pid` has stopped or exited, or the process is gone. A
/// process stops once each of its threads has.
fn stopped(pid: u32) -> bool {
    let Ok(threads) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return true;
    };
    threads.flatten().all(|thread| {
        std::fs::read_to_string(thread.path().join("stat"))
            .ok()
            .and_then(|stat| state_and_parent(&stat))
            .is_none_or(|(state, _)| matches!(state, 'T' | 't' | 'Z' | 'X' | 'x'))
    })
}

/// Every process that `/proc` lists, with its parent, from one read.
fn processes() -> Vec<(u32, u32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| Some((pid, parent_of(pid)?)))
        .collect()
}

/// The parent of process `pid` as its `stat` states it, or `None` once the process is gone.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    state_and_parent(&stat).map(|(_, parent)| parent)
}

/// The state and the parent's process ID in a `stat` line: its 3rd and 4th fields, after the
/// command name in parentheses, which may itself hold spaces and parentheses.
fn state_and_parent(stat: &str) -> Option<(char, u32)> {
    let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    Some((state, parent))
}

/// Send `signal` to process `pid`: never to a process group, to every process, or to this
/// process. Whether it was sent.
fn signal(pid: u32, signal: libc::c_int) -> bool {
    if pid == std::process::id() {
        return false;
    }
    match libc::pid_t::try_from(pid) {
        // SAFETY: `kill` only sends a signal, here to the one process that `pid` names.
        Ok(pid) if pid > 0 => unsafe { libc::kill(pid, signal) == 0 },
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command, Stdio};

    /// The processes other than this one whose argv is `argv`.
    pub(crate) fn running(argv: &[&str]) -> Vec<u32> {
        processes()
            .into_iter()
            .map(|(pid, _)| pid)
            .filter(|pid| *pid != std::process::id())
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
                    let args = cmdline
                        .split(|byte| *byte == 0)
                        .filter(|arg| !arg.is_empty());
                    args.eq(argv.iter().map(|arg| arg.as_bytes()))
                })
            })
            .collect()
    }

    /// Wait up to two seconds for no process with argv `argv` to run, then kill any that still
    /// does, so that a failed test leaves none behind. The ones that still ran.
    pub(crate) fn left_running(argv: &[&str]) -> Vec<u32> {
        let started = Instant::now();
        while !running(argv).is_empty() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let left = running(argv);
        for pid in &left {
            signal(*pid, libc::SIGKILL);
        }
        left
    }

    /// Whether the sandbox whose command has `marker` in its argv is being set up: its first
    /// process, in a PID namespace below `root`'s, still runs bubblewrap, so it has not started
    /// the command yet, and its parent is the bubblewrap that runs the command, not the one that
    /// the helper's preflight runs first.
    pub(crate) fn sandbox_setting_up(root: u32, marker: &str) -> bool {
        let all = processes();
        let mut below = HashSet::from([root]);
        loop {
            let before = below.len();
            for (pid, parent) in &all {
                if below.contains(parent) {
                    below.insert(*pid);
                }
            }
            if below.len() == before {
                break;
            }
        }
        let read = |pid: u32, file: &str| std::fs::read(format!("/proc/{pid}/{file}"));
        let bubblewrap = |pid: u32| read(pid, "comm").is_ok_and(|comm| comm == b"bwrap\n");
        // The `NSpid` line of `status` lists the process's ID in each PID namespace it is in.
        let namespaces = |pid: u32| {
            let status = String::from_utf8(read(pid, "status").ok()?).ok()?;
            let line = status.lines().find(|line| line.starts_with("NSpid:"))?;
            Some(line.split_whitespace().count() - 1)
        };
        let runs_marker = |pid: u32| {
            read(pid, "cmdline").is_ok_and(|cmdline| {
                cmdline
                    .split(|byte| *byte == 0)
                    .any(|arg| arg == marker.as_bytes())
            })
        };
        let root_namespaces = namespaces(root);
        all.iter().any(|(pid, parent)| {
            *pid != root
                && below.contains(pid)
                && bubblewrap(*pid)
                && namespaces(*pid) > root_namespaces
                && bubblewrap(*parent)
                && runs_marker(*parent)
        })
    }

    /// `/bin/sh -c script` with its standard output piped, once a process with argv `argv` runs.
    fn spawn_until_running(script: &str, argv: &[&str]) -> Child {
        let child = Command::new("/bin/sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while running(argv).is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{argv:?} never ran"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    }

    #[test]
    fn a_stat_line_names_its_state_and_parent() {
        assert_eq!(
            state_and_parent("42 (sleep) S 7 42 42 0 -1 4194304"),
            Some(('S', 7))
        );
        // The command name may hold spaces and parentheses.
        assert_eq!(
            state_and_parent("42 (a) b (c) T 9 42 42 0 -1 4194304"),
            Some(('T', 9))
        );
        assert_eq!(state_and_parent("42 (sleep"), None);
    }

    #[test]
    fn signal_never_reaches_a_group_or_this_process() {
        assert!(!signal(0, 0));
        assert!(!signal(u32::MAX, 0));
        assert!(!signal(std::process::id(), 0));
    }

    /// A root whose processes outlive it when it is killed alone: a shell that starts a shell
    /// that starts `sleep` in a session of its own, as bubblewrap's sandbox process runs, all of
    /// them holding the root's standard output.
    #[test]
    fn every_process_below_the_root_ends_and_the_root_stays_stopped() {
        let sleep = ["/bin/sleep", "30.1092"];
        let script = format!(
            "/bin/sh -c 'setsid {} {} & wait' & wait",
            sleep[0], sleep[1]
        );
        let mut root = spawn_until_running(&script, &sleep);
        kill_descendants(root.id());
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", root.id())).unwrap();
        assert_eq!(state_and_parent(&stat).map(|(state, _)| state), Some('T'));
        let left = left_running(&sleep);
        root.kill().unwrap();
        root.wait().unwrap();
        assert_eq!(left, Vec::<u32>::new());
        // Nothing holds the root's output any more.
        let mut output = Vec::new();
        root.stdout
            .take()
            .unwrap()
            .read_to_end(&mut output)
            .unwrap();
    }

    #[test]
    fn a_process_that_is_not_this_ones_child_is_left_alone() {
        let sleep = ["/bin/sleep", "30.1093"];
        let script = format!("{} {} & wait", sleep[0], sleep[1]);
        let mut parent = spawn_until_running(&script, &sleep);
        let grandchild = running(&sleep)[0];
        kill_descendants(grandchild);
        let untouched = !stopped(grandchild);
        signal(grandchild, libc::SIGKILL);
        parent.kill().unwrap();
        parent.wait().unwrap();
        assert!(
            untouched,
            "the walk stopped a process that is not this one's child"
        );
    }
}
