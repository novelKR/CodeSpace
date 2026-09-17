//! Isolated Codex adapter helper. Invoked by the MCP gateway over JSON stdin/stdout.
//! This is CodeSpace's adapter process, not the upstream standalone `apply_patch` binary.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use codespace_domain::{ErrorBody, ErrorCode, FileChange, Profile, WorkspaceId};
use codespace_patch::{apply_in_workspace, ApplyOutcome};
use codespace_policy::Workspace;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    root: PathBuf,
    patch: String,
    #[serde(default)]
    check_only: bool,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Vec<FileChange>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn fail(err: ErrorBody) -> Response {
    Response {
        ok: false,
        files: None,
        changes: None,
        code: Some(err.code),
        message: Some(err.message),
    }
}

#[tokio::main]
async fn main() {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).expect("read stdin");
    let req: Request = match serde_json::from_str(&buf) {
        Ok(req) => req,
        Err(err) => {
            let _ = write_resp(&fail(ErrorBody::new(
                ErrorCode::InvalidPatch,
                err.to_string(),
            )));
            return;
        }
    };
    let ws = Workspace {
        id: WorkspaceId("helper".into()),
        root: req.root,
        profile: Profile::WorkspaceWrite,
    };
    let resp = match req.op.as_str() {
        "preflight" => match apply_in_workspace(&ws, &req.patch, true).await {
            Ok(ApplyOutcome { files, changes }) => Response {
                ok: true,
                files: Some(files),
                changes: Some(changes),
                code: None,
                message: None,
            },
            Err(err) => fail(err),
        },
        "apply" => match apply_in_workspace(&ws, &req.patch, req.check_only).await {
            Ok(ApplyOutcome { files, changes }) => Response {
                ok: true,
                files: Some(files),
                changes: Some(changes),
                code: None,
                message: None,
            },
            Err(err) => fail(err),
        },
        other => fail(ErrorBody::new(
            ErrorCode::InvalidPatch,
            format!("unknown helper op {other}"),
        )),
    };
    let _ = write_resp(&resp);
}

fn write_resp(resp: &Response) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, resp)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}
