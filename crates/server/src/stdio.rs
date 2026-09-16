use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};

use crate::mcp::CodeSpace;
use codespace_policy::Registry;

pub async fn serve() -> Result<()> {
    serve_with_registry(Registry::new()).await
}

pub async fn serve_with_registry(registry: Registry) -> Result<()> {
    tracing::info!(transport = "stdio", "codespace mcp listening");
    let service = CodeSpace::new(registry).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
