//! Watching the vault for user edits.

use std::path::PathBuf;
use std::sync::Arc;

use notify_debouncer_full::notify::{self, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, NoCache, new_debouncer_opt};
use nugget_core::{Error, Result};
use tokio_util::sync::CancellationToken;

use crate::Vault;
use crate::paths::{META_DIR, TMP_SUFFIX};

/// What a batch of watcher events asks for.
enum Work {
    Paths(Vec<PathBuf>),
    /// Events were lost: compare every page with its fingerprint.
    Rescan,
}

fn plan(batch: Vec<DebounceEventResult>) -> Option<Work> {
    let mut paths = Vec::new();
    for r in batch {
        match r {
            Err(_) => return Some(Work::Rescan),
            Ok(events) => {
                for ev in events {
                    if ev.need_rescan() {
                        return Some(Work::Rescan);
                    }
                    if matches!(ev.kind, EventKind::Access(_)) {
                        continue;
                    }
                    for p in &ev.paths {
                        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        let ours = p.components().any(|c| c.as_os_str() == META_DIR)
                            || name.ends_with(TMP_SUFFIX);
                        if !ours && name.ends_with(".md") && !paths.contains(p) {
                            paths.push(p.clone());
                        }
                    }
                }
            }
        }
    }
    (!paths.is_empty()).then_some(Work::Paths(paths))
}

impl Vault {
    /// Watch the vault for user edits until `cancel` fires, applying each edited page to the graph
    /// (and re-rendering it) after the debounce interval. Our own writes are recognised by their
    /// fingerprint and ignored. Pages edited while nobody was watching are applied at start.
    pub async fn watch(self: &Arc<Self>, cancel: CancellationToken) -> Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            self.config.debounce,
            None,
            move |r: DebounceEventResult| {
                let _ = tx.send(r);
            },
            NoCache,
            notify::Config::default().with_follow_symlinks(false),
        )
        .map_err(|e| Error::Other(format!("cannot start vault watcher: {e}")))?;
        debouncer
            .watch(self.root(), RecursiveMode::Recursive)
            .map_err(|e| Error::Other(format!("cannot watch vault: {e}")))?;

        // Registered first, so nothing written from here on is missed.
        self.blocking(|v| v.apply_pending_edits()).await;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    let Some(first) = msg else {
                        return Err(Error::Other("vault watcher stopped".into()));
                    };
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    match plan(batch) {
                        Some(Work::Paths(paths)) => {
                            self.blocking(move |v| v.on_paths_changed(&paths)).await;
                        }
                        Some(Work::Rescan) => {
                            self.blocking(|v| v.apply_pending_edits()).await;
                        }
                        None => {}
                    }
                }
            }
        }
        debouncer.stop_nonblocking();
        Ok(())
    }

    async fn blocking<F>(self: &Arc<Self>, f: F)
    where
        F: FnOnce(&Vault) -> Result<crate::EditReport> + Send + 'static,
    {
        let v = Arc::clone(self);
        match tokio::task::spawn_blocking(move || f(&v)).await {
            Ok(Ok(report)) if !report.is_empty() => {
                self.edits.send_modify(|n| *n = n.wrapping_add(1));
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "vault: applying user edits failed"),
            Err(e) => tracing::warn!(error = %e, "vault: edit task panicked"),
        }
    }
}
