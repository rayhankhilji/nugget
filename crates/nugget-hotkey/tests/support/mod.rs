//! The Double Tap draft-and-insert path, shared by the real-OS integration tests.
//!
//! It runs the same steps as the desktop shell's `doubletap::flow` and the daemon's `draft.begin`,
//! through the same library calls, with only the transport (Tauri events, JSON-RPC) left out:
//!
//! 1. the trigger fires (a real `nugget_hotkey` listener fed with synthesized key presses);
//! 2. the focused field is read with `nugget_a11y` (`Accessibility::focused`), refused when it is
//!    secure, and turned into a `DraftRequest` exactly as `flow::fire` does;
//! 3. `nugget_draft::Drafter::begin` (the daemon's engine) drafts with a scripted provider that
//!    streams fixed chunks with no delay;
//! 4. the whole draft is inserted once, as on accept (`flow::insert`), with
//!    `Accessibility::insert` (verified read-back, clipboard fallback), and read back.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, mpsc};
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

/// What one draft took, measured from the trigger firing.
#[derive(Debug, Clone)]
pub struct Report {
    pub app: AppInfo,
    /// The focused-field read (`Accessibility::focused`).
    pub read_field: Duration,
    /// `draft.begin` returned (provider opened).
    pub draft_started: Duration,
    /// The first delta reaching the overlay.
    pub first_token: Duration,
    /// How long the `Accessibility::insert` call took (it includes its own read-back).
    pub insert_call: Duration,
    /// The whole draft read back from the field.
    pub total: Duration,
    pub chunks: usize,
    pub method: InsertMethod,
    pub verified: bool,
}

impl Report {
    /// Print the timing line and append a table row to `$NUGGET_DOUBLETAP_SUMMARY` when set (the
    /// workflow points it at the job summary).
    pub fn publish(&self, os: &str) {
        let (app, chunks, method, ok) = (&self.app.name, self.chunks, self.method, self.verified);
        let [read, started, first, insert, total] = [
            self.read_field,
            self.draft_started,
            self.first_token,
            self.insert_call,
            self.total,
        ]
        .map(ms);
        println!(
            "DOUBLETAP-TIMING os={os} app={app:?} read_field_ms={read:.1} \
             draft_started_ms={started:.1} first_token_ms={first:.1} insert_call_ms={insert:.1} \
             total_ms={total:.1} chunks={chunks} method={method:?} verified={ok}"
        );
        if let Ok(path) = std::env::var("NUGGET_DOUBLETAP_SUMMARY") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    f,
                    "| {os} | shell | {app} | {read:.1} | {started:.1} | {first:.1} | {insert:.1} \
                     | {total:.1} | {chunks} | {:?} | {ok} |",
                    [method]
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
/// accessibility backend the shell uses (field values are polled with [`field_value`]).
pub fn draft_and_insert<P: nugget_a11y::Platform>(
    backend: &nugget_a11y::Backend<P>,
    pipeline: &Pipeline,
    fired: Instant,
) -> std::result::Result<Report, String> {
    let a11y: &dyn Accessibility = backend;
    // flow::fire: read the focused field.
    let focused = a11y
        .focused()
        .map_err(|e| format!("focused(): {e}"))?
        .ok_or("focused(): no focused text field")?;
    let read_field = fired.elapsed();
    if focused.field.secure {
        return Err("the focused field is a secure field".into());
    }
    let before = norm(&focused.field.value);
    let app = focused.app.clone();
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
    while let Some(event) = pipeline.rt.block_on(stream.next()) {
        if let ChatEvent::TextDelta(d) = event.map_err(|e| format!("draft stream: {e}"))?
            && !d.is_empty()
        {
            chunks += 1;
            text.push_str(&d);
            first_token.get_or_insert_with(|| fired.elapsed());
        }
    }
    let Some(first_token) = first_token else {
        return Err("the draft stream produced no text".into());
    };
    // flow::insert: the whole draft, once, on accept.
    let started = Instant::now();
    let out = a11y
        .insert(&text)
        .map_err(|e| format!("insert({text:?}): {e}"))?;
    let insert_call = started.elapsed();
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
    Ok(Report {
        app,
        read_field,
        draft_started,
        first_token,
        insert_call,
        total: fired.elapsed(),
        chunks,
        method: out.method,
        verified: out.verified,
    })
}

/// The scripted draft: plain sentences, which post-processing passes through.
pub const CHUNKS: &[&str] = &[
    "Thanks for the notes",
    ", I will send",
    " the revised plan",
    " by Friday.",
];

/// A callback that sends the fire time, and its receiver.
pub fn channel_callback() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<Instant>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let cb = move || {
        let _ = tx.lock().unwrap().send(Instant::now());
    };
    (Box::new(cb), rx)
}

/// How many fires arrive until 300 ms pass without one.
pub fn fired(rx: &mpsc::Receiver<Instant>) -> usize {
    std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(300)).ok()).count()
}

/// Press the trigger with `press` (given the attempt number, at most 3 attempts, each given 2 s
/// to fire) and run [`draft_and_insert`] as soon as `fired` delivers the fire time, on its own
/// thread, as the shell's `on_trigger` does: the draft starts at the fire, not when `press`
/// returns.
pub fn trigger_and_draft<P: nugget_a11y::Platform>(
    backend: &nugget_a11y::Backend<P>,
    pipeline: &Pipeline,
    fired: mpsc::Receiver<Instant>,
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
            draft_and_insert(backend, pipeline, at)
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
