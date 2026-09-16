use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use codespace_policy::Registry;
use codespace_store::Store;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::auth::authorize_headers;
use crate::config::HttpConfig;
use crate::mcp::CodeSpace;

#[derive(Clone)]
struct HttpState {
    bearer_token: Option<Arc<str>>,
}

async fn bearer_middleware(State(state): State<HttpState>, req: Request, next: Next) -> Response {
    if let Some(denied) = authorize_headers(req.headers(), state.bearer_token.as_deref()) {
        return denied;
    }
    next.run(req).await
}

pub fn router(config: HttpConfig) -> Router {
    router_with_registry(config, Registry::new())
}

pub fn router_with_registry(config: HttpConfig, registry: Registry) -> Router {
    let store = Arc::new(Store::memory().expect("in-memory operations store"));
    http_router(&config, registry, store, CancellationToken::new())
}

pub fn http_router(
    config: &HttpConfig,
    registry: Registry,
    store: Arc<Store>,
    cancel: CancellationToken,
) -> Router {
    let mut allowed_hosts = vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
        format!("{}:{}", config.host, config.port),
        format!("localhost:{}", config.port),
        format!("127.0.0.1:{}", config.port),
    ];
    allowed_hosts.sort();
    allowed_hosts.dedup();

    let registry = registry.clone();
    let service = StreamableHttpService::new(
        move || Ok(CodeSpace::with_store(registry.clone(), store.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_allowed_hosts(allowed_hosts)
            .with_cancellation_token(cancel),
    );
    let state = HttpState {
        bearer_token: config.bearer_token.clone().map(Arc::from),
    };
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(state, bearer_middleware))
}

pub async fn serve_http(
    config: HttpConfig,
    registry: Registry,
    store: Arc<Store>,
) -> anyhow::Result<(std::net::SocketAddr, CancellationToken)> {
    let addr = format!("{}:{}", config.host, config.port);
    let cancel = CancellationToken::new();
    let bearer_required = config.bearer_token.is_some();
    let router = http_router(&config, registry, store, cancel.clone());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let bound = listener.local_addr()?;
    let child = cancel.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                child.cancelled().await;
            })
            .await;
    });
    tracing::info!(
        transport = "streamable-http",
        %bound,
        path = "/mcp",
        bearer_required,
        "codespace mcp listening"
    );
    Ok((bound, cancel))
}
