//! meiose: a context-trimming proxy for MCP servers.
//!
//! The binary lives in `main.rs`; this library exposes the pieces that tests
//! and benchmarks exercise directly.

pub mod cli;
pub mod diff;
pub mod doctor;
pub mod frame;
pub mod handlers;
pub mod install;
pub mod intercept;
pub mod logging;
pub mod paths;
pub mod proxy;
pub mod record;
pub mod report;
pub mod rpc;
pub mod setup;
pub mod store;
pub mod tokens;
