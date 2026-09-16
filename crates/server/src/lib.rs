//! CodeSpace MCP gateway. Live tools: workspace_info, read, find.

pub mod auth;
pub mod config;
pub mod http;
pub mod logging;
pub mod mcp;
pub mod stdio;

pub use config::{HttpConfig, MCP_PATH};
pub use mcp::CodeSpace;
