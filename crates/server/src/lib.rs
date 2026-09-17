//! CodeSpace MCP gateway. Live tools include process supervision.

pub mod auth;
pub mod config;
pub mod http;
pub mod logging;
pub mod mcp;
pub mod patch_helper;
pub mod protocol;
pub mod rollback;
pub mod stdio;
pub mod supervisor;

pub use config::{HttpConfig, MCP_PATH};
pub use mcp::CodeSpace;
pub use protocol::{NegotiatedFeatures, CORE_BASELINE, ENHANCEMENT, HTTP_FLOOR};
