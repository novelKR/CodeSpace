use std::sync::Arc;

use anyhow::Result;
use codespace_policy::Registry;
use codespace_runner::RuntimeBackend;
use codespace_store::Store;
use rmcp::{transport::stdio, ServiceExt};

use crate::mcp::CodeSpace;

pub async fn serve() -> Result<()> {
    serve_with_registry(Registry::new()).await
}

pub async fn serve_with_registry(registry: Registry) -> Result<()> {
    let store = Arc::new(Store::memory().map_err(anyhow::Error::msg)?);
    serve_with(registry, store).await
}

pub async fn serve_with(registry: Registry, store: Arc<Store>) -> Result<()> {
    tracing::info!(transport = "stdio", "codespace mcp listening");
    let service = CodeSpace::with_store(registry, store)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

pub async fn serve_with_runner(
    registry: Registry,
    store: Arc<Store>,
    runner: RuntimeBackend,
) -> Result<()> {
    serve_handler(CodeSpace::with_store_and_runner(registry, store, runner)).await
}

/// Serve a handler built by the caller, such as one that reports a resource authority.
pub async fn serve_handler(handler: CodeSpace) -> Result<()> {
    tracing::info!(transport = "stdio", "codespace mcp listening");
    let service = handler.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
