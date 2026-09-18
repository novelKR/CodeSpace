//! Isolated Linux command-sandbox adapter. Wraps `codex-linux-sandbox`
//! and exposes only CodeSpace-owned types. Codex `PermissionProfile`
//! stays inside this crate. Not an MCP tool.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath, NetworkSandboxPolicy,
};
use codex_sandboxing::landlock::create_linux_sandbox_command_args_for_permission_profile;
use codex_utils_path_uri::PathUri;

/// Env override for the helper, matching `CODESPACE_PATCH_BIN` /
/// `CODESPACE_RUNTIME_BIN`.
pub const HELPER_BIN_ENV: &str = "CODESPACE_LINUX_SANDBOX_BIN";
pub const HELPER_BIN_NAME: &str = "codespace-linux-sandbox";

/// PATH inside the sandbox. Host `HOME` / `~/.cargo/bin` are not mounted
/// for toolchain discovery.
pub const SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin";

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROTECTED_METADATA_NAMES: &[&str] = &[".git", ".agents", ".codex"];

/// Filesystem / network inputs the runner already decided. Codex named
/// `workspace-write` is not reused: that profile remounts `.git` read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxExecSpec {
    pub workspace_root: PathBuf,
    pub writable_workspace: bool,
    pub network: SandboxNetwork,
}

/// This work package only hard-denies network. `Enabled` / proxy is later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxNetwork {
    Restricted,
}

/// Helper program + argv. Pipe and PTY spawn this, not the user argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxLaunch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Locate the helper: `CODESPACE_LINUX_SANDBOX_BIN`, else a binary named
/// [`HELPER_BIN_NAME`] next to the current executable.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(HELPER_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(HELPER_BIN_NAME);
    candidate.is_file().then_some(candidate)
}

/// Cached Linux helper + bwrap/userns/pid/seccomp probe. Non-Linux is
/// always `false`. A failed probe does not wrap later spawns. A successful
/// probe never falls back to unsandboxed user argv.
pub fn probe() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return false;
        }
        let Some(helper) = helper_path() else {
            return false;
        };
        probe_helper(&helper)
    })
}

/// Run `/usr/bin/true` (or `/bin/true`) once through the helper. Used by
/// [`probe`] and by Linux isolation tests that pass an explicit helper.
pub fn probe_helper(helper: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if !helper.is_file() {
        return false;
    }
    let workspace = probe_workspace();
    let spec = SandboxExecSpec {
        workspace_root: workspace.clone(),
        writable_workspace: true,
        network: SandboxNetwork::Restricted,
    };
    let Ok(launch) = prepare_from_helper(helper, &spec, &[true_command()], &workspace) else {
        return false;
    };
    let mut child = Command::new(&launch.program);
    child
        .args(&launch.args)
        .current_dir(&workspace)
        .env_clear()
        .envs(sandbox_exec_env(&workspace))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(child) = child.spawn() else {
        return false;
    };
    matches!(wait_with_timeout(child, PROBE_TIMEOUT), Some(status) if status.success())
}

/// Build helper argv for `command` at `command_cwd`. Uses [`helper_path`].
/// Does not spawn. Missing helper is an error (caller must not unsandbox).
pub fn prepare(
    spec: &SandboxExecSpec,
    command: &[String],
    command_cwd: &Path,
) -> Result<SandboxLaunch, String> {
    let helper = helper_path().ok_or_else(|| {
        format!(
            "{HELPER_BIN_ENV} is unset and {HELPER_BIN_NAME} was not found next to the executable"
        )
    })?;
    prepare_from_helper(&helper, spec, command, command_cwd)
}

/// Same as [`prepare`] with an explicit helper path (tests and probe).
pub fn prepare_from_helper(
    helper: &Path,
    spec: &SandboxExecSpec,
    command: &[String],
    command_cwd: &Path,
) -> Result<SandboxLaunch, String> {
    if command.is_empty() || command[0].is_empty() {
        return Err("command must be a non-empty argv (no shell)".into());
    }
    let profile = permission_profile(spec)?;
    let cwd = absolute_dir(command_cwd)?;
    let policy_cwd = absolute_dir(&spec.workspace_root)?;
    let args = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        create_linux_sandbox_command_args_for_permission_profile(
            command.to_vec(),
            &cwd,
            &profile,
            &policy_cwd,
            /*use_legacy_landlock*/ false,
            /*allow_network_for_proxy*/ false,
        )
    }))
    .map_err(|_| "failed to build linux sandbox argv".to_string())?;
    if args.iter().any(|arg| {
        arg == "--allow-network-for-proxy"
            || arg == "--proxy-route-spec"
            || arg == "--not-a-security-boundary"
            || arg == "--use-legacy-landlock"
    }) {
        return Err("linux sandbox argv included a forbidden helper flag".into());
    }
    Ok(SandboxLaunch {
        program: helper.to_path_buf(),
        args: args.into_iter().map(OsString::from).collect(),
    })
}

/// Env applied after `env_clear` for a sandboxed spawn. `HOME` stays the
/// workspace root (same as unsandboxed runner defaults).
pub fn sandbox_exec_env(home: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), SANDBOX_PATH.into());
    env.insert("HOME".into(), home.display().to_string());
    env.insert("LANG".into(), "C".into());
    env.insert("TMPDIR".into(), "/tmp".into());
    env
}

fn permission_profile(spec: &SandboxExecSpec) -> Result<PermissionProfile, String> {
    let mut entries = vec![FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Minimal,
        },
        FileSystemAccessMode::Read,
    )];
    let workspace = path_uri(&spec.workspace_root)?;
    let access = if spec.writable_workspace {
        FileSystemAccessMode::Write
    } else {
        FileSystemAccessMode::Read
    };
    entries.push(FileSystemSandboxEntry::new(
        workspace.clone().into(),
        access,
    ));
    if spec.writable_workspace {
        // Codex auto-protects `.git` / `.agents` / `.codex` on writable
        // roots. CodeSpace `WorkspaceWrite` is `**` Write, so override.
        for name in PROTECTED_METADATA_NAMES {
            if let Ok(path) = workspace.join(name) {
                entries.push(FileSystemSandboxEntry::new(
                    path.into(),
                    FileSystemAccessMode::Write,
                ));
            }
        }
    }
    // Private `/tmp` comes from bubblewrap `--tmpfs /` under Minimal.
    // Do not bind host `/tmp` (`SlashTmp`) or host `TMPDIR`.
    let file_system = FileSystemSandboxPolicy::restricted(entries);
    let network = match spec.network {
        SandboxNetwork::Restricted => NetworkSandboxPolicy::Restricted,
    };
    Ok(PermissionProfile::from_runtime_permissions(
        &file_system,
        network,
    ))
}

fn path_uri(path: &Path) -> Result<PathUri, String> {
    let path = absolute_dir(path)?;
    PathUri::from_host_native_path(&path).map_err(|err| err.to_string())
}

fn absolute_dir(path: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !path.is_absolute() {
        return Err(format!("sandbox path must be absolute: {}", path.display()));
    }
    if path.to_str().is_none() {
        return Err(format!(
            "sandbox path must be valid UTF-8: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn true_command() -> String {
    for candidate in ["/usr/bin/true", "/bin/true"] {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "true".to_string()
}

fn probe_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join("codespace-linux-sandbox-probe");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn dummy_helper() -> PathBuf {
        PathBuf::from("/usr/bin/codespace-linux-sandbox")
    }

    fn spec(root: &Path, writable: bool) -> SandboxExecSpec {
        SandboxExecSpec {
            workspace_root: root.to_path_buf(),
            writable_workspace: writable,
            network: SandboxNetwork::Restricted,
        }
    }

    fn launch_args(root: &Path, writable: bool) -> Vec<String> {
        let launch = prepare_from_helper(
            &dummy_helper(),
            &spec(root, writable),
            &["/bin/echo".into(), "hi".into()],
            root,
        )
        .expect("prepare");
        assert_eq!(launch.program, dummy_helper());
        launch
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn profile_json(args: &[String]) -> Value {
        let idx = args
            .iter()
            .position(|arg| arg == "--permission-profile")
            .expect("permission-profile flag");
        serde_json::from_str(&args[idx + 1]).expect("profile json")
    }

    fn special_kinds(profile: &Value) -> Vec<String> {
        profile["file_system"]["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|entry| {
                let path = &entry["path"];
                (path["type"] == "special").then(|| {
                    path["value"]["kind"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
            })
            .collect()
    }

    #[test]
    fn crate_is_isolated_adapter() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codespace-linux-sandbox");
    }

    #[test]
    fn probe_is_false_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(!probe());
            assert!(!probe_helper(Path::new("/usr/bin/true")));
        }
    }

    #[test]
    fn prepare_uses_minimal_workspace_and_restricted_network() {
        let dir = tempfile::tempdir().unwrap();
        let args = launch_args(dir.path(), true);
        assert!(!args.iter().any(|arg| arg == "--allow-network-for-proxy"
            || arg == "--proxy-route-spec"
            || arg == "--not-a-security-boundary"
            || arg == "--use-legacy-landlock"));
        assert!(args.contains(&"--sandbox-policy-cwd".to_string()));
        assert!(args.contains(&"--command-cwd".to_string()));
        assert_eq!(args[args.len() - 3], "--");
        assert_eq!(args[args.len() - 2], "/bin/echo");
        assert_eq!(args[args.len() - 1], "hi");

        let profile = profile_json(&args);
        assert_eq!(profile["type"], "managed");
        assert_eq!(profile["network"], "restricted");
        let kinds = special_kinds(&profile);
        assert!(kinds.contains(&"minimal".to_string()), "{kinds:?}");
        assert!(
            !kinds
                .iter()
                .any(|kind| kind == "slash_tmp" || kind == "tmpdir"),
            "host /tmp must not be bound: {kinds:?}"
        );
        assert!(
            !kinds.iter().any(|kind| kind == "project_roots"),
            "must not use Codex project_roots workspace-write: {kinds:?}"
        );
        let dumped = profile.to_string();
        assert!(
            !dumped.contains("\"subpath\":\".git\""),
            "must not add Codex .git RO carveout: {dumped}"
        );
    }

    #[test]
    fn read_only_workspace_is_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_json(&launch_args(dir.path(), false));
        let writes = profile["file_system"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["access"] == "write")
            .count();
        assert_eq!(writes, 0, "{profile}");
    }

    #[test]
    fn writable_workspace_overrides_codex_metadata_ro() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_json(&launch_args(dir.path(), true));
        let writes = profile["file_system"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["access"] == "write")
            .count();
        assert!(
            writes >= 4,
            "workspace + .git/.agents/.codex writes: {profile}"
        );
        let dumped = profile.to_string();
        for name in PROTECTED_METADATA_NAMES {
            assert!(
                dumped.contains(name),
                "missing {name} write override: {dumped}"
            );
        }
    }

    #[test]
    fn prepare_rejects_empty_command() {
        let dir = tempfile::tempdir().unwrap();
        let err = prepare_from_helper(&dummy_helper(), &spec(dir.path(), true), &[], dir.path())
            .unwrap_err();
        assert!(err.contains("non-empty argv"), "{err}");
    }

    #[test]
    fn sandbox_env_uses_fixed_path_and_workspace_home() {
        let env = sandbox_exec_env(Path::new("/workspace"));
        assert_eq!(env.get("PATH").unwrap(), SANDBOX_PATH);
        assert_eq!(env.get("HOME").unwrap(), "/workspace");
        assert_eq!(env.get("TMPDIR").unwrap(), "/tmp");
        assert!(!env.get("PATH").unwrap().contains(".cargo/bin"));
    }
}
