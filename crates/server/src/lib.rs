//! CodeSpace MCP gateway. W03 still exposes `workspace_info` only.

pub mod auth;
pub mod config;
pub mod http;
pub mod logging;
pub mod mcp;
pub mod stdio;

pub use config::{HttpConfig, MCP_PATH};
pub use mcp::CodeSpace;
