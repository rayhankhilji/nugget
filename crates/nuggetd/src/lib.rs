//! `nuggetd`, the Heed background daemon.

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
mod paging;
pub mod probes;
mod rerank;
pub mod service;
mod sources;

pub use config::{Config, DEFAULT_CONFIG};
pub use daemon::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
