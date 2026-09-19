//! Host-side managed `NetworkProxy` for Enabled `run --plan`.
//! Restricted still `exec`s Codex argv in this process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use codex_network_proxy::{
    NetworkProxy, NetworkProxyConfig, NetworkProxyHandle, NetworkProxyState,
    RemoteNetworkProxyConfig, RemoteNetworkProxyLaunchConfig, NO_PROXY_ENV_KEYS,
};

pub fn run_codex_with_proxy(argv: &[String]) -> ! {
    become_group_leader();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("failed to start network proxy runtime: {err}");
            std::process::exit(1);
        }
    };
    let (proxy, handle) = match runtime.block_on(start_proxy()) {
        Ok(started) => started,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    let mut env: HashMap<String, String> = std::env::vars().collect();
    proxy.apply_to_env(&mut env);
    // Local destinations are enforced by the proxy. Codex's default NO_PROXY
    // would let sandbox clients bypass it for 127.0.0.1.
    route_loopback_through_proxy(&mut env);
    let status = match spawn_codex(argv, &env) {
        Ok(mut child) => child.wait(),
        Err(err) => {
            let _ = runtime.block_on(handle.shutdown());
            eprintln!("failed to spawn linux sandbox helper: {err}");
            std::process::exit(1);
        }
    };
    let _ = runtime.block_on(handle.shutdown());
    drop(proxy);
    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("failed to wait for linux sandbox helper: {err}");
            std::process::exit(1);
        }
    }
}

async fn start_proxy() -> Result<(NetworkProxy, NetworkProxyHandle), String> {
    let mut config = NetworkProxyConfig {
        enabled: true,
        allow_local_binding: true,
        ..NetworkProxyConfig::default()
    };
    // Destination allowlists are out of scope; Enabled HTTP goes through this proxy.
    config.set_allowed_domains(vec!["*".to_string()]);
    let remote = RemoteNetworkProxyConfig::from_effective_config(&config)
        .map_err(|err| format!("failed to build network proxy config: {err}"))?;
    let state =
        NetworkProxyState::from_remote_launch_config(RemoteNetworkProxyLaunchConfig::new(remote))
            .map_err(|err| format!("failed to build network proxy state: {err}"))?;
    let proxy = NetworkProxy::builder()
        .state(Arc::new(state))
        .build()
        .await
        .map_err(|err| format!("failed to bind network proxy: {err}"))?;
    let handle = proxy
        .run()
        .await
        .map_err(|err| format!("failed to start network proxy: {err}"))?;
    Ok((proxy, handle))
}

fn spawn_codex(
    argv: &[String],
    env: &HashMap<String, String>,
) -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codespace-linux-sandbox"));
    let mut cmd = Command::new(exe);
    cmd.args(argv).env_clear().envs(env);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() == 1 {
                    libc::raise(libc::SIGKILL);
                }
                Ok(())
            });
        }
    }
    cmd.spawn()
}

fn become_group_leader() {
    #[cfg(unix)]
    unsafe {
        libc::setpgid(0, 0);
    }
}

fn route_loopback_through_proxy(env: &mut HashMap<String, String>) {
    for key in NO_PROXY_ENV_KEYS {
        env.insert((*key).to_string(), String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_network_proxy::DEFAULT_NO_PROXY_VALUE;

    #[test]
    fn route_loopback_through_proxy_clears_bypass_keys() {
        let mut env = HashMap::new();
        env.insert("HTTP_PROXY".into(), "http://127.0.0.1:9".into());
        env.insert("HTTPS_PROXY".into(), "http://127.0.0.1:9".into());
        for key in NO_PROXY_ENV_KEYS {
            env.insert((*key).to_string(), DEFAULT_NO_PROXY_VALUE.to_string());
        }
        route_loopback_through_proxy(&mut env);
        assert_eq!(
            env.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:9")
        );
        assert_eq!(
            env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://127.0.0.1:9")
        );
        for key in NO_PROXY_ENV_KEYS {
            assert_eq!(env.get(*key).map(String::as_str), Some(""), "{key}");
        }
    }
}
