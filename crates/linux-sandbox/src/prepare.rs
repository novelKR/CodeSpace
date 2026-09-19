//! Build Codex helper argv from a CodeSpace prepare request. The
//! resulting argv is written to an opaque plan file, not printed.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use codespace_linux_sandbox_protocol::{
    SandboxNetwork, SandboxPrepareRequest, SandboxPrepareResponse, SANDBOX_HELPER_PROTOCOL,
};
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath, NetworkSandboxPolicy,
};
use codex_sandboxing::landlock::create_linux_sandbox_command_args_for_permission_profile;
use codex_utils_path_uri::PathUri;

const PROTECTED_METADATA_NAMES: &[&str] = &[".git", ".agents", ".codex"];

pub fn main() {
    match read_request().and_then(prepare_plan) {
        Ok(plan_path) => emit(SandboxPrepareResponse::Prepared {
            plan_path: plan_path.to_string_lossy().into_owned(),
        }),
        Err(message) => {
            emit(SandboxPrepareResponse::Error { message });
            std::process::exit(1);
        }
    }
}

fn emit(response: SandboxPrepareResponse) {
    match serde_json::to_string(&response) {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("failed to encode prepare response: {err}");
            std::process::exit(1);
        }
    }
}

fn read_request() -> Result<SandboxPrepareRequest, String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .map_err(|err| format!("failed to read prepare request: {err}"))?;
    serde_json::from_str(&buf).map_err(|err| format!("invalid prepare request: {err}"))
}

fn prepare_plan(req: SandboxPrepareRequest) -> Result<PathBuf, String> {
    let helper =
        std::env::current_exe().map_err(|err| format!("failed to locate sandbox helper: {err}"))?;
    let argv = linux_sandbox_args(&req, &helper)?;
    crate::plan::write_argv(argv)
}

pub(crate) fn linux_sandbox_args(
    req: &SandboxPrepareRequest,
    helper: &Path,
) -> Result<Vec<String>, String> {
    if req.protocol != SANDBOX_HELPER_PROTOCOL {
        return Err(format!(
            "unsupported sandbox helper protocol {}",
            req.protocol
        ));
    }
    if req.argv.is_empty() || req.argv[0].is_empty() {
        return Err("command must be a non-empty argv (no shell)".into());
    }
    let profile = permission_profile(req, helper)?;
    let cwd = absolute_dir(Path::new(&req.command_cwd))?;
    let policy_cwd = absolute_dir(Path::new(&req.workspace_root))?;
    let args = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        create_linux_sandbox_command_args_for_permission_profile(
            req.argv.clone(),
            &cwd,
            &profile,
            &policy_cwd,
            /*use_legacy_landlock*/ false,
            matches!(req.network, SandboxNetwork::Enabled),
        )
    }))
    .map_err(|_| "failed to build linux sandbox argv".to_string())?;
    if args.iter().any(|arg| {
        arg == "--proxy-route-spec"
            || arg == "--not-a-security-boundary"
            || arg == "--use-legacy-landlock"
    }) {
        return Err("linux sandbox argv included a forbidden helper flag".into());
    }
    let has_proxy_flag = args.iter().any(|arg| arg == "--allow-network-for-proxy");
    match req.network {
        SandboxNetwork::Restricted if has_proxy_flag => {
            return Err("restricted network plan must not enable the managed proxy".into());
        }
        SandboxNetwork::Enabled if !has_proxy_flag => {
            return Err("enabled network plan must include --allow-network-for-proxy".into());
        }
        SandboxNetwork::Restricted | SandboxNetwork::Enabled => {}
    }
    Ok(args)
}

fn permission_profile(
    req: &SandboxPrepareRequest,
    helper: &Path,
) -> Result<PermissionProfile, String> {
    let mut entries = vec![FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: FileSystemSpecialPath::Minimal,
        },
        FileSystemAccessMode::Read,
    )];
    // The Codex Linux helper re-execs its own current executable inside
    // bubblewrap before applying seccomp. `Minimal` does not expose arbitrary
    // host paths, so make only this infrastructure binary readable there.
    entries.push(FileSystemSandboxEntry::new(
        path_uri(helper)?.into(),
        FileSystemAccessMode::Read,
    ));
    let workspace = path_uri(Path::new(&req.workspace_root))?;
    let access = if req.writable_workspace {
        FileSystemAccessMode::Write
    } else {
        FileSystemAccessMode::Read
    };
    entries.push(FileSystemSandboxEntry::new(
        workspace.clone().into(),
        access,
    ));
    if req.writable_workspace {
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
    let network = match req.network {
        SandboxNetwork::Restricted => NetworkSandboxPolicy::Restricted,
        SandboxNetwork::Enabled => NetworkSandboxPolicy::Enabled,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn req(root: &Path, writable: bool) -> SandboxPrepareRequest {
        req_network(root, writable, SandboxNetwork::Restricted)
    }

    fn req_network(root: &Path, writable: bool, network: SandboxNetwork) -> SandboxPrepareRequest {
        SandboxPrepareRequest {
            protocol: SANDBOX_HELPER_PROTOCOL,
            workspace_root: root.to_string_lossy().into_owned(),
            command_cwd: root.to_string_lossy().into_owned(),
            writable_workspace: writable,
            network,
            argv: vec!["/bin/echo".into(), "hi".into()],
        }
    }

    fn dummy_helper() -> PathBuf {
        PathBuf::from("/usr/bin/codespace-linux-sandbox")
    }

    fn launch_args(root: &Path, writable: bool) -> Vec<String> {
        linux_sandbox_args(&req(root, writable), &dummy_helper()).expect("prepare")
    }

    fn launch_args_network(root: &Path, network: SandboxNetwork) -> Vec<String> {
        linux_sandbox_args(&req_network(root, true, network), &dummy_helper()).expect("prepare")
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
    fn enabled_plan_has_allow_network_for_proxy_only() {
        let dir = tempfile::tempdir().unwrap();
        let args = launch_args_network(dir.path(), SandboxNetwork::Enabled);
        assert!(
            args.iter().any(|arg| arg == "--allow-network-for-proxy"),
            "Enabled must request the loopback proxy bridge: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "--proxy-route-spec"
                || arg == "--not-a-security-boundary"
                || arg == "--use-legacy-landlock"),
            "proxy route spec is attached at helper run time, not in the plan: {args:?}"
        );
        let profile = profile_json(&args);
        assert_eq!(profile["network"], "enabled");
    }

    #[test]
    fn permission_profile_reads_helper_for_inner_reexec() {
        let dir = tempfile::tempdir().unwrap();
        let profile = profile_json(&launch_args(dir.path(), true));
        let dumped = profile.to_string();
        assert!(
            dumped.contains("codespace-linux-sandbox"),
            "helper must stay readable for the inner sandbox re-exec: {dumped}"
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
        let mut request = req(dir.path(), true);
        request.argv.clear();
        let err = linux_sandbox_args(&request, &dummy_helper()).unwrap_err();
        assert!(err.contains("non-empty argv"), "{err}");
    }

    #[test]
    fn prepare_rejects_protocol_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = req(dir.path(), true);
        request.protocol = 0;
        let err = linux_sandbox_args(&request, &dummy_helper()).unwrap_err();
        assert!(err.contains("unsupported sandbox helper protocol"), "{err}");
    }
}
