//! How a worker's processes relate to its connections (P1R-C01), and which worker process a
//! gateway is talking to.
//!
//! A `managed` worker belongs to the gateway that starts it: it serves that one connection and
//! ends its processes as it exits once the connection closes. An `independent` Runner is
//! started by an operator or a harness and only connected to: it serves one connection at a
//! time, and its processes, their output and their timeouts outlive each connection.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How a worker's processes relate to its connections (P1R-C01).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerLifecycle {
    /// The gateway that starts the worker owns it: one connection, after which the worker ends
    /// its processes and exits.
    #[default]
    Managed,
    /// Started by an operator or a harness, not by a gateway. It serves one connection at a
    /// time, and its processes, their output and their timeouts outlive each connection until a
    /// `Stop`, or its own SIGTERM or SIGINT, ends them and the Runner.
    Independent,
}

impl RunnerLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Independent => "independent",
        }
    }
}

/// Which Runner process this is (P1R-C01): stated in every `Hello`, the same for each
/// connection of one process and different for any other. Nothing in it is a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerIdentity {
    /// The host's boot session, when the host reports one: Linux's `boot_id`, macOS's
    /// `kern.bootsessionuuid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    pub pid: u32,
    /// When the Runner started, in milliseconds since the Unix epoch.
    pub started_at_ms: u64,
    /// 128 random bits minted when the Runner started, in hex.
    pub nonce: String,
}

impl RunnerIdentity {
    /// This process's identity, fixed by the first call: a worker makes it first thing.
    pub fn this_process() -> RunnerIdentity {
        static IDENTITY: OnceLock<RunnerIdentity> = OnceLock::new();
        IDENTITY
            .get_or_init(|| RunnerIdentity {
                boot_id: boot_id(),
                pid: std::process::id(),
                started_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis() as u64)
                    .unwrap_or(0),
                nonce: nonce(),
            })
            .clone()
    }
}

#[cfg(target_os = "linux")]
fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

#[cfg(target_os = "macos")]
fn boot_id() -> Option<String> {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `buf` has room for `len` bytes, which sysctlbyname fills and updates; the name is
    // a NUL-terminated C string.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let id = String::from_utf8_lossy(&buf[..len.min(buf.len())]);
    let id = id.trim_end_matches('\0').trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot_id() -> Option<String> {
    None
}

/// 128 random bits in hex. A host without `/dev/urandom` gets the time and the process ID
/// instead: the nonce tells Runner processes apart, it guards nothing.
fn nonce() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    match std::fs::File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut bytes)) {
        Ok(()) => hex::encode(bytes),
        Err(_) => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            format!("{nanos:x}{:x}", std::process::id())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_is_this_process_and_stays_the_same() {
        let identity = RunnerIdentity::this_process();
        assert_eq!(identity.pid, std::process::id());
        assert_eq!(identity.nonce.len(), 32, "{identity:?}");
        assert!(identity.started_at_ms > 0);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(identity.boot_id.is_some(), "{identity:?}");
        }
        assert_eq!(RunnerIdentity::this_process(), identity);
    }

    #[test]
    fn lifecycles_cross_the_wire_by_name() {
        assert_eq!(
            serde_json::to_string(&RunnerLifecycle::Independent).unwrap(),
            "\"independent\""
        );
        assert_eq!(
            serde_json::from_str::<RunnerLifecycle>("\"managed\"").unwrap(),
            RunnerLifecycle::Managed
        );
        assert_eq!(RunnerLifecycle::default(), RunnerLifecycle::Managed);
    }
}
