//! A DevGuard fixture authority in its own process, for CodeSpace's gateway tests (CSRG-U2).
//!
//! `fixture_authority <private base directory> [instance limit]` provisions, as an operator
//! would, a `codespace` control-service consumer with a static control reservation and a
//! private credential file, and serves DevGuard's fixture authority under the base directory.
//! Without native host evidence (Linux before DG-LINUX) DevGuard's fixture cannot register,
//! so it serves the authority without evidence and its bootstrap `dev-cli` consumer instead.
//!
//! It prints one JSON line with the settings (none is a secret: the secret stays in its file)
//! once the authority admits, then answers each `instances` line on standard input with a JSON
//! line listing the consumer's registered instances, and each `charged` line with one listing
//! the attempts that still hold resources and the budget they hold (CSRG-U3). It stops at the
//! end of its input. Built only as an
//! example, from this crate's development dependencies; no product links it.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use devguard_daemon::config;
use devguard_daemon::paths::AuthorityPaths;
use serde_json::json;
#[cfg(not(target_os = "macos"))]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let base = PathBuf::from(args.next().expect("a private base directory"));
    let max_instances: u32 = args
        .next()
        .map(|limit| limit.to_str().unwrap().parse().unwrap())
        .unwrap_or(2);
    let paths = AuthorityPaths::fixture(&base);
    let fixture = Fixture::start(&base, &paths, max_instances);
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", fixture.settings).unwrap();
    out.flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().trim() {
            "instances" => writeln!(out, "{}", fixture.instances()).unwrap(),
            "charged" => writeln!(out, "{}", fixture.charged()).unwrap(),
            other => panic!("unknown command {other:?}"),
        }
        out.flush().unwrap();
    }
    fixture.stop();
}

struct Fixture {
    settings: serde_json::Value,
    #[cfg(target_os = "macos")]
    authority: devguard_daemon::fixture::TestAuthority,
    #[cfg(not(target_os = "macos"))]
    stop: (Arc<AtomicBool>, std::thread::JoinHandle<()>),
}

impl Fixture {
    #[cfg(target_os = "macos")]
    fn start(base: &Path, paths: &AuthorityPaths, max_instances: u32) -> Self {
        let credential = paths.credentials().join("codespace.secret");
        let secret = fresh_secret();
        let consumer: config::ConsumerConfig = serde_json::from_value(json!({
            "generation": "codespace-g1",
            "role": "control_service",
            "credential_sha256": devguard_contract::digest_bytes(secret.as_bytes()),
            "max_instances": max_instances,
            "control_reservation": {"cpu_milli": 10, "memory_bytes": 16 << 20, "tasks": 1},
        }))
        .unwrap();
        let authority = devguard_daemon::fixture::TestAuthority::start_with(base, |host| {
            devguard_daemon::paths::write_new_private(&credential, secret.as_bytes()).unwrap();
            host.consumers.insert("codespace".into(), consumer);
        })
        .unwrap();
        // Its synthetic host readings open admission after two samples.
        authority
            .wait_until_admitting(std::time::Duration::from_secs(10))
            .unwrap();
        Self {
            settings: json!({
                "socket": authority.socket(),
                "consumer": "codespace",
                "generation": "codespace-g1",
                "credential_file": credential,
                "native": true,
            }),
            authority,
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn start(_base: &Path, paths: &AuthorityPaths, _max_instances: u32) -> Self {
        let host = config::initialize(paths).unwrap();
        let server = devguard_daemon::server::Server::open(paths).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = std::thread::spawn(move || server.run(signal).unwrap());
        Self {
            settings: json!({
                "socket": paths.socket(),
                "consumer": "dev-cli",
                "generation": host.consumers["dev-cli"].generation,
                "credential_file": paths.cli_credential(),
                "native": false,
            }),
            stop: (stop, worker),
        }
    }

    /// The consumer's registered instances: identity and process ID.
    fn instances(&self) -> serde_json::Value {
        #[cfg(target_os = "macos")]
        let instances: Vec<_> = self
            .authority
            .instances()
            .unwrap()
            .into_iter()
            .filter(|record| record.consumer_id == "codespace")
            .map(|record| {
                json!({
                    "instance_id": record.instance.instance_id,
                    "pid": record.instance.process.pid,
                })
            })
            .collect();
        #[cfg(not(target_os = "macos"))]
        let instances: Vec<serde_json::Value> = Vec::new();
        json!(instances)
    }

    /// The attempts that still hold resources, and the budget they hold.
    fn charged(&self) -> serde_json::Value {
        #[cfg(target_os = "macos")]
        {
            let attempts: Vec<_> = self
                .authority
                .attempts()
                .unwrap()
                .into_iter()
                .map(|record| json!({"attempt_id": record.key.attempt_id, "phase": record.phase}))
                .collect();
            let committed = self.authority.committed().unwrap();
            json!({"attempts": attempts, "committed": committed})
        }
        #[cfg(not(target_os = "macos"))]
        json!({"attempts": [], "committed": {"cpu_milli": 0, "memory_bytes": 0, "tasks": 0}})
    }

    fn stop(self) {
        #[cfg(target_os = "macos")]
        self.authority.stop().unwrap();
        #[cfg(not(target_os = "macos"))]
        {
            self.stop.0.store(true, Ordering::Relaxed);
            self.stop.1.join().unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
fn fresh_secret() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
