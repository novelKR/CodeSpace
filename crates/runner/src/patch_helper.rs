//! Calls the isolated `codespace-patch` helper. That process hosts Codex
//! in-process; this crate does not depend on `codex-apply-patch` (workspace
//! feature conflicts). The helper is not the upstream standalone binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

use codespace_domain::{ErrorBody, ErrorCode, FileChange};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const HELPER_IO_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct HelperResponse {
    ok: bool,
    #[serde(default)]
    files: Option<Vec<String>>,
    #[serde(default)]
    changes: Option<Vec<FileChange>>,
    #[serde(default)]
    code: Option<ErrorCode>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HelperSuccess {
    pub files: Vec<String>,
    pub changes: Vec<FileChange>,
}

pub async fn preflight(root: &Path, patch: &str) -> Result<HelperSuccess, ErrorBody> {
    invoke("preflight", root, patch, true).await
}

pub async fn apply(root: &Path, patch: &str, check_only: bool) -> Result<HelperSuccess, ErrorBody> {
    invoke("apply", root, patch, check_only).await
}

async fn invoke(
    op: &str,
    root: &Path,
    patch: &str,
    check_only: bool,
) -> Result<HelperSuccess, ErrorBody> {
    let bin = helper_bin()?;
    let mut child = Command::new(&bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            ErrorBody::new(
                ErrorCode::InvalidPatch,
                format!("failed to spawn patch helper: {err}"),
            )
        })?;
    let payload = serde_json::json!({
        "op": op,
        "root": root,
        "patch": patch,
        "check_only": check_only,
    });
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.to_string().as_bytes())
            .await
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
    }
    let output = tokio::time::timeout(HELPER_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| ErrorBody::new(ErrorCode::Timeout, "patch helper timed out"))?
        .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
    if output.stdout.len() > HELPER_IO_LIMIT || output.stderr.len() > HELPER_IO_LIMIT {
        return Err(ErrorBody::new(
            ErrorCode::OutputLimit,
            "patch helper output exceeded 1 MiB",
        ));
    }
    if !output.status.success() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidPatch,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let parsed: HelperResponse = serde_json::from_slice(&output.stdout).map_err(|err| {
        ErrorBody::new(
            ErrorCode::InvalidPatch,
            format!("patch helper returned invalid JSON: {err}"),
        )
    })?;
    if parsed.ok {
        Ok(HelperSuccess {
            files: parsed.files.unwrap_or_default(),
            changes: parsed.changes.unwrap_or_default(),
        })
    } else {
        Err(ErrorBody::new(
            parsed.code.unwrap_or(ErrorCode::InvalidPatch),
            parsed
                .message
                .unwrap_or_else(|| "patch helper failed".into()),
        ))
    }
}

fn helper_bin() -> Result<PathBuf, ErrorBody> {
    if let Some(path) = std::env::var_os("CODESPACE_PATCH_BIN") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("codespace-patch");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(ErrorBody::new(
        ErrorCode::InvalidPatch,
        "CODESPACE_PATCH_BIN is unset and codespace-patch was not found next to codespace-mcp",
    ))
}

/// Test-only helper path: the `CODESPACE_PATCH_BIN` that
/// `scripts/validate-upstream.py` builds and exports before the root tests,
/// or else a locked build of `crates/patch` into a temporary target dir.
pub fn ensure_helper_for_tests() -> PathBuf {
    use std::process::Command as StdCommand;
    use std::sync::OnceLock;
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(bin) = std::env::var_os("CODESPACE_PATCH_BIN")
            .map(PathBuf::from)
            .filter(|bin| bin.is_file())
        {
            return bin;
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patch/Cargo.toml");
        let target = std::env::temp_dir().join("codespace-patch-helper");
        let status = StdCommand::new("cargo")
            .arg("build")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(&manifest)
            .arg("--bin")
            .arg("codespace-patch")
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .expect("spawn cargo for patch helper");
        assert!(status.success(), "codespace-patch helper failed to build");
        let bin = target.join("debug/codespace-patch");
        assert!(bin.is_file(), "missing {bin:?}");
        bin
    })
    .clone()
}
