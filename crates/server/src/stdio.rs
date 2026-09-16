use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};

use crate::mcp::CodeSpace;

pub async fn serve() -> Result<()> {
    tracing::info!(transport = "stdio", "codespace mcp listening");
    let service = CodeSpace::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
