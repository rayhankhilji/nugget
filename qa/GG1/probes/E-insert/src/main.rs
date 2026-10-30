//! GG1 area E probe (synthetic data only).
//!
//! Part A drives `nugget_a11y::Backend::insert` (the call `apps/desktop/.../doubletap/flow.rs`
//! `insert()` makes) over a scripted `Platform` whose fields behave like a well-behaved
//! accessibility API (`insert_text` replaces the selection). It shows what the product inserts,
//! where, and what read-back verification reports.
//!
//! Part B drives `nugget_draft::Drafter::read_ahead` with the product's default read-ahead config
//! (`min_interval_ms = 5000`), as the desktop's read-ahead loop would call it.
//!
//! Run: `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/user/gg1-target cargo run --release` here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_a11y::engine::{FieldRead, Target};
use nugget_a11y::tree::{Element, Node, States};
use nugget_a11y::verify::FieldState;
use nugget_a11y::role::Role;
use nugget_a11y::clipboard::{Clipboard, MemoryClipboard, ClipboardSnapshot};
use nugget_a11y::{A11yConfig, Accessibility, Backend, Permission, Platform, Rect};
use nugget_core::capability::{Locality, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{AppInfo, DraftContext, DraftRequest, FieldInfo, ThreadMessage};
use nugget_core::{Error, Result};
use nugget_draft::{DraftConfig, Drafter, NoVoice, Warmer};
use nugget_infer::{
    ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, Router, StopReason,
};
use nugget_retrieve::{RetrieveConfig, Retriever};
use nugget_store::Store;

// ------------------------------------------------------------------------------------------
// Part A: insertion over a scripted platform

#[derive(Clone)]
struct Field {
    role: Role,
    name: &'static str,
    multiline: bool,
    value: String,
    selection: Option<(u32, u32)>,
}

#[derive(Clone)]
struct El {
    node: Node,
}

impl Element for El {
    fn node(&self) -> Node {
        self.node.clone()
    }
    fn children(&self, _max: usize) -> Vec<Self> {
        Vec::new()
    }
    fn parent(&self) -> Option<Self> {
        None
    }
    fn same(&self, other: &Self) -> bool {
        self.node.name == other.node.name
    }
}

/// A window with several fields; `focus` is the index of the focused one.
struct Fake {
    app: AppInfo,
    fields: Mutex<Vec<Field>>,
    focus: Mutex<usize>,
}

impl Fake {
    fn idx(&self, el: &El) -> usize {
        let name = el.node.name.clone().unwrap_or_default();
        self.fields
            .lock()
            .unwrap()
            .iter()
            .position(|f| f.name == name)
            .unwrap()
    }
}

struct Cb(MemoryClipboard);
impl Clipboard for Cb {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        self.0.snapshot()
    }
    fn set_text(&mut self, t: &str) -> Result<()> {
        self.0.set_text(t)
    }
    fn restore(&mut self, s: &ClipboardSnapshot) -> Result<()> {
        self.0.restore(s)
    }
}

impl Platform for Fake {
    type Element = El;
    fn permission(&self) -> Permission {
        Permission::Granted
    }
    fn request_permission(&self) -> Result<()> {
        Ok(())
    }
    fn focus(&self) -> Result<Option<Target<El>>> {
        let f = self.fields.lock().unwrap()[*self.focus.lock().unwrap()].clone();
        let node = Node {
            role: f.role,
            name: Some(f.name.into()),
            value: Some(f.value.clone()),
            states: States {
                focused: true,
                editable: true,
                multiline: Some(f.multiline),
                ..Default::default()
            },
            ..Default::default()
        };
        Ok(Some(Target {
            app: self.app.clone(),
            element: El { node },
        }))
    }
    fn read_field(&self, el: &El, _max: usize) -> Result<FieldRead> {
        let f = self.fields.lock().unwrap()[self.idx(el)].clone();
        Ok(FieldRead {
            node: el.node.clone(),
            label: None,
            state: FieldState {
                value: f.value,
                selection: f.selection,
                truncated: false,
            },
        })
    }
    fn caret_bounds(&self, _el: &El, _s: &FieldState) -> Option<Rect> {
        None
    }
    fn insert_text(&self, el: &El, text: &str, before: &FieldState) -> Result<()> {
        // Like AT-SPI's InsertText: at the selection, or at the end when it is unknown.
        let i = self.idx(el);
        let mut fields = self.fields.lock().unwrap();
        let f = &mut fields[i];
        let n = before.value.chars().count() as u32;
        let (s, e) = before.selection.unwrap_or((n, n));
        let st = FieldState {
            value: before.value.clone(),
            selection: Some((s, e)),
            truncated: false,
        };
        f.value = nugget_a11y::verify::expected_after_insert(&st, text).unwrap();
        let c = s.min(e) + text.chars().count() as u32;
        f.selection = Some((c, c));
        Ok(())
    }
    fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
        Ok(Box::new(Cb(MemoryClipboard::default())))
    }
    fn paste(&self, _el: &El) -> Result<()> {
        Err(Error::Unsupported("no paste in the probe".into()))
    }
}

fn mail_app() -> AppInfo {
    AppInfo {
        id: Some("com.apple.mail".into()),
        name: "Mail".into(),
        window_title: Some("Re: order".into()),
        url: None,
    }
}

fn backend(fields: Vec<Field>, focus: usize) -> Backend<Fake> {
    Backend::new(
        Fake {
            app: mail_app(),
            fields: Mutex::new(fields),
            focus: Mutex::new(focus),
        },
        A11yConfig::default(),
    )
}

fn body(value: &str) -> Field {
    let n = value.chars().count() as u32;
    Field {
        role: Role::TextArea,
        name: "Message body",
        multiline: true,
        value: value.into(),
        selection: Some((n, n)),
    }
}

fn subject(value: &str) -> Field {
    let n = value.chars().count() as u32;
    Field {
        role: Role::TextField,
        name: "Subject",
        multiline: false,
        value: value.into(),
        selection: Some((n, n)),
    }
}

fn show(label: &str, b: &Backend<Fake>, r: Result<nugget_a11y::InsertOutcome>) {
    let fields = b.platform().fields.lock().unwrap();
    println!("[{label}] insert -> {:?}", r.map(|o| (o.method, o.verified)));
    for f in fields.iter() {
        println!("    {:<13} = {:?}", f.name, f.value);
    }
}

fn part_a() {
    println!("== Part A: nugget_a11y::Backend::insert (what flow.rs insert() calls)");
    // A1: half-typed text without a trailing space; the engine's draft never starts with one.
    let b = backend(vec![body("Hi Ana,\n\nThanks for the update,")], 0);
    let r = b.insert("I'll move our prep meeting to Monday.");
    show("A1 join, no separator", &b, r);

    // A2: draft read from the body; before Enter the user clicked into Subject (same app).
    // wait_frontmost() only checks the app, so insert() targets the newly focused field.
    let b = backend(vec![subject("Re: order"), body("Hi Ana,\n\n")], 1);
    let read = b.focused().unwrap().unwrap();
    println!(
        "[A2] read at trigger: role={} label={:?} value={:?}",
        read.field.role, read.field.label, read.field.value
    );
    *b.platform().focus.lock().unwrap() = 0;
    let r = b.insert("The total is $8,750 and delivery is on November 21.\n\nBest,\nMara");
    show("A2 focus moved to Subject", &b, r);

    // A3: the field changed between the read and Enter (user clicked back and typed).
    let b = backend(vec![body("Hi Ana,\n\n")], 0);
    {
        let mut f = b.platform().fields.lock().unwrap();
        f[0].value = "Hi Ana,\n\nThe tot".into();
        f[0].selection = Some((16, 16));
    }
    let r = b.insert("The total is $8,750.");
    show("A3 field changed while streaming", &b, r);

    // A4: no selection reported (some toolkits): the platform appends at the end (AT-SPI), the
    // prompt also put the caret at the end; the sign-off ends up above the body.
    let b = backend(vec![body("Hi Ana,\n\nBest,\nMara")], 0);
    b.platform().fields.lock().unwrap()[0].selection = None;
    let r = b.insert("Yes, delivery is still on November 21.");
    show("A4 selection unknown", &b, r);
}

// ------------------------------------------------------------------------------------------
// Part B: read-ahead with the product's default config

struct Local {
    id: ProviderId,
    chats: Mutex<usize>,
}

#[async_trait]
impl Provider for Local {
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: "probe".into(),
            locality: Locality::OnDevice,
            context_tokens: None,
        }
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        *self.chats.lock().unwrap() += 1;
        let ev: Vec<Result<ChatEvent>> = vec![
            Ok(ChatEvent::TextDelta("ok".into())),
            Ok(ChatEvent::Done {
                stop_reason: StopReason::EndTurn,
                input_tokens: None,
                output_tokens: None,
            }),
        ];
        Ok(Box::pin(futures::stream::iter(ev)))
    }
    async fn embed(&self, _t: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("none".into()))
    }
}

#[derive(Default)]
struct Rec {
    warms: Mutex<usize>,
    prefixes: Mutex<usize>,
}

#[async_trait]
impl Warmer for Rec {
    async fn warm(&self, _p: Arc<dyn Provider>) {
        *self.warms.lock().unwrap() += 1;
    }
    async fn warm_prefix(&self, _p: Arc<dyn Provider>, _r: ChatRequest) {
        *self.prefixes.lock().unwrap() += 1;
    }
}

fn drafter(rec: Arc<Rec>) -> Drafter {
    let store = Store::open_in_memory().unwrap();
    let p: Arc<dyn Provider> = Arc::new(Local {
        id: ProviderId::new("local"),
        chats: Mutex::new(0),
    });
    let router = Arc::new(Router::new(
        vec![p],
        HashMap::from([(Tier::Synthesis, vec![ProviderId::new("local")])]),
    ));
    let retriever = Retriever::new(store.clone(), None, RetrieveConfig::default());
    // Product defaults: read_ahead.enabled = true, min_interval_ms = 5000.
    Drafter::new(store, retriever, router, Arc::new(NoVoice), DraftConfig::default())
        .with_warmer(rec)
}

fn slack(thread: &str, value: &str) -> DraftRequest {
    let n = value.chars().count() as u32;
    DraftRequest {
        context: DraftContext {
            app: AppInfo {
                id: Some("com.tinyspeck.slackmacgap".into()),
                name: "Slack".into(),
                window_title: Some(format!("Slack | {thread}")),
                url: None,
            },
            field: FieldInfo {
                role: "text_area".into(),
                placeholder: Some("Message Sam".into()),
                value: value.into(),
                selection: Some((n, n)),
                ..Default::default()
            },
            thread: vec![ThreadMessage {
                author: Some("Sam Okafor".into()),
                text: thread.into(),
                from_me: Some(false),
                timestamp: None,
            }],
            ..Default::default()
        },
        instruction: None,
        provider: None,
        max_tokens: None,
    }
}

fn outcome(r: &std::result::Result<nugget_draft::ReadAheadOutcome, nugget_draft::ReadAheadSkip>) -> String {
    match r {
        Ok(o) => format!("done (prompt_chars={})", o.prompt_chars),
        Err(s) => format!("skipped: {}", s.as_str()),
    }
}

async fn part_b() {
    println!("\n== Part B: Drafter::read_ahead with DraftConfig::default()");
    let rec = Arc::new(Rec::default());
    let d = drafter(rec.clone());
    println!("default read_ahead config: {:?}", d.config().read_ahead);

    let a = slack("Can you send the deck before the review?", "");
    let b = slack("Different conversation: are we still on for lunch Thursday?", "");
    let r = d.read_ahead(a.clone()).await;
    println!("[B1] read_ahead(thread A)                         -> {}", outcome(&r));
    let r = d.read_ahead(b.clone()).await;
    println!("[B1] read_ahead(thread B) right after (switched)   -> {}", outcome(&r));
    let typed = slack("Can you send the deck before the review?", "Sounds good,");
    let r = d.read_ahead(typed).await;
    println!("[B1] read_ahead(thread A + typed text)             -> {}", outcome(&r));

    // The trigger in thread B: prewarm (app-only hint, as flow.rs) then begin.
    let (w0, p0) = (*rec.warms.lock().unwrap(), *rec.prefixes.lock().unwrap());
    let hint = DraftContext {
        app: b.context.app.clone(),
        ..Default::default()
    };
    let pw = d.prewarm(&hint);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (w1, p1) = (*rec.warms.lock().unwrap(), *rec.prefixes.lock().unwrap());
    println!(
        "[B2] trigger in thread B: prewarm={:?}; connection warms +{}, prefix warms +{} (read-ahead for thread A 'covers' the app)",
        pw.map(|p| p.provider.to_string()),
        w1 - w0,
        p1 - p0
    );

    let mut secure = slack("x", "");
    secure.context.field.secure = true;
    secure.context.field.role = "text_field".into();
    println!("[B3] secure field                                  -> {}", outcome(&d.read_ahead(secure).await));
    let mut search = slack("x", "");
    search.context.field.role = "search_field".into();
    println!("[B3] search field                                  -> {}", outcome(&d.read_ahead(search).await));
    println!("[B3] cancel_read_ahead() with nothing in flight    -> {}", d.cancel_read_ahead());
}

fn main() {
    part_a();
    tokio::runtime::Runtime::new().unwrap().block_on(part_b());
}
