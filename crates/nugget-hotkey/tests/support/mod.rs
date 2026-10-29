//! The Double Tap draft-and-insert path, shared by the real-OS integration tests
//! (`windows_it.rs`, `macos_it.rs`).
//!
//! It runs the same steps as the desktop shell's `doubletap::flow` and the daemon's `draft.begin`,
//! through the same library calls, with only the transport (Tauri events, JSON-RPC) left out:
//!
//! 1. the trigger fires (a real `nugget_hotkey` listener fed with synthesized key presses);
//! 2. the focused field is read with `nugget_a11y` (`Accessibility::focused`), refused when it is
//!    secure, and turned into a `DraftRequest` exactly as `flow::fire` does;
//! 3. `nugget_draft::Drafter::begin` (the daemon's engine) drafts with a scripted provider that
//!    streams fixed chunks with no delay, so the numbers measure Nugget's own overhead;
//! 4. the text is inserted with `Accessibility::insert` (verified read-back, clipboard fallback),
//!    and read back through `Accessibility::focused`.
//!
//! Two insert modes are measured:
//! - [`Mode::Shell`]: what the shell ships today. The draft streams into the overlay, and the
//!   whole text is inserted once on accept (`flow::insert`). "First token" is the first delta
//!   reaching the overlay; "visible" is the inserted text read back from the field.
//! - [`Mode::Streamed`]: every post-processed delta is inserted into the field as it arrives.
//!   "First token" is the first chunk read back from the field.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use nugget_a11y::{Accessibility, InsertMethod};
use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{AppInfo, DraftContext, DraftRequest};
use nugget_core::{Error, Result};
use nugget_draft::{DraftConfig, Drafter, NoVoice};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, Router, StopReason,
};
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_store::Store;

/// A provider that streams a fixed script, with no delay.
struct Scripted {
    id: ProviderId,
    chunks: Vec<String>,
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "scripted".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        let mut events: Vec<Result<ChatEvent>> = self
            .chunks
            .iter()
            .map(|c| Ok(ChatEvent::TextDelta(c.clone())))
            .collect();
        events.push(Ok(ChatEvent::Done {
            stop_reason: StopReason::EndTurn,
            input_tokens: Some(1),
            output_tokens: Some(self.chunks.len() as u32),
        }));
        Ok(futures::stream::iter(events).boxed())
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("no embeddings".into()))
    }
}

/// The daemon side: an in-memory store, retrieval and a drafter over the scripted provider.
pub struct Pipeline {
    pub rt: tokio::runtime::Runtime,
    drafter: Drafter,
}

impl Pipeline {
    pub fn new(chunks: &[&str]) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let id = ProviderId::new("scripted");
        let provider: Arc<dyn Provider> = Arc::new(Scripted {
            id: id.clone(),
            chunks: chunks.iter().map(|c| (*c).to_owned()).collect(),
        });
        let router = Arc::new(Router::new(
            vec![provider],
            HashMap::from([(Tier::Synthesis, vec![id])]),
        ));
        let store = Store::open_in_memory().expect("in-memory store");
        let retriever = Retriever::new(store.clone(), None, RetrieveConfig::default());
        let drafter = {
            let _guard = rt.enter();
            Drafter::new(
                store,
                retriever,
                router,
                Arc::new(NoVoice),
                DraftConfig::default(),
            )
        };
        Self { rt, drafter }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shell,
    Streamed,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Shell => "shell",
            Mode::Streamed => "streamed",
        }
    }
}

/// What one draft took, measured from the trigger firing.
#[derive(Debug, Clone)]
pub struct Report {
    pub mode: Mode,
    pub app: AppInfo,
    pub field_role: String,
    /// The focused-field read (`Accessibility::focused`).
    pub read_field: Duration,
    /// The `Accessibility::focused` call alone.
    pub focused_call: Duration,
    /// `draft.begin` returned (provider opened).
    pub draft_started: Duration,
    /// Shell: first delta in the overlay. Streamed: first chunk read back from the field.
    pub first_token: Duration,
    /// How long the first `Accessibility::insert` call took (it includes its own read-back).
    pub first_insert: Duration,
    /// The whole draft read back from the field.
    pub total: Duration,
    pub text: String,
    pub chunks: usize,
    pub methods: Vec<InsertMethod>,
    pub verified: bool,
}

impl Report {
    pub fn line(&self, os: &str) -> String {
        format!(
            "DOUBLETAP-TIMING os={os} mode={} app={:?} role={} read_field_ms={:.1} \
             focused_call_ms={:.1} \
             draft_started_ms={:.1} first_token_ms={:.1} first_insert_call_ms={:.1} \
             total_ms={:.1} chunks={} methods={:?} verified={}",
            self.mode.as_str(),
            self.app.name,
            self.field_role,
            ms(self.read_field),
            ms(self.focused_call),
            ms(self.draft_started),
            ms(self.first_token),
            ms(self.first_insert),
            ms(self.total),
            self.chunks,
            self.methods,
            self.verified,
        )
    }

    /// Print the timing line and append it to `$NUGGET_DOUBLETAP_SUMMARY` when set (the
    /// workflow points it at the job summary).
    pub fn publish(&self, os: &str) {
        let line = self.line(os);
        println!("{line}");
        if let Ok(path) = std::env::var("NUGGET_DOUBLETAP_SUMMARY") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    f,
                    "| {os} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {} | {:?} | {} |",
                    self.mode.as_str(),
                    self.app.name,
                    ms(self.read_field),
                    ms(self.draft_started),
                    ms(self.first_token),
                    ms(self.first_insert),
                    ms(self.total),
                    self.chunks,
                    self.methods,
                    self.verified
                );
            }
        }
    }
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

/// Line endings and trailing whitespace differ between editors (`\r` in rich edits).
pub fn norm(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Poll `cond` every 10 ms until it holds or `timeout` passes.
pub fn wait_for<T>(timeout: Duration, mut cond: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = cond() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The focused field's value, read back through the platform backend alone (the focused
/// element and its value, without `focused()`'s thread snapshot), so polling it costs little.
pub fn field_value<P: nugget_a11y::Platform>(backend: &nugget_a11y::Backend<P>) -> Option<String> {
    let p = backend.platform();
    let target = p.focus().ok().flatten()?;
    let read = p
        .read_field(&target.element, backend.config().max_value_bytes)
        .ok()?;
    Some(norm(&read.state.value))
}

/// Wait until `app_matches` holds for the frontmost app and its focus is in an editable field.
pub fn wait_for_field(
    a11y: &dyn Accessibility,
    timeout: Duration,
    app_matches: impl Fn(&AppInfo) -> bool,
) -> std::result::Result<nugget_a11y::Focused, String> {
    let mut last = String::from("nothing read yet");
    wait_for(timeout, || match a11y.focused() {
        Ok(Some(f)) if app_matches(&f.app) => Some(f),
        Ok(Some(f)) => {
            last = format!("focus is in {:?} ({:?})", f.app.name, f.app.id);
            None
        }
        Ok(None) => {
            last = "no focused text field".into();
            None
        }
        Err(e) => {
            last = format!("accessibility error: {e}");
            None
        }
    })
    .ok_or_else(|| format!("no editable field in the target app after {timeout:?}: {last}"))
}

/// Run the draft-and-insert path for a trigger that fired at `fired`, through `backend`, the
/// accessibility backend the shell uses (the read and the inserts go through its
/// `Accessibility` API; field values are polled with [`field_value`]).
pub fn draft_and_insert<P: nugget_a11y::Platform>(
    backend: &nugget_a11y::Backend<P>,
    pipeline: &Pipeline,
    fired: Instant,
    mode: Mode,
) -> std::result::Result<Report, String> {
    let a11y: &dyn Accessibility = backend;
    // flow::fire: read the focused field.
    let call = Instant::now();
    let focused = a11y
        .focused()
        .map_err(|e| format!("focused(): {e}"))?
        .ok_or("focused(): no focused text field")?;
    let read_field = fired.elapsed();
    let focused_call = call.elapsed();
    if focused.field.secure {
        return Err("the focused field is a secure field".into());
    }
    let before = norm(&focused.field.value);
    let app = focused.app.clone();
    let field_role = focused.field.role.clone();
    let request = DraftRequest {
        context: DraftContext {
            app: focused.app,
            field: focused.field,
            thread: focused.thread,
            recipients: focused.recipients,
            subject: focused.subject,
            screenshot_png_base64: None,
        },
        instruction: None,
        provider: None,
        max_tokens: None,
    };

    // The daemon's draft.begin.
    let (_started, mut stream) = pipeline
        .rt
        .block_on(pipeline.drafter.begin(request))
        .map_err(|e| format!("draft.begin: {e}"))?;
    let draft_started = fired.elapsed();

    let mut text = String::new();
    let mut first_token = None;
    let mut chunks = 0usize;
    let mut methods = Vec::new();
    let mut verified = true;
    let mut first_insert = None;
    let mut insert = |t: &str| -> std::result::Result<(), String> {
        let started = Instant::now();
        let out = a11y.insert(t).map_err(|e| format!("insert({t:?}): {e}"))?;
        first_insert.get_or_insert(started.elapsed());
        methods.push(out.method);
        verified &= out.verified;
        Ok(())
    };

    loop {
        let next = pipeline.rt.block_on(stream.next());
        let Some(event) = next else { break };
        match event.map_err(|e| format!("draft stream: {e}"))? {
            ChatEvent::TextDelta(d) if !d.is_empty() => {
                chunks += 1;
                text.push_str(&d);
                match mode {
                    Mode::Shell => {
                        first_token.get_or_insert_with(|| fired.elapsed());
                    }
                    Mode::Streamed => {
                        insert(&d)?;
                        if first_token.is_none() {
                            let want = format!("{before}{}", norm(&d));
                            wait_for(Duration::from_secs(5), || {
                                field_value(backend).filter(|v| v.starts_with(&want))
                            })
                            .ok_or_else(|| {
                                format!(
                                    "the first chunk {d:?} never showed up in the field (value \
                                     {:?})",
                                    field_value(backend)
                                )
                            })?;
                            first_token = Some(fired.elapsed());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if text.is_empty() {
        return Err("the draft stream produced no text".into());
    }
    if mode == Mode::Shell {
        // flow::insert: the whole draft, once, on accept.
        insert(&text)?;
    }
    let want = format!("{before}{}", norm(&text));
    wait_for(Duration::from_secs(5), || {
        field_value(backend).filter(|v| v.trim_end() == want.trim_end())
    })
    .ok_or_else(|| {
        format!(
            "the field does not hold the draft: want {want:?}, read back {:?}",
            field_value(backend)
        )
    })?;
    let total = fired.elapsed();
    Ok(Report {
        mode,
        app,
        field_role,
        read_field,
        focused_call,
        draft_started,
        first_token: first_token.unwrap_or(total),
        first_insert: first_insert.unwrap_or_default(),
        total,
        text,
        chunks,
        methods,
        verified,
    })
}

/// The scripted drafts: plain sentences, which post-processing passes through, short enough
/// for every length band. The streamed one has a first line longer than the post-processor's
/// held-back tail (100 bytes), so its text is released, and inserted, while it streams.
pub const SHELL_CHUNKS: &[&str] = &[
    "Thanks for the notes",
    ", I will send",
    " the revised plan",
    " by Friday.",
];
pub const STREAMED_CHUNKS: &[&str] = &[
    " The revised plan",
    " moves the launch",
    " to the second week",
    " of March, which gives",
    " the team enough time",
    " to finish testing.",
    "\n\nI will share",
    " the updated timeline",
    " on Monday morning.",
];

fn count_nodes(n: &nugget_a11y::tree::Node) -> usize {
    1 + n.children.iter().map(count_nodes).sum::<usize>()
}

/// Where the focused-field read spends its time, step by step through the backend the shell
/// uses (`Accessibility::focused` = focus + read_field + caret + the thread snapshot). Printed,
/// three times each, so a cold first call shows up.
pub fn read_breakdown<P: nugget_a11y::Platform>(backend: &nugget_a11y::Backend<P>, os: &str) {
    let cfg = backend.config().clone();
    for run in 0..3 {
        let t = Instant::now();
        let target = backend.platform().focus();
        let focus = t.elapsed();
        let (read, caret) = match &target {
            Ok(Some(target)) => {
                let t = Instant::now();
                let r = backend
                    .platform()
                    .read_field(&target.element, cfg.max_value_bytes);
                let read = t.elapsed();
                let t = Instant::now();
                if let Ok(r) = &r {
                    let _ = backend.platform().caret_bounds(&target.element, &r.state);
                }
                (read, t.elapsed())
            }
            _ => (Duration::ZERO, Duration::ZERO),
        };
        let t = Instant::now();
        let snap = backend.snapshot();
        let snapshot = t.elapsed();
        let (nodes, truncated) = match &snap {
            Ok(Some((_, s))) => (count_nodes(&s.root), s.truncated),
            _ => (0, false),
        };
        let t = Instant::now();
        let focused = backend.focused();
        let whole = t.elapsed();
        println!(
            "DOUBLETAP-A11Y os={os} run={run} focus_ms={:.1} read_field_ms={:.1} caret_ms={:.1} \
             snapshot_ms={:.1} snapshot_nodes={nodes} truncated={truncated} focused_ms={:.1} \
             ok={}",
            ms(focus),
            ms(read),
            ms(caret),
            ms(snapshot),
            ms(whole),
            matches!(focused, Ok(Some(_))),
        );
    }
}

/// Press the trigger with `press` (given the attempt number, at most 3 attempts, each given 2 s
/// to fire) and run [`draft_and_insert`] as soon as `fired` delivers the fire time, on its own
/// thread, as the shell's `on_trigger` does: the draft starts at the fire, not when `press`
/// returns.
pub fn trigger_and_draft<P: nugget_a11y::Platform>(
    backend: &nugget_a11y::Backend<P>,
    pipeline: &Pipeline,
    mode: Mode,
    fired: std::sync::mpsc::Receiver<Instant>,
    mut press: impl FnMut(u32),
) -> std::result::Result<Report, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let seen = AtomicBool::new(false);
    std::thread::scope(|s| {
        let seen = &seen;
        let draft = s.spawn(move || {
            let at = fired
                .recv_timeout(Duration::from_secs(8))
                .map_err(|_| "the trigger did not fire in 3 attempts".to_string())?;
            seen.store(true, Ordering::Release);
            draft_and_insert(backend, pipeline, at, mode)
        });
        for attempt in 1..=3 {
            press(attempt);
            if wait_for(Duration::from_secs(2), || {
                seen.load(Ordering::Acquire).then_some(())
            })
            .is_some()
            {
                println!("trigger fired on attempt {attempt} of 3");
                break;
            }
            println!("trigger missed (attempt {attempt} of 3)");
        }
        draft
            .join()
            .unwrap_or_else(|_| Err("the draft thread panicked".into()))
    })
}
