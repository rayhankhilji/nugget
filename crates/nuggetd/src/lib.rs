//! `nuggetd`, the Nugget background daemon.
//!
//! It loads `config.toml`, opens the encrypted store, builds the model providers, the on-device
//! runtime and the router, runs the ingest pipeline over the configured sources, and serves the
//! app over local IPC (`nugget_core::ipc`). The binary (`src/main.rs`) adds the
//! CLI, signal handling and logging; this library exists so tests can run the daemon in-process.

pub mod ask;
pub mod backup_cli;
pub mod cli;
pub mod config;
pub mod daemon;
mod features;
mod fsutil;
mod graphview;
mod handler;
mod handler_m2;
mod hub;
pub mod install;
pub mod knowledge;
mod local;
pub mod logging;
pub mod mcp;
pub mod mcp_bridge;
pub mod probes;
mod rerank;
pub mod service;
mod sources;

pub use config::{Config, DEFAULT_CONFIG};
pub use daemon::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
