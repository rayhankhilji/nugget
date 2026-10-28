//! Shared types and contracts for Nugget crates.
//!
//! Everything that crosses a crate boundary lives here so crates can be built in parallel
//! against a stable contract. See `docs/m0-spec.md`.

pub mod capability;
pub mod egress;
pub mod error;
pub mod ids;
pub mod ipc;
pub mod paths;

pub use error::{Error, Result};
