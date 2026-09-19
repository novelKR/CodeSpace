//! Opaque 0700/0600 plan file. Codex argv lives only here. The runner
//! sees the pathname, never the bytes.

use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use codespace_linux_sandbox_protocol::SANDBOX_HELPER_PROTOCOL;

const PLAN_FILE_NAME: &str = "plan";
const PLAN_DIR_PREFIX: &str = "codespace-linux-sandbox-";

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
    let path = dir.join(PLAN_FILE_NAME);
    let plan = OpaquePlan {
        protocol: SANDBOX_HELPER_PROTOCOL,
        argv,
    };
    if let Err(err) = write_new(&path, &plan) {
        discard(&path);
        return Err(err);
    }
    Ok(path)
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
    discard(path);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read sandbox plan: {err}"))?;
    drop(file);
    let plan: OpaquePlan =
        serde_json::from_slice(&bytes).map_err(|err| format!("invalid sandbox plan: {err}"))?;
    if plan.protocol != SANDBOX_HELPER_PROTOCOL {
        return Err(format!(
            "unsupported sandbox plan protocol {}",
            plan.protocol
        ));
    }
    if plan.argv.is_empty() {
        return Err("sandbox plan argv must not be empty".into());
    }
    Ok(plan.argv)
}

/// Unlink the plan file and remove its now-empty parent directory.
pub fn discard(path: &Path) {
    let parent = path.parent().map(Path::to_path_buf);
    let _ = fs::remove_file(path);
    if let Some(parent) = parent {
        if parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(PLAN_DIR_PREFIX))
        {
            let _ = fs::remove_dir(parent);
        }
    }
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
        return Err(format!("failed to write sandbox plan: {err}"));
    }
    Ok(())
}

fn plan_dir() -> Result<PathBuf, String> {
    let mut template = std::env::temp_dir();
    template.push(format!("{PLAN_DIR_PREFIX}XXXXXX"));
    let cstr = CString::new(template.as_os_str().as_bytes())
        .map_err(|_| "sandbox plan dir template contains interior NUL".to_string())?;
    let mut buf = cstr.into_bytes_with_nul();
    let ptr = unsafe { libc::mkdtemp(buf.as_mut_ptr().cast()) };
    if ptr.is_null() {
        return Err(format!(
            "failed to create sandbox plan dir: {}",
            std::io::Error::last_os_error()
        ));
    }
    buf.pop();
    Ok(PathBuf::from(OsStr::from_bytes(&buf)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_roundtrip_unlinks() {
        let path = write_argv(vec!["--sandbox-policy-cwd".into(), "/tmp".into()]).unwrap();
        let parent = path.parent().unwrap().to_path_buf();
        assert!(path.is_file());
        assert!(parent.is_dir());
        let argv = load_and_unlink(&path).unwrap();
        assert_eq!(argv[0], "--sandbox-policy-cwd");
        assert!(!path.exists());
        assert!(!parent.exists());
    }

    #[test]
    fn plan_preserves_empty_argument() {
        let path = write_argv(vec![
            "--sandbox-policy-cwd".into(),
            "/tmp".into(),
            String::new(),
        ])
        .unwrap();
        let argv = load_and_unlink(&path).unwrap();
        assert_eq!(
            argv,
            vec![
                "--sandbox-policy-cwd".to_string(),
                "/tmp".into(),
                String::new()
            ]
        );
    }

    #[test]
    fn plan_symlink_is_rejected() {
        let path = write_argv(vec!["--sandbox-policy-cwd".into(), "/tmp".into()]).unwrap();
        let parent = path.parent().unwrap().to_path_buf();
        let target = parent.join("target");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = load_and_unlink(&path).unwrap_err();
        assert!(err.contains("failed to open"), "{err}");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_dir(&parent);
    }

    #[test]
    fn relative_plan_is_rejected() {
        let err = load_and_unlink(Path::new("relative.plan")).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }
}
