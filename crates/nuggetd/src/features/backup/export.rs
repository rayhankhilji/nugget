//! `export.run`: everything in open formats, unencrypted, in
//! `<folder>/nugget-export-YYYYMMDDTHHMMSSZ/` (written as `<name>.partial` and renamed once
//! complete, so a failed export never leaves a folder that looks finished).

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use nugget_core::{Error, Result};
use nugget_store::Store;
use serde::Serialize;
use time::OffsetDateTime;

use super::disk::{self, EXPORT_PREFIX, io_err};

/// Items read per page.
const PAGE: usize = 256;
/// Progress is reported every this many items.
const PROGRESS_EVERY: u64 = 500;

/// What `export.run` answers.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ExportResult {
    pub path: String,
    pub items: u64,
    pub entities: u64,
    pub claims: u64,
    pub audit_entries: u64,
    pub egress_rows: u64,
    pub vault_files: u64,
    /// Items exported with an `error` field (partly unreadable as stored; see `items.jsonl`).
    pub damaged_items: u64,
}

const README: &str = "# Nugget export

This folder holds everything Nugget stored, in open formats. It is **not encrypted**: keep it
somewhere safe and delete it when you no longer need it.

| File | Contents |
|---|---|
| `items.jsonl` | One JSON object per line per item: `id`, `source`, `key` (the item's id in its source), `title`, `mime`, `url`, `source_modified_at`, `created_at`, `updated_at`, `content_hash`, `version`, `metadata` (the source's metadata, as stored) and `text` (the full text; `chunks` instead when the item has no text of its own). An item that could not be read completely carries an `error` field saying what is missing. |
| `graph.json` | The knowledge graph: `{\"format\": 1, \"entities\": [...], \"claims\": [...], \"mentions\": [...]}`. Entities carry aliases and identifiers, claims their evidence. Merged and deleted entities are included. |
| `audit.csv` | The audit log, oldest first: `seq,at,actor,action,target,detail,prev_hash,hash`. |
| `egress.csv` | The egress ledger: every request that left this computer. |
| `vault/` | The Markdown vault, one page per entity (Nugget's internal `.nugget/` state is left out). |

CSV files are RFC 4180 (CRLF line ends, quoted cells), written verbatim: import them as text in
a spreadsheet if their contents are untrusted.
";

/// Export `store` (and the vault at `vault_root`) into a new directory in `folder`. `progress`
/// gets `(phase, done)`. Blocking.
pub(crate) fn run_export(
    store: &Store,
    folder: &Path,
    vault_root: Option<&Path>,
    progress: &mut dyn FnMut(&'static str, u64),
) -> Result<ExportResult> {
    if !folder.is_dir() {
        return Err(Error::NotFound(format!(
            "the export folder {} does not exist",
            folder.display()
        )));
    }
    let name = disk::fresh_name(folder, EXPORT_PREFIX, OffsetDateTime::now_utc())?;
    let partial = folder.join(format!("{name}.partial"));
    let done = folder.join(&name);
    crate::fsutil::create_private_dir(&partial).map_err(|e| io_err(&partial, e))?;
    let written = write_all(store, &partial, vault_root, progress);
    match written {
        Ok(mut r) => {
            fs::rename(&partial, &done).map_err(|e| io_err(&done, e))?;
            disk::sync_dir(folder);
            r.path = done.display().to_string();
            Ok(r)
        }
        Err(e) => {
            // Ours entirely (just created); the output is invalid after an error.
            let _ = fs::remove_dir_all(&partial);
            Err(e)
        }
    }
}

fn write_all(
    store: &Store,
    dir: &Path,
    vault_root: Option<&Path>,
    progress: &mut dyn FnMut(&'static str, u64),
) -> Result<ExportResult> {
    let mut r = ExportResult::default();
    {
        let path = dir.join("items.jsonl");
        let mut w = BufWriter::new(File::create(&path).map_err(|e| io_err(&path, e))?);
        progress("items", 0);
        let (mut n, mut damaged) = (0u64, 0u64);
        r.items = store.export_items(PAGE, &mut |item| {
            item.write_jsonl(&mut w)?;
            damaged += u64::from(item.error.is_some());
            n += 1;
            if n % PROGRESS_EVERY == 0 {
                progress("items", n);
            }
            Ok(())
        })?;
        finish(w, &path)?;
        r.damaged_items = damaged;
        progress("items", r.items);
    }
    {
        let path = dir.join("graph.json");
        let mut w = BufWriter::new(File::create(&path).map_err(|e| io_err(&path, e))?);
        progress("graph", 0);
        let c = store.export_graph_json(&mut w)?;
        finish(w, &path)?;
        r.entities = c.entities;
        r.claims = c.claims;
        progress("graph", c.entities + c.claims + c.mentions);
    }
    {
        let path = dir.join("audit.csv");
        let mut w = BufWriter::new(File::create(&path).map_err(|e| io_err(&path, e))?);
        progress("audit", 0);
        r.audit_entries = store.export_audit_csv(&mut w)?;
        finish(w, &path)?;
        progress("audit", r.audit_entries);
    }
    {
        let path = dir.join("egress.csv");
        let mut w = BufWriter::new(File::create(&path).map_err(|e| io_err(&path, e))?);
        progress("egress", 0);
        r.egress_rows = store.export_egress_csv(&mut w)?;
        finish(w, &path)?;
        progress("egress", r.egress_rows);
    }
    let vault = dir.join("vault");
    crate::fsutil::create_private_dir(&vault).map_err(|e| io_err(&vault, e))?;
    progress("vault", 0);
    if let Some(root) = vault_root.filter(|r| r.is_dir()) {
        let mut found = Vec::new();
        disk::walk_files(root, root, &mut found)?;
        for (rel, path) in found {
            if rel.starts_with(".nugget/") || rel.ends_with(".nugget-tmp") || !disk::safe_rel(&rel)
            {
                continue;
            }
            let to = rel.split('/').fold(vault.clone(), |p, s| p.join(s));
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            match fs::copy(&path, &to) {
                Ok(_) => r.vault_files += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(&path, e)),
            }
            if r.vault_files % PROGRESS_EVERY == 0 {
                progress("vault", r.vault_files);
            }
        }
    }
    progress("vault", r.vault_files);
    let readme = dir.join("README.md");
    fs::write(&readme, README).map_err(|e| io_err(&readme, e))?;
    Ok(r)
}

fn finish(w: BufWriter<File>, path: &Path) -> Result<()> {
    let f = w.into_inner().map_err(|e| io_err(path, e.into_error()))?;
    f.sync_all().map_err(|e| io_err(path, e))
}
