//! `nuggetd backup [--to DIR]`, `nuggetd backup set-passphrase`, `nuggetd restore --from DIR`
//! and `nuggetd export --to DIR`.
//!
//! Backup, set-passphrase and export go through the running daemon's IPC when one answers, and
//! otherwise work offline on the store directly, with the same library code; while offline the
//! command holds the daemon's IPC socket so no daemon can open the store meanwhile. Restore is
//! offline only (it refuses while a daemon answers): it verifies and stages the backup and
//! applies it at once, exactly as a daemon does at start.
//!
//! The backup passphrase comes from `--passphrase-file`, else `NUGGET_BACKUP_PASSPHRASE`, else
//! a no-echo prompt when stdin is a terminal (Unix; elsewhere use the file or the variable).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ipc::{Client, Listener};
use nugget_store::{FileHealth, Store, StoreKey};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::config::Config;
use crate::features::backup::{disk, export, restore};

/// Environment variable holding the backup passphrase (scripts).
pub const PASSPHRASE_ENV: &str = "NUGGET_BACKUP_PASSPHRASE";

/// `nuggetd backup [--to DIR]`. `default_vault` is the vault directory when `[vault] path` is
/// unset (as the daemon would use).
pub async fn backup(paths: &Paths, to: Option<PathBuf>, default_vault: &Path) -> Result<String> {
    let to = to.map(absolute).transpose()?;
    if let Some(client) = daemon(paths).await {
        let params = match &to {
            Some(d) => json!({"folder": d}),
            None => json!({}),
        };
        let info: disk::BackupInfo =
            serde_json::from_value(rpc(&client, "backup.run", params).await?)?;
        return Ok(backup_line(&info));
    }
    let _hold = hold_socket(paths).await?;
    let (paths, default_vault) = (paths.clone(), default_vault.to_path_buf());
    let info = crate::daemon::blocking(move || {
        let (store, key) = open_offline(&paths)?;
        let settings = disk::Settings::load(&paths)?;
        let configured = settings.folder();
        let folder = to
            .clone()
            .or_else(|| configured.clone())
            .ok_or_else(|| Error::Invalid("no backup folder configured: pass --to DIR".into()))?;
        let keep = (configured.as_ref() == Some(&folder)).then_some(settings.keep);
        let vault = vault_root(&paths, &default_vault)?;
        check_outside(&paths, &folder, vault.as_deref())?;
        let result = disk::run_backup(&disk::BackupRequest {
            paths: &paths,
            store: &store,
            key: &key,
            folder: &folder,
            keep,
            vault_root: vault.as_deref(),
        });
        let mut s = disk::Settings::load(&paths)?;
        match &result {
            Ok(info) => {
                s.last_backup = Some(info.clone());
                s.last_error = None;
                s.last_failure_at = None;
            }
            Err(e) => {
                s.last_error = Some(e.to_string());
                s.last_failure_at = Some(disk::rfc3339(time::OffsetDateTime::now_utc()));
            }
        }
        s.save(&paths)?;
        store.flush_egress();
        result
    })
    .await?;
    Ok(backup_line(&info))
}

fn backup_line(info: &disk::BackupInfo) -> String {
    format!(
        "{}\nbacked up {} items ({} bytes)",
        info.path, info.items, info.bytes
    )
}

/// `nuggetd backup set-passphrase [--passphrase-file FILE]`.
pub async fn set_passphrase(paths: &Paths, file: Option<&Path>) -> Result<String> {
    let passphrase = passphrase(file, true)?.ok_or_else(no_passphrase)?;
    disk::check_passphrase(&passphrase)?;
    if let Some(client) = daemon(paths).await {
        rpc(
            &client,
            "backup.set_passphrase",
            json!({"passphrase": passphrase.as_str()}),
        )
        .await?;
        return Ok("backup passphrase set".into());
    }
    let _hold = hold_socket(paths).await?;
    let paths = paths.clone();
    crate::daemon::blocking(move || {
        let key = StoreKey::load_existing(&paths)?
            .ok_or_else(|| Error::NotFound("this home has no store key yet".into()))?;
        disk::set_passphrase(&paths, &key, &passphrase)
    })
    .await?;
    Ok("backup passphrase set".into())
}

/// `nuggetd export --to DIR`.
pub async fn export(paths: &Paths, to: PathBuf, default_vault: &Path) -> Result<String> {
    let to = absolute(to)?;
    if let Some(client) = daemon(paths).await {
        let r = rpc(&client, "export.run", json!({"folder": to})).await?;
        return Ok(export_line(&r));
    }
    let _hold = hold_socket(paths).await?;
    let (paths, default_vault) = (paths.clone(), default_vault.to_path_buf());
    let r = crate::daemon::blocking(move || {
        let (store, _) = open_offline(&paths)?;
        let vault = vault_root(&paths, &default_vault)?;
        check_outside(&paths, &to, vault.as_deref())?;
        let r = export::run_export(&store, &to, vault.as_deref(), &mut |phase, done| {
            tracing::debug!(phase, done, "export progress");
        })?;
        store.flush_egress();
        Ok(serde_json::to_value(r)?)
    })
    .await?;
    Ok(export_line(&r))
}

fn export_line(r: &Value) -> String {
    let n = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
    format!(
        "{}\nexported {} items, {} entities, {} claims, {} audit entries, {} egress rows, {} vault files",
        r.get("path").and_then(Value::as_str).unwrap_or(""),
        n("items"),
        n("entities"),
        n("claims"),
        n("audit_entries"),
        n("egress_rows"),
        n("vault_files")
    )
}

/// `nuggetd restore --from DIR [--passphrase-file FILE]` (offline only).
pub async fn restore(
    paths: &Paths,
    from: PathBuf,
    file: Option<&Path>,
    default_vault: &Path,
) -> Result<String> {
    let from = absolute(from)?;
    if daemon(paths).await.is_some() {
        return Err(Error::Other(
            "nuggetd is running: stop it first (restore replaces the database it has open), or restore from the app".into(),
        ));
    }
    let _hold = hold_socket(paths).await?;
    let current = {
        let paths = paths.clone();
        crate::daemon::blocking(move || StoreKey::load_existing(&paths)).await?
    };
    let manifest = disk::Manifest::read(&from)?;
    let needs_passphrase = current
        .as_ref()
        .is_none_or(|k| k.key_id() != manifest.key_id);
    let pass = if needs_passphrase {
        Some(passphrase(file, false)?.ok_or_else(no_passphrase)?)
    } else {
        None
    };
    let (paths, default_vault) = (paths.clone(), default_vault.to_path_buf());
    crate::daemon::blocking(move || {
        let db = paths.db_file();
        let aside = match &current {
            Some(k) => match nugget_store::check_file(&db, k, false) {
                Ok(FileHealth::Corrupt(_) | FileHealth::WrongKey) => restore::Aside::Corrupt,
                _ => restore::Aside::PreRestore,
            },
            None => restore::Aside::PreRestore,
        };
        let ready = restore::stage(
            &paths,
            &restore::StageRequest {
                dir: &from,
                current: current.as_ref(),
                passphrase: pass.as_deref().map(String::as_str),
                aside,
                default_vault: Some(&default_vault),
            },
        )?;
        let (key, applied) = restore::apply_pending(&paths, current.as_ref())?
            .ok_or_else(|| Error::Other("the staged restore disappeared".into()))?;
        let store = Store::open(&db, &key)?;
        restore::finish_applied(&paths, &store, Some(&applied))?;
        let counts = store.counts()?;
        store.flush_egress();
        let mut out = format!(
            "restored {} (made {}): {} items, {} chunks",
            ready.source, ready.created_at, counts.items, counts.chunks
        );
        if let Some(m) = &applied.moved_to {
            out.push_str(&format!(
                "\nthe previous database was kept as {}",
                m.display()
            ));
        }
        Ok(out)
    })
    .await
}

/// A client of the running daemon, if one answers.
async fn daemon(paths: &Paths) -> Option<Client> {
    Client::connect(paths).await.ok()
}

async fn rpc(client: &Client, method: &str, params: Value) -> Result<Value> {
    client
        .call_value(method, params)
        .await
        .map_err(|e| match e {
            nugget_ipc::IpcError::Rpc(r) => Error::from(r),
            other => Error::Other(other.to_string()),
        })
}

/// Bind the daemon's socket for the duration of an offline command, so no daemon starts on
/// this home meanwhile.
async fn hold_socket(paths: &Paths) -> Result<Listener> {
    nugget_ipc::bind(paths).await.map_err(|e| {
        Error::Other(format!(
            "cannot take the daemon's socket ({e}): is nuggetd starting or running?"
        ))
    })
}

/// Open the store of a stopped daemon, refusing a damaged database (never written).
fn open_offline(paths: &Paths) -> Result<(Store, StoreKey)> {
    let key = StoreKey::load_existing(paths)?
        .ok_or_else(|| Error::NotFound("this home has no store key".into()))?;
    let db = paths.db_file();
    match nugget_store::check_file(&db, &key, false)? {
        FileHealth::Healthy => {}
        FileHealth::Missing => {
            return Err(Error::NotFound(format!("no database at {}", db.display())));
        }
        FileHealth::WrongKey => {
            return Err(Error::Integrity(format!(
                "the store key does not open {}",
                db.display()
            )));
        }
        FileHealth::Corrupt(why) => {
            return Err(Error::Integrity(format!(
                "{} is damaged ({why}); restore it from a backup",
                db.display()
            )));
        }
    }
    Ok((Store::open(&db, &key)?, key))
}

/// Refuse a folder inside (or containing) this home's data, config or vault directory.
fn check_outside(paths: &Paths, folder: &Path, vault: Option<&Path>) -> Result<()> {
    let mut protected: Vec<(&str, &Path)> = vec![("data", &paths.data), ("config", &paths.config)];
    if let Some(v) = vault {
        protected.push(("vault", v));
    }
    disk::check_folder_outside(folder, &protected)
}

/// The vault directory the daemon would use, if the vault is enabled.
fn vault_root(paths: &Paths, default_vault: &Path) -> Result<Option<PathBuf>> {
    let config = if paths.config_file().exists() {
        Config::load_or_create(paths)?.0
    } else {
        Config::default_local()
    };
    if !config.vault.enabled {
        return Ok(None);
    }
    crate::knowledge::vault_root(&config.vault, default_vault).map(Some)
}

fn absolute(p: PathBuf) -> Result<PathBuf> {
    std::path::absolute(&p).map_err(|e| Error::Invalid(format!("{}: {e}", p.display())))
}

fn no_passphrase() -> Error {
    Error::Invalid(format!(
        "a backup passphrase is needed: pass --passphrase-file FILE or set {PASSPHRASE_ENV}"
    ))
}

/// The passphrase from `file`, the environment, or a no-echo prompt (asked twice when
/// `confirm`). `None` when none of them is available.
fn passphrase(file: Option<&Path>, confirm: bool) -> Result<Option<Zeroizing<String>>> {
    if let Some(f) = file {
        let s = Zeroizing::new(
            std::fs::read_to_string(f)
                .map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?,
        );
        return Ok(Some(Zeroizing::new(strip_newline(&s).to_string())));
    }
    if let Ok(s) = std::env::var(PASSPHRASE_ENV)
        && !s.is_empty()
    {
        return Ok(Some(Zeroizing::new(s)));
    }
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    let first = prompt("Backup passphrase: ")?;
    let Some(first) = first else { return Ok(None) };
    if confirm {
        let again = prompt("Again: ")?.unwrap_or_default();
        if *again != *first {
            return Err(Error::Invalid("the passphrases do not match".into()));
        }
    }
    Ok(Some(first))
}

fn strip_newline(s: &str) -> &str {
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s)
}

/// Read a line from the terminal without echo (Unix, through `stty`). `None` where that is
/// not possible. (Interrupted with Ctrl-C, the terminal keeps echo off until `stty echo` or
/// `reset`.)
#[cfg(unix)]
fn prompt(label: &str) -> Result<Option<Zeroizing<String>>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let stty = |arg: &str| {
        Command::new("stty")
            .arg(arg)
            .stdin(Stdio::inherit())
            .status()
            .is_ok_and(|s| s.success())
    };
    eprint!("{label}");
    let _ = std::io::stderr().flush();
    if !stty("-echo") {
        eprintln!();
        return Ok(None);
    }
    let mut line = Zeroizing::new(String::new());
    let read = std::io::stdin().read_line(&mut line);
    stty("echo");
    eprintln!();
    read.map_err(Error::Io)?;
    Ok(Some(Zeroizing::new(strip_newline(&line).to_string())))
}

#[cfg(not(unix))]
fn prompt(_label: &str) -> Result<Option<Zeroizing<String>>> {
    Ok(None)
}
