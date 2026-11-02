//! meiose: a context-trimming proxy for MCP servers.
//!
//! The binary lives in `main.rs`; this library exposes the pieces that tests
//! and benchmarks exercise directly.

pub mod cli;
pub mod frame;
pub mod handlers;
pub mod logging;
pub mod paths;
pub mod proxy;
pub mod record;
pub mod tokens;
