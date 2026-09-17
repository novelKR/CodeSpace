//! Unix-socket Runner client. Codex types stay out of this crate.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use codespace_domain::{ErrorBody, ErrorCode, FindResult, ProcessId, ReadResult};
use codespace_policy::Workspace;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::wire::{RunnerOp, RunnerOpResult, WireRequest, WireResponse};
use crate::{
    Runner, RunnerApplyPatchRequest, RunnerApplyPatchResult, RunnerExecRequest, RunnerExecResult,
    RunnerReadProcess, RunnerReadResult, RunnerWriteStdin,
};

struct Conn {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

pub struct ContainerRunner {
    conn: Arc<Mutex<Conn>>,
    next_id: Arc<AtomicU64>,
}

impl Clone for ContainerRunner {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            next_id: self.next_id.clone(),
        }
    }
}

impl ContainerRunner {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, ErrorBody> {
        let stream = UnixStream::connect(path.as_ref()).await.map_err(io_err)?;
        Ok(Self::from_stream(stream))
    }

    pub fn from_stream(stream: UnixStream) -> Self {
        let (read, write) = stream.into_split();
        Self {
            conn: Arc::new(Mutex::new(Conn {
                reader: BufReader::new(read),
                writer: write,
            })),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn call(&self, op: RunnerOp) -> Result<RunnerOpResult, ErrorBody> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = WireRequest { id, op };
        let mut conn = self.conn.lock().await;
        let mut payload = serde_json::to_vec(&request)
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        payload.push(b'\n');
        conn.writer.write_all(&payload).await.map_err(io_err)?;
        conn.writer.flush().await.map_err(io_err)?;
        let mut line = String::new();
        conn.reader.read_line(&mut line).await.map_err(io_err)?;
        if line.is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidPatch,
                "runner socket closed",
            ));
        }
        let response: WireResponse = serde_json::from_str(line.trim())
            .map_err(|err| ErrorBody::new(ErrorCode::InvalidPatch, err.to_string()))?;
        if response.ok {
            response
                .result
                .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "runner rpc missing result"))
        } else {
            Err(response
                .error
                .unwrap_or_else(|| ErrorBody::new(ErrorCode::InvalidPatch, "runner rpc failed")))
        }
    }
}

fn io_err(err: std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidPatch, err.to_string())
}

fn unexpected(result: RunnerOpResult) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidPatch,
        format!("unexpected runner rpc result: {result:?}"),
    )
}

impl Runner for ContainerRunner {
    async fn read(&self, ws: &Workspace, path: &str) -> Result<ReadResult, ErrorBody> {
        match self
            .call(RunnerOp::Read {
                workspace: ws.clone(),
                path: path.to_string(),
            })
            .await?
        {
            RunnerOpResult::Read(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn find(&self, ws: &Workspace, glob: Option<&str>) -> Result<FindResult, ErrorBody> {
        match self
            .call(RunnerOp::Find {
                workspace: ws.clone(),
                glob: glob.map(str::to_string),
            })
            .await?
        {
            RunnerOpResult::Find(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn version(&self, ws: &Workspace, path: &str) -> Result<String, ErrorBody> {
        match self
            .call(RunnerOp::Version {
                workspace: ws.clone(),
                path: path.to_string(),
            })
            .await?
        {
            RunnerOpResult::Version(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn apply_patch(
        &self,
        ws: &Workspace,
        req: RunnerApplyPatchRequest,
    ) -> Result<RunnerApplyPatchResult, ErrorBody> {
        match self
            .call(RunnerOp::ApplyPatch {
                workspace: ws.clone(),
                request: req,
            })
            .await?
        {
            RunnerOpResult::ApplyPatch(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn exec(
        &self,
        ws: &Workspace,
        req: RunnerExecRequest,
    ) -> Result<RunnerExecResult, ErrorBody> {
        match self
            .call(RunnerOp::Exec {
                workspace: ws.clone(),
                request: req,
            })
            .await?
        {
            RunnerOpResult::Exec(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn write_stdin(&self, req: RunnerWriteStdin) -> Result<(), ErrorBody> {
        match self.call(RunnerOp::WriteStdin { request: req }).await? {
            RunnerOpResult::WriteStdin => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    async fn read_process(&self, req: RunnerReadProcess) -> Result<RunnerReadResult, ErrorBody> {
        match self.call(RunnerOp::ReadProcess { request: req }).await? {
            RunnerOpResult::ReadProcess(result) => Ok(result),
            other => Err(unexpected(other)),
        }
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), ErrorBody> {
        match self
            .call(RunnerOp::Terminate {
                process_id: process_id.clone(),
            })
            .await?
        {
            RunnerOpResult::Terminate => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    async fn workspace_of(&self, process_id: &str) -> Option<String> {
        match self
            .call(RunnerOp::WorkspaceOf {
                process_id: process_id.to_string(),
            })
            .await
            .ok()?
        {
            RunnerOpResult::WorkspaceOf(value) => value,
            _ => None,
        }
    }

    async fn terminate_workspace(&self, workspace_id: &str) -> Result<u32, ErrorBody> {
        match self
            .call(RunnerOp::TerminateWorkspace {
                workspace_id: workspace_id.to_string(),
            })
            .await?
        {
            RunnerOpResult::TerminateWorkspace(count) => Ok(count),
            other => Err(unexpected(other)),
        }
    }
}
