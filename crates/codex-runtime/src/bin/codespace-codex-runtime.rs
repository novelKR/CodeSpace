//! Isolated runner worker. Binds a CodeSpace RPC Unix socket with Codex UDS helpers.

use std::path::PathBuf;
use std::sync::Arc;

use codespace_runner::{serve_runner_connection, InProcessRunner};

#[tokio::main]
async fn main() {
    codex_process_hardening::pre_main_hardening();
    let socket = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CODESPACE_RUNNER_SOCKET").map(PathBuf::from))
        .expect("socket path argument or CODESPACE_RUNNER_SOCKET");
    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            codex_uds::prepare_private_socket_directory(parent)
                .await
                .expect("private socket directory");
        }
    }
    if socket.exists() {
        match codex_uds::is_stale_socket_path(&socket).await {
            Ok(true) | Err(_) => {
                let _ = std::fs::remove_file(&socket);
            }
            Ok(false) => {}
        }
    }
    let mut listener = codex_uds::UnixListener::bind(&socket)
        .await
        .expect("bind runner socket");
    loop {
        let stream = listener.accept().await.expect("accept runner socket");
        let runner = InProcessRunner::new(Arc::new(|_| {}));
        tokio::spawn(async move {
            let _ = serve_runner_connection(stream, runner).await;
        });
    }
}
