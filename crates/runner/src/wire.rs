//! CodeSpace-owned runner RPC. Not App Server and not MCP.

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::{
    InProcessRunner, Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerExecRequest,
    RunnerExecResult, RunnerReadProcess, RunnerReadResult, RunnerWriteStdin,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRequest {
    pub id: u64,
    pub op: RunnerOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RunnerOpResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "data", rename_all = "snake_case")]
pub enum RunnerOp {
    Read {
        workspace: Workspace,
        path: String,
    },
    Find {
        workspace: Workspace,
        glob: Option<String>,
    },
    Version {
        workspace: Workspace,
        path: String,
    },
    ApplyPatch {
        workspace: Workspace,
        request: RunnerApplyPatchRequest,
    },
    Exec {
        workspace: Workspace,
        request: RunnerExecRequest,
    },
    WriteStdin {
        request: RunnerWriteStdin,
    },
    ReadProcess {
        request: RunnerReadProcess,
    },
    Terminate {
        process_id: ProcessId,
    },
    WorkspaceOf {
        process_id: String,
    },
    TerminateWorkspace {
        workspace_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RunnerOpResult {
    Read(ReadResult),
    Find(FindResult),
    Version(String),
    ApplyPatch(RunnerApplyPatchResult),
    Exec(RunnerExecResult),
    WriteStdin,
    ReadProcess(RunnerReadResult),
    Terminate,
    WorkspaceOf(Option<String>),
    TerminateWorkspace(u32),
}

pub async fn serve_runner_connection<S>(
    stream: S,
    runner: InProcessRunner,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let request: WireRequest = match serde_json::from_str(line.trim()) {
            Ok(request) => request,
            Err(err) => {
                let response = WireResponse {
                    id: 0,
                    ok: false,
                    result: None,
                    error: Some(ErrorBody::new(
                        ErrorCode::InvalidPatch,
                        format!("invalid runner rpc: {err}"),
                    )),
                };
                write_response(&mut write, &response).await?;
                continue;
            }
        };
        let response = dispatch(&runner, request).await;
        write_response(&mut write, &response).await?;
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    write: &mut W,
    response: &WireResponse,
) -> Result<(), std::io::Error> {
    let mut payload = serde_json::to_vec(response).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    write.write_all(&payload).await?;
    write.flush().await
}

async fn dispatch(runner: &InProcessRunner, request: WireRequest) -> WireResponse {
    let result = match request.op {
        RunnerOp::Read { workspace, path } => runner
            .read(&workspace, &path)
            .await
            .map(RunnerOpResult::Read),
        RunnerOp::Find { workspace, glob } => runner
            .find(&workspace, glob.as_deref())
            .await
            .map(RunnerOpResult::Find),
        RunnerOp::Version { workspace, path } => runner
            .version(&workspace, &path)
            .await
            .map(RunnerOpResult::Version),
        RunnerOp::ApplyPatch { workspace, request } => runner
            .apply_patch(&workspace, request)
            .await
            .map(RunnerOpResult::ApplyPatch),
        RunnerOp::Exec { workspace, request } => runner
            .exec(&workspace, request)
            .await
            .map(RunnerOpResult::Exec),
        RunnerOp::WriteStdin { request } => runner
            .write_stdin(request)
            .await
            .map(|_| RunnerOpResult::WriteStdin),
        RunnerOp::ReadProcess { request } => runner
            .read_process(request)
            .await
            .map(RunnerOpResult::ReadProcess),
        RunnerOp::Terminate { process_id } => runner
            .terminate(&process_id)
            .await
            .map(|_| RunnerOpResult::Terminate),
        RunnerOp::WorkspaceOf { process_id } => Ok(RunnerOpResult::WorkspaceOf(
            runner.workspace_of(&process_id).await,
        )),
        RunnerOp::TerminateWorkspace { workspace_id } => runner
            .terminate_workspace(&workspace_id)
            .await
            .map(RunnerOpResult::TerminateWorkspace),
    };
    match result {
        Ok(value) => WireResponse {
            id: request.id,
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => WireResponse {
            id: request.id,
            ok: false,
            result: None,
            error: Some(error),
        },
    }
}
