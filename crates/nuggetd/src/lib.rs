//! `nuggetd`, the Nugget background daemon.
//!
//! It loads `config.toml`, opens the encrypted store, builds the model providers and router,
//! and serves the app over local IPC (`nugget_core::ipc`). The binary (`src/main.rs`) adds the
//! CLI, signal handling and logging; this library exists so tests can run the daemon in-process.

pub mod cli;
pub mod config;
pub mod daemon;
mod fsutil;
mod handler;
pub mod logging;
pub mod probes;
pub mod service;

pub use config::{Config, DEFAULT_CONFIG};
pub use daemon::{Daemon, DaemonOptions, Startup};
