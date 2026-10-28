//! Registration of the statically linked `sqlite-vec` extension.

use std::sync::OnceLock;

use nugget_core::{Error, Result};
use rusqlite::auto_extension::{RawAutoExtension, register_auto_extension};

/// Register `sqlite-vec` as an auto-extension so every connection opened afterwards (in this
/// process) gets the `vec0` virtual table and `vec_*` functions. Idempotent.
pub(crate) fn ensure_registered() -> Result<()> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| register().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| Error::Storage(format!("cannot register sqlite-vec: {e}")))
}

/// The only `unsafe` in this crate.
#[allow(unsafe_code)]
fn register() -> rusqlite::Result<()> {
    // SAFETY: `sqlite3_vec_init` is the extension entry point compiled (with `SQLITE_CORE`) and
    // statically linked by the `sqlite-vec` crate. Its real C signature is
    // `int sqlite3_vec_init(sqlite3*, char**, const sqlite3_api_routines*)`, which is exactly
    // `RawAutoExtension`; the crate merely declares it as `fn()`, so the fn-pointer transmute
    // restores the true signature (same size and ABI). The entry point only registers functions
    // and modules on the connection it is given: it never opens a database, closes `db`, or
    // touches the auto-extension list, which are the safety conditions of
    // `register_auto_extension`.
    unsafe {
        let entry = std::mem::transmute::<unsafe extern "C" fn(), RawAutoExtension>(
            sqlite_vec::sqlite3_vec_init,
        );
        register_auto_extension(entry)
    }
}
