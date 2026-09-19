//! Opaque 0700/0600 plan file. Codex argv lives only here. The runner
//! sees the pathname, never the bytes.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use codespace_linux_sandbox_protocol::SANDBOX_HELPER_PROTOCOL;

#[derive(Debug, Serialize, Deserialize)]
struct OpaquePlan {
    protocol: u32,
    argv: Vec<String>,
}

pub fn write_argv(argv: Vec<String>) -> Result<PathBuf, String> {
    if argv.is_empty() {
        return Err("sandbox plan argv must not be empty".into());
    }
    let dir = plan_dir()?;
    let plan = OpaquePlan {
        protocol: SANDBOX_HELPER_PROTOCOL,
        argv,
    };
    let mut last_err = "failed to create sandbox plan".to_string();
    for _ in 0..16 {
        let path = dir.join(format!("{}.plan", unique_name()));
        match write_new(&path, &plan) {
            Ok(()) => return Ok(path),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

pub fn load_and_unlink(path: &Path) -> Result<Vec<String>, String> {
    if !path.is_absolute() {
        return Err(format!(
            "sandbox plan path must be absolute: {}",
            path.display()
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|err| format!("failed to open sandbox plan: {err}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read sandbox plan: {err}"))?;
    drop(file);
    let _ = fs::remove_file(path);
    let plan: OpaquePlan =
        serde_json::from_slice(&bytes).map_err(|err| format!("invalid sandbox plan: {err}"))?;
    if plan.protocol != SANDBOX_HELPER_PROTOCOL {
        return Err(format!(
            "unsupported sandbox plan protocol {}",
            plan.protocol
        ));
    }
    if plan.argv.is_empty() || plan.argv.iter().any(|arg| arg.is_empty()) {
        return Err("sandbox plan argv must be non-empty".into());
    }
    Ok(plan.argv)
}

fn write_new(path: &Path, plan: &OpaquePlan) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|err| format!("failed to create sandbox plan: {err}"))?;
    let bytes = serde_json::to_vec(plan).map_err(|err| err.to_string())?;
    if let Err(err) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(format!("failed to write sandbox plan: {err}"));
    }
    Ok(())
}

fn plan_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("codespace-linux-sandbox-{}", std::process::id()));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|err| format!("failed to create sandbox plan dir: {err}"))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|err| format!("failed to chmod sandbox plan dir: {err}"))?;
    Ok(dir)
}

fn unique_name() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        ^ u128::from(std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_roundtrip_unlinks() {
        let path = write_argv(vec!["--sandbox-policy-cwd".into(), "/tmp".into()]).unwrap();
        assert!(path.is_file());
        let argv = load_and_unlink(&path).unwrap();
        assert_eq!(argv[0], "--sandbox-policy-cwd");
        assert!(!path.exists());
    }

    #[test]
    fn relative_plan_is_rejected() {
        let err = load_and_unlink(Path::new("relative.plan")).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }
}
