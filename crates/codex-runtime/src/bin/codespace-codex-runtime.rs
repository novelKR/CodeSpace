//! Isolated runner worker. Binds the given `$dir/runner.sock` (or a
//! newly allocated unique dir when run standalone). Parent chmod is the
//! gateway's job: this process never calls
//! `prepare_private_socket_directory`. One `InProcessRunner` per
//! process. P0 is 1:1 — serve one connection, then exit.

use std::path::PathBuf;

use codespace_runner::{
    allocate_private_runner_dir, host_worker, reclaim_leftover_socket, runner_socket_path,
    serve_runner_connection,
};

#[tokio::main]
async fn main() {
    codex_process_hardening::pre_main_hardening();
    let socket = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => match std::env::var_os("CODESPACE_RUNNER_SOCKET") {
            Some(path) => PathBuf::from(path),
            None => {
                let dir = allocate_private_runner_dir(None).expect("private runner dir");
                let socket = runner_socket_path(&dir);
                eprintln!("codespace-codex-runtime listening on {}", socket.display());
                socket
            }
        },
    };
    reclaim_leftover_socket(&socket).expect("runner socket");
    let mut listener = codex_uds::UnixListener::bind(&socket)
        .await
        .expect("bind runner socket");
    let (runner, events) = host_worker();
    let stream = listener.accept().await.expect("accept runner socket");
    // Keep the listener bound so a live connect-probe sees AddrInUse
    // instead of unlinking this rendezvous.
    let _listener = listener;
    let _ = serve_runner_connection(stream, runner, events).await;
}
