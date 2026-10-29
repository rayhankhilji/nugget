//! The [`Connector`] implementation shared by the `files` and `obsidian` kinds.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use notify_debouncer_full::notify::event::{AccessKind, AccessMode, ModifyKind};
use notify_debouncer_full::notify::{self, EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, NoCache, new_debouncer_opt};
use nugget_connector::{CancellationToken, Change, ChangeSink, Connector, ConnectorContext};
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::time::Instant;

use crate::config::{Config, IGNORE_FILES};
use crate::key::rel_key;
use crate::scan::{Emit, KnownMap, Outcome, Scope, Target, reconcile};

/// Checkpoint the sync cursor after this many files.
pub const CHECKPOINT_EVERY: u64 = 500;
/// While some root cannot be watched, reconcile this often instead.
const DEGRADED_RESCAN: Duration = Duration::from_secs(60);

/// The sync cursor. Every pass is a full reconcile against the store's known versions; the
/// cursor only records which root list the keys were made for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cursor {
    pub v: u32,
    /// [`Config::roots_hash`] of the last completed pass.
    #[serde(default)]
    pub roots: Option<String>,
    /// RFC 3339 time of the last completed pass.
    #[serde(default)]
    pub scanned_at: Option<String>,
    /// Files seen by the last completed pass (or so far, while `in_progress`).
    #[serde(default)]
    pub files: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_progress: bool,
}

/// A configured root as the watcher may report it (FSEvents uses canonical paths, e.g.
/// `/private/var/...` for `/var/...`).
#[derive(Debug, Clone)]
pub(crate) struct WatchRoot {
    pub configured: PathBuf,
    pub canonical: Option<PathBuf>,
}

pub(crate) struct FsConnector {
    cfg: Arc<Config>,
    ctx: ConnectorContext,
    /// Versions as of the last pass (sync or live), so live events need not query the store.
    state: Mutex<Option<KnownMap>>,
}

impl FsConnector {
    pub fn new(cfg: Config, ctx: ConnectorContext) -> Self {
        Self {
            cfg: Arc::new(cfg),
            ctx,
            state: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, Option<KnownMap>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn known_from_store(&self) -> Result<KnownMap> {
        Ok(self
            .ctx
            .known
            .versions()
            .await?
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect())
    }

    /// Replace the in-memory versions with what the store holds.
    async fn reload_known(&self) -> Result<()> {
        let known = self.known_from_store().await?;
        *self.state() = Some(known);
        Ok(())
    }

    /// Run a reconcile pass on a blocking thread and forward its changes to `sink`.
    async fn run(
        &self,
        scope: Scope,
        known: KnownMap,
        force: bool,
        cancel: &CancellationToken,
        sink: &dyn ChangeSink,
        checkpoint: Option<&(dyn Fn(u64) -> Value + Sync)>,
    ) -> Result<Outcome> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Emit>(256);
        let cfg = Arc::clone(&self.cfg);
        let token = cancel.clone();
        let every = if checkpoint.is_some() {
            CHECKPOINT_EVERY
        } else {
            0
        };
        let handle = tokio::task::spawn_blocking(move || {
            reconcile(&cfg, &scope, &known, force, &token, every, &mut |e| {
                tx.blocking_send(e).is_ok()
            })
        });

        let mut forwarded: Result<()> = Ok(());
        while let Some(msg) = rx.recv().await {
            let step = match msg {
                Emit::Change(change) => send_cancellable(sink, change, cancel).await,
                Emit::Progress(n) => match checkpoint {
                    Some(f) => sink.checkpoint(f(n)).await,
                    None => Ok(()),
                },
            };
            if let Err(e) = step {
                forwarded = Err(e);
                break;
            }
        }
        drop(rx); // unblocks the producer if we stopped early
        let joined = handle.await;
        forwarded?;
        let out = joined.map_err(|e| Error::Other(format!("scan task failed: {e}")))??;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(out)
    }

    /// Reconcile `scope` against the in-memory versions and update them.
    async fn apply(
        &self,
        scope: Scope,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let known = {
            let st = self.state();
            match st.as_ref() {
                Some(m) => scope.covered_known(m),
                None => KnownMap::new(),
            }
        };
        let out = self.run(scope, known, false, cancel, sink, None).await?;
        if !out.deleted.is_empty() || out.upserted > 0 {
            tracing::debug!(
                kind = self.cfg.kind,
                upserts = out.upserted,
                deletes = out.deleted.len(),
                "applied live changes"
            );
        }
        let mut st = self.state();
        let map = st.get_or_insert_with(KnownMap::new);
        for k in out.deleted {
            map.remove(&k);
        }
        for (k, v) in out.found {
            map.insert(k, Some(v));
        }
        Ok(())
    }

    fn watch_roots(
        &self,
        debouncer: &mut Debouncer<RecommendedWatcher, NoCache>,
        roots: &mut [WatchRoot],
        watched: &mut [bool],
    ) {
        for (i, r) in roots.iter_mut().enumerate() {
            if watched[i] {
                continue;
            }
            r.canonical = std::fs::canonicalize(&r.configured)
                .ok()
                .filter(|c| *c != r.configured);
            match debouncer.watch(&r.configured, RecursiveMode::Recursive) {
                Ok(()) => watched[i] = true,
                Err(e) => tracing::warn!(
                    root = %r.configured.display(),
                    error = %e,
                    "cannot watch folder; falling back to periodic rescans"
                ),
            }
        }
    }
}

async fn send_cancellable(
    sink: &dyn ChangeSink,
    change: Change,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        r = sink.send(change) => r,
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Map an absolute event path to (root index, `/`-separated relative path).
pub(crate) fn locate(roots: &[WatchRoot], path: &Path) -> Option<(usize, String)> {
    roots.iter().enumerate().find_map(|(i, r)| {
        std::iter::once(&r.configured)
            .chain(r.canonical.as_ref())
            .find_map(|base| path.strip_prefix(base).ok())
            .map(|rel| (i, rel_key(rel)))
    })
}

/// Turn a batch of debounced watcher output into the subtrees to reconcile (`None`: nothing).
///
/// Paths are re-examined rather than trusting event kinds, which differ per OS (FSEvents does
/// not say which side of a rename a path is): a vanished path deletes its key and everything
/// below it, an existing file is upserted when its version changed, and an existing directory
/// (created or moved in) is walked.
pub(crate) fn plan(
    cfg: &Config,
    roots: &[WatchRoot],
    batch: Vec<DebounceEventResult>,
    known: &KnownMap,
) -> Option<Scope> {
    let mut targets = Vec::new();
    let add = |path: &Path, filter_noise: bool, targets: &mut Vec<Target>| {
        let Some((root, mut rel)) = locate(roots, path) else {
            return;
        };
        let name = rel.rsplit('/').next().unwrap_or_default();
        if IGNORE_FILES.contains(&name) {
            // An ignore file changed: its whole directory may change membership.
            rel = rel
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_default();
        }
        let t = Target { root, rel };
        if filter_noise && cfg.is_noise(&t.rel) && t.covered(known).is_empty() {
            return;
        }
        targets.push(t);
    };

    for result in batch {
        match result {
            Err(errors) => {
                for e in errors {
                    tracing::warn!(error = %e, "file watcher error; reconciling");
                    if e.paths.is_empty() {
                        return Some(Scope::All);
                    }
                    for p in &e.paths {
                        add(p, false, &mut targets);
                    }
                }
            }
            Ok(events) => {
                for ev in events {
                    if ev.need_rescan() {
                        if ev.paths.is_empty() {
                            return Some(Scope::All);
                        }
                        for p in &ev.paths {
                            add(p, false, &mut targets);
                        }
                        continue;
                    }
                    let structural = match ev.kind {
                        EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
                        EventKind::Access(_) => continue,
                        EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Modify(ModifyKind::Name(_)) => true,
                        _ => false,
                    };
                    for p in &ev.paths {
                        // A directory's own metadata or "modified" notice (Windows reports one
                        // for every child change) says nothing its children's events don't.
                        if !structural && p.is_dir() {
                            continue;
                        }
                        add(p, true, &mut targets);
                    }
                }
            }
        }
    }
    (!targets.is_empty()).then(|| Scope::targets(targets))
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[async_trait]
impl Connector for FsConnector {
    fn kind(&self) -> &'static str {
        self.cfg.kind
    }

    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let prev: Option<Cursor> = cursor.and_then(|c| serde_json::from_value(c).ok());
        let hash = self.cfg.roots_hash();
        let known = self.known_from_store().await?;
        // Root indices changed meaning: versions recorded under the old roots cannot be trusted.
        // No cursor while items are known is a reconciling full pass (e.g. the settings were
        // updated and the cursor reset): the roots may have changed too, so the same applies.
        let force = match &prev {
            Some(c) => c.roots.as_deref() != Some(hash.as_str()),
            None => !known.is_empty(),
        };
        if force {
            tracing::info!(
                kind = self.cfg.kind,
                "root folders changed or cursor reset; full rescan"
            );
        }

        // Mid-pass cursors keep the previous root hash during a forced rescan, so a crash
        // before the end forces the rescan again.
        let mid_roots = if force {
            prev.as_ref().and_then(|c| c.roots.clone())
        } else {
            Some(hash.clone())
        };
        let mid_scanned_at = prev.as_ref().and_then(|c| c.scanned_at.clone());
        let checkpoint = move |n: u64| {
            serde_json::to_value(Cursor {
                v: 1,
                roots: mid_roots.clone(),
                scanned_at: mid_scanned_at.clone(),
                files: n,
                in_progress: true,
            })
            .unwrap_or(Value::Null)
        };

        let out = self
            .run(
                Scope::All,
                known.clone(),
                force,
                cancel,
                sink,
                Some(&checkpoint),
            )
            .await?;
        tracing::info!(
            kind = self.cfg.kind,
            files = out.found.len(),
            upserts = out.upserted,
            deletes = out.deleted.len(),
            "sync pass done"
        );

        let files = out.found.len() as u64;
        let mut map = known;
        for k in &out.deleted {
            map.remove(k);
        }
        for (k, v) in out.found {
            map.insert(k, Some(v));
        }
        *self.state() = Some(map);

        Ok(serde_json::to_value(Cursor {
            v: 1,
            roots: Some(hash),
            scanned_at: Some(now_rfc3339()),
            files,
            in_progress: false,
        })?)
    }

    fn supports_watch(&self) -> bool {
        true
    }

    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        self.reload_known().await?;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            self.cfg.debounce,
            // The debouncer thread wakes every tick even when nothing changes; the default tick
            // (a quarter of the timeout) costs several wakeups a second in an idle daemon. One
            // tick per timeout delivers an event at most one timeout later than the default.
            Some(self.cfg.debounce),
            move |r: DebounceEventResult| {
                let _ = tx.send(r);
            },
            NoCache,
            notify::Config::default().with_follow_symlinks(self.cfg.follow_symlinks),
        )
        .map_err(|e| Error::Other(format!("cannot start file watcher: {e}")))?;

        let mut roots: Vec<WatchRoot> = self
            .cfg
            .roots
            .iter()
            .map(|r| WatchRoot {
                configured: r.path.clone(),
                canonical: None,
            })
            .collect();
        let mut watched = vec![false; roots.len()];
        self.watch_roots(&mut debouncer, &mut roots, &mut watched);

        // Catch up with anything that changed between the last sync pass and now.
        self.apply(Scope::All, sink.as_ref(), &cancel).await?;

        let period = |watched: &[bool]| {
            if watched.iter().any(|w| !w) {
                Some(DEGRADED_RESCAN)
            } else {
                self.cfg.rescan_interval
            }
        };
        let mut next_full = period(&watched).map(|p| Instant::now() + p);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    let Some(first) = msg else {
                        return Err(Error::Other("file watcher stopped".into()));
                    };
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    let scope = {
                        let st = self.state();
                        let empty = KnownMap::new();
                        plan(&self.cfg, &roots, batch, st.as_ref().unwrap_or(&empty))
                    };
                    if let Some(scope) = scope {
                        match self.apply(scope, sink.as_ref(), &cancel).await {
                            Err(Error::Cancelled) => break,
                            r => r?,
                        }
                    }
                }
                _ = sleep_until_opt(next_full) => {
                    self.watch_roots(&mut debouncer, &mut roots, &mut watched);
                    // Live changes are recorded as sent, not as applied: start the full pass
                    // from what the store really holds, so a change the pipeline failed to
                    // apply is sent again.
                    self.reload_known().await?;
                    match self.apply(Scope::All, sink.as_ref(), &cancel).await {
                        Err(Error::Cancelled) => break,
                        r => r?,
                    }
                    next_full = period(&watched).map(|p| Instant::now() + p);
                }
            }
        }
        debouncer.stop_nonblocking();
        Err(Error::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_full::DebouncedEvent;
    use notify_debouncer_full::notify::event::{
        CreateKind, DataChange, Flag, MetadataKind, RemoveKind, RenameMode,
    };
    use notify_debouncer_full::notify::{ErrorKind, Event};
    use serde_json::json;

    fn t(root: usize, rel: &str) -> Target {
        Target {
            root,
            rel: rel.into(),
        }
    }

    struct Fx {
        _dir: tempfile::TempDir,
        root: PathBuf,
        cfg: Config,
        roots: Vec<WatchRoot>,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let cfg = Config::files(&json!({ "roots": [root] })).unwrap();
        let roots = vec![WatchRoot {
            configured: root.clone(),
            // As FSEvents would report it (e.g. `/private/var/...` for `/var/...`).
            canonical: Some(PathBuf::from("/canonical/r")),
        }];
        Fx {
            _dir: dir,
            root,
            cfg,
            roots,
        }
    }

    fn ev(kind: EventKind, paths: Vec<PathBuf>) -> DebounceEventResult {
        let mut e = Event::new(kind);
        for p in paths {
            e = e.add_path(p.clone());
        }
        Ok(vec![DebouncedEvent::new(e, std::time::Instant::now())])
    }

    fn plan_one(f: &Fx, r: DebounceEventResult, known: &KnownMap) -> Option<Scope> {
        plan(&f.cfg, &f.roots, vec![r], known)
    }

    /// A pipeline stand-in that applies changes to a version map (also the `KnownItems`), except
    /// that it fails to apply the next `fail_next` upserts (they are recorded, not stored).
    #[derive(Default)]
    struct FlakySink {
        sent: std::sync::Mutex<Vec<(String, Option<String>)>>,
        store:
            std::sync::Mutex<std::collections::HashMap<nugget_core::ids::ItemKey, Option<String>>>,
        fail_next: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ChangeSink for FlakySink {
        async fn send(&self, change: Change) -> Result<()> {
            use std::sync::atomic::Ordering::SeqCst;
            if let Change::Upsert(d) = &change {
                self.sent
                    .lock()
                    .unwrap()
                    .push((d.key.0.clone(), d.version.clone()));
                // `fetch_update` is `try_update` from Rust 1.99, which is newer than the MSRV.
                #[allow(deprecated)]
                let failed = self
                    .fail_next
                    .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                    .is_ok();
                if !failed {
                    self.store
                        .lock()
                        .unwrap()
                        .insert(d.key.clone(), d.version.clone());
                }
            } else if let Change::Delete(k) = &change {
                self.store.lock().unwrap().remove(k);
            }
            Ok(())
        }
        async fn checkpoint(&self, _: Value) -> Result<()> {
            Ok(())
        }
    }

    struct Known(Arc<FlakySink>);
    #[async_trait]
    impl nugget_connector::KnownItems for Known {
        async fn versions(
            &self,
        ) -> Result<std::collections::HashMap<nugget_core::ids::ItemKey, Option<String>>> {
            Ok(self.0.store.lock().unwrap().clone())
        }
    }

    struct NoSecrets;
    impl nugget_connector::SecretStore for NoSecrets {
        fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn periodic_rescans_retry_live_changes_the_pipeline_did_not_apply() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.md");
        std::fs::write(&file, "v1").unwrap();
        let mut cfg = Config::files(&json!({ "roots": [root], "debounce_ms": 50 })).unwrap();
        cfg.rescan_interval = Some(Duration::from_millis(300));
        let sink = Arc::new(FlakySink::default());
        let ctx = ConnectorContext {
            source: nugget_core::ids::SourceId::new("files:t"),
            paths: nugget_core::paths::Paths::rooted(dir.path().join("home")),
            egress: Arc::new(nugget_core::egress::NullEgress),
            secrets: Arc::new(NoSecrets),
            known: Arc::new(Known(sink.clone())),
        };
        let c = Arc::new(FsConnector::new(cfg, ctx));
        c.sync(None, sink.as_ref(), &CancellationToken::new())
            .await
            .unwrap();

        // Changed while not watching; the catch-up pass sends it, and the pipeline fails to
        // apply it (e.g. a store error).
        std::fs::write(&file, "version 2").unwrap();
        let v2 = crate::scan::version_of(&std::fs::metadata(&file).unwrap());
        sink.fail_next.store(1, std::sync::atomic::Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let task = {
            let (c, s, cancel) = (
                c.clone(),
                sink.clone() as Arc<dyn ChangeSink>,
                cancel.clone(),
            );
            tokio::spawn(async move { c.watch(s, cancel).await })
        };
        // The next periodic full pass compares against the store and sends it again.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let sends = sink
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, v)| k == "0:a.md" && v.as_deref() == Some(v2.as_str()))
                .count();
            if sends >= 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the failed change was never retried ({sends} sends)"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stored = sink.store.lock().unwrap()[&nugget_core::ids::ItemKey::new("0:a.md")].clone();
        assert_eq!(stored.as_deref(), Some(v2.as_str()));
        cancel.cancel();
        let _ = task.await;
    }

    #[test]
    fn renames_touch_both_sides() {
        let f = fx();
        let known = KnownMap::new();
        let from = f.root.join("a.md");
        let to = f.root.join("sub").join("b.md");
        // inotify / Windows: a paired rename.
        let s = plan_one(
            &f,
            ev(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![from.clone(), to.clone()],
            ),
            &known,
        );
        assert_eq!(
            s,
            Some(Scope::Targets(vec![t(0, "a.md"), t(0, "sub/b.md")]))
        );
        // FSEvents: two ambiguous "renamed" notices, one per side.
        let batch = vec![
            ev(
                EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
                vec![from],
            ),
            ev(
                EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
                vec![to],
            ),
        ];
        assert_eq!(
            plan(&f.cfg, &f.roots, batch, &known),
            Some(Scope::Targets(vec![t(0, "a.md"), t(0, "sub/b.md")]))
        );
    }

    #[test]
    fn canonical_paths_map_to_the_configured_root() {
        let f = fx();
        let s = plan_one(
            &f,
            ev(
                EventKind::Create(CreateKind::File),
                vec![PathBuf::from("/canonical/r/sub/x.md")],
            ),
            &KnownMap::new(),
        );
        assert_eq!(s, Some(Scope::Targets(vec![t(0, "sub/x.md")])));
        assert_eq!(
            plan_one(
                &f,
                ev(
                    EventKind::Create(CreateKind::File),
                    vec![PathBuf::from("/elsewhere/x.md")]
                ),
                &KnownMap::new()
            ),
            None,
            "paths outside every root are ignored"
        );
    }

    #[test]
    fn rescans_and_errors_reconcile() {
        let f = fx();
        let known = KnownMap::new();
        // inotify queue overflow / FSEvents dropped events: no path → everything.
        let overflow: DebounceEventResult = Ok(vec![DebouncedEvent::new(
            Event::new(EventKind::Other).set_flag(Flag::Rescan),
            std::time::Instant::now(),
        )]);
        assert_eq!(plan_one(&f, overflow, &known), Some(Scope::All));
        // FSEvents MustScanSubDirs on one directory: that subtree, even if it looks like noise.
        let sub: DebounceEventResult = Ok(vec![DebouncedEvent::new(
            Event::new(EventKind::Other)
                .set_flag(Flag::Rescan)
                .add_path(f.root.join(".hidden")),
            std::time::Instant::now(),
        )]);
        assert_eq!(
            plan_one(&f, sub, &known),
            Some(Scope::Targets(vec![t(0, ".hidden")]))
        );
        let err: DebounceEventResult =
            Err(vec![notify::Error::new(ErrorKind::Generic("boom".into()))]);
        assert_eq!(plan_one(&f, err, &known), Some(Scope::All));
        let err: DebounceEventResult = Err(vec![
            notify::Error::new(ErrorKind::Generic("gone".into())).add_path(f.root.join("sub")),
        ]);
        assert_eq!(
            plan_one(&f, err, &known),
            Some(Scope::Targets(vec![t(0, "sub")]))
        );
    }

    #[test]
    fn noise_is_dropped_unless_it_covers_known_items() {
        let f = fx();
        let mut known = KnownMap::new();
        let git = f.root.join(".git").join("index");
        let nm = f.root.join("node_modules").join("x.js");
        let swp = f.root.join(".a.md.swp");
        for p in [&git, &nm, &swp] {
            assert_eq!(
                plan_one(
                    &f,
                    ev(EventKind::Modify(ModifyKind::Any), vec![p.clone()]),
                    &known
                ),
                None,
                "{}",
                p.display()
            );
        }
        // Reads are never interesting; a close-after-write is.
        let a = f.root.join("a.md");
        assert_eq!(
            plan_one(
                &f,
                ev(EventKind::Access(AccessKind::Read), vec![a.clone()]),
                &known
            ),
            None
        );
        assert_eq!(
            plan_one(
                &f,
                ev(
                    EventKind::Access(AccessKind::Close(AccessMode::Write)),
                    vec![a]
                ),
                &known
            ),
            Some(Scope::Targets(vec![t(0, "a.md")]))
        );
        // A hidden path that holds indexed items (include_hidden was on before) still counts.
        known.insert("0:.notes/x.md".into(), None);
        assert_eq!(
            plan_one(
                &f,
                ev(
                    EventKind::Remove(RemoveKind::Folder),
                    vec![f.root.join(".notes")]
                ),
                &known
            ),
            Some(Scope::Targets(vec![t(0, ".notes")]))
        );
    }

    #[test]
    fn directory_modify_notices_are_ignored_but_structure_changes_are_not() {
        let f = fx();
        let known = KnownMap::new();
        let sub = f.root.join("sub");
        // Windows reports a "modified" parent for every child change; metadata too.
        for kind in [
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
        ] {
            assert_eq!(plan_one(&f, ev(kind, vec![sub.clone()]), &known), None);
        }
        assert_eq!(
            plan_one(
                &f,
                ev(EventKind::Create(CreateKind::Folder), vec![sub.clone()]),
                &known
            ),
            Some(Scope::Targets(vec![t(0, "sub")]))
        );
        assert_eq!(
            plan_one(
                &f,
                ev(
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                    vec![sub]
                ),
                &known
            ),
            Some(Scope::Targets(vec![t(0, "sub")]))
        );
    }

    #[test]
    fn ignore_file_changes_target_their_directory_and_batches_coalesce() {
        let f = fx();
        let known = KnownMap::new();
        let batch = vec![
            ev(
                EventKind::Modify(ModifyKind::Any),
                vec![f.root.join("sub").join(".nuggetignore")],
            ),
            ev(
                EventKind::Create(CreateKind::File),
                vec![f.root.join("sub").join("n.md")],
            ),
            ev(
                EventKind::Remove(RemoveKind::File),
                vec![f.root.join("z.md")],
            ),
        ];
        assert_eq!(
            plan(&f.cfg, &f.roots, batch, &known),
            Some(Scope::Targets(vec![t(0, "sub"), t(0, "z.md")]))
        );
        let root_ignore = vec![ev(
            EventKind::Modify(ModifyKind::Any),
            vec![f.root.join(".gitignore")],
        )];
        assert_eq!(
            plan(&f.cfg, &f.roots, root_ignore, &known),
            Some(Scope::Targets(vec![t(0, "")]))
        );
    }
}
