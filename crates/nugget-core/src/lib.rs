//! Shared types and contracts for Heed crates.
//!
//! Everything that crosses a crate boundary lives here so crates can be built in parallel
//! against a stable contract.

pub mod capability;
pub mod clock;
pub mod egress;
pub mod env;
pub mod error;
pub mod identity;
pub mod ids;
pub mod ipc;
pub mod lang;
pub mod migrate;
pub mod paths;
pub mod secrets;

pub use error::{Error, Result};
