//! Calls the isolated `codespace-patch` helper. That process hosts Codex
//! in-process; this crate does not depend on `codex-apply-patch` (workspace
//! feature conflicts). The helper is not the upstream standalone binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use codespace_domain::{ErrorBody, ErrorCode};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct HelperResponse {
    ok: bool,
    #[serde(default)]
    files: Option<Vec<String>>,
    #[serde(default)]
    code: Option<ErrorCode>,
    #[serde(default)]
    message: Option<String>,
}

pub fn preflight(root: &Path, patch: &str) -> Result<Vec<String>, ErrorBody> {
    invoke("preflight", root, patch, true)
}

pub fn apply(root: &Path, patch: &str, check_only: bool) -> Result<Vec<String>, ErrorBody> {
    invoke("apply", root, patch, check_only)
}

fn invoke(op: &str, root: &Path, patch: &str, check_only: bool) -> Result<Vec<String>, ErrorBody> {
    let bin = helper_bin()?;
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
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
        Ok(parsed.files.unwrap_or_default())
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

pub fn ensure_helper_for_tests() -> PathBuf {
    use std::sync::OnceLock;
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(path) = std::env::var_os("CODESPACE_PATCH_BIN") {
            let existing = PathBuf::from(path);
            if existing.is_file() {
                return existing;
            }
        }
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let known = [
            manifest_dir.join("../../target/patch-helper/debug/codespace-patch"),
            std::env::temp_dir().join("codespace-patch-helper/debug/codespace-patch"),
        ];
        for candidate in known {
            if candidate.is_file() {
                return candidate;
            }
        }
        let manifest = manifest_dir.join("../patch/Cargo.toml");
        let target = std::env::temp_dir().join("codespace-patch-helper");
        let status = Command::new("cargo")
            .arg("build")
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
