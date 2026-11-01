//! The caret overlay as a pure state machine: events in, an [`OverlayView`] and side-effect
//! [`Effect`]s out. Nothing here touches Tauri, the daemon or the OS, so every transition is
//! unit-tested; `flow.rs` feeds it events and carries out the effects.
//!
//! ```text
//! Begin ─▶ reading ─Started─▶ streaming ─Done─▶ done
//!             │                  │  │             │
//!          Refused/Error       Accept(text?) ◀────┘      Cancel (any open phase) ─▶ closed
//!             ▼                  ▼
//!         refused/error ◀─── inserting ─Inserted─▶ inserted ─Expire─▶ closed
//! ```
//!
//! While a draft is on screen (streaming or done, or after a draft error), `Retry` and
//! `Refine` go back to reading and ask the flow for a new draft of the same context
//! ([`Effect::Redraft`]); `InsertPrevious` inserts a draft from the history instead. Closing an
//! overlay that had the keyboard hands focus back to the field ([`Effect::RestoreFocus`]).
//! Practice sessions (the Double Tap page's practice field) send no feedback and keep no history.

use nugget_a11y::InsertMethod;
use nugget_core::capability::Locality;
use nugget_core::ipc::DraftFeedback;
use serde::{Deserialize, Serialize};

/// How long a refusal stays on screen.
pub const REFUSED_MS: u64 = 1_800;
/// How long an error stays on screen.
pub const ERROR_MS: u64 = 4_000;
/// How long after a verified insertion the session is closed.
pub const INSERTED_MS: u64 = 1_200;
/// Deltas buffered while `draft.begin` has not answered yet (they can arrive first).
const MAX_BUFFERED: usize = 1_024;
const MAX_BUFFERED_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Reading,
    Refused,
    Streaming,
    Done,
    Inserting,
    Inserted,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Accessibility,
    Clipboard,
    Typing,
}

impl From<InsertMethod> for Method {
    fn from(m: InsertMethod) -> Self {
        match m {
            InsertMethod::Accessibility => Method::Accessibility,
            InsertMethod::Clipboard => Method::Clipboard,
            InsertMethod::Typing => Method::Typing,
        }
    }
}

/// A quick refinement: a new draft of the text on screen, with an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refinement {
    Shorter,
    Longer,
    Formal,
    Casual,
}

/// The i18n key shown with a refinement that missed (unchanged, or not shorter or longer) after
/// its stricter second try.
pub const REFINE_MISSED: &str = "doubletap.overlay.refineMissed";

/// Length in words, counting each CJK, kana or Hangul character as a word (those scripts don't
/// separate words with spaces).
pub fn length_units(text: &str) -> usize {
    let dense = |c: char| matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF);
    let cjk = text.chars().filter(|c| dense(*c)).count();
    let words = text
        .split(|c: char| c.is_whitespace() || dense(c))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count();
    words + cjk
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Refinement {
    /// The length the rewrite aims at, in [`length_units`].
    pub fn target_units(self, base: usize) -> usize {
        match self {
            Refinement::Shorter => (base * 6 / 10).clamp(1, base.saturating_sub(1).max(1)),
            Refinement::Longer => base * 16 / 10 + 3,
            Refinement::Formal | Refinement::Casual => base.max(1),
        }
    }

    /// The instruction sent with the refined request (the user's own hint to the model): an
    /// explicit rewrite with a target length taken from the draft. `strict` is the second try,
    /// after a rewrite that missed. Under 300 characters (the local prompt's instruction cap).
    pub fn instruction(self, base_units: usize, strict: bool) -> String {
        let target = self.target_units(base_units);
        let task = match self {
            Refinement::Shorter => format!(
                "Rewrite the selected draft to be clearly shorter: at most {target} words (it has {base_units}). Keep only the main point."
            ),
            Refinement::Longer => format!(
                "Rewrite the selected draft to be longer: about {target} words (it has {base_units}). Add detail only from the conversation; invent nothing."
            ),
            Refinement::Formal => format!(
                "Rewrite the selected draft in a clearly more formal tone: polite, full sentences, no slang. About {target} words."
            ),
            Refinement::Casual => format!(
                "Rewrite the selected draft in a clearly more casual, friendly tone: contractions, short sentences. About {target} words."
            ),
        };
        let strict = if strict {
            " The last rewrite missed this; change it now."
        } else {
            ""
        };
        format!("{task} Same meaning and language. Never return it unchanged.{strict}")
    }

    /// A token cap that fits the target length with room to spare (tokens per word vary by
    /// language), so a "shorter" rewrite cannot run on.
    pub fn max_tokens(self, base_units: usize) -> u32 {
        u32::try_from(self.target_units(base_units) * 3 + 32).unwrap_or(u32::MAX)
    }

    /// Whether `out` is the rewrite asked for: changed, and shorter or longer when that was the
    /// point. (Tone can't be checked by rule; changed is the floor.)
    pub fn accepts(self, base: &str, out: &str) -> bool {
        if out.trim().is_empty() || normalized(out) == normalized(base) {
            return false;
        }
        let (b, o) = (length_units(base), length_units(out));
        match self {
            Refinement::Shorter => o < b,
            Refinement::Longer => o > b,
            Refinement::Formal | Refinement::Casual => true,
        }
    }
}

/// One refinement request: rewrite `base` with `kind`; `strict` on the automatic second try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefineRequest {
    pub kind: Refinement,
    pub base: String,
    pub strict: bool,
}

/// What the overlay window renders (event `doubletap:overlay`, command `overlay_state`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverlayView {
    pub phase: Phase,
    pub text: String,
    pub provider: Option<String>,
    pub locality: Option<Locality>,
    pub citations: u32,
    pub excluded_sensitive: u32,
    /// An i18n key (`doubletap.refused.secure`, ...) or raw error text.
    pub message: Option<String>,
    pub verified: Option<bool>,
    pub method: Option<Method>,
    /// Retry and the refinements can run now.
    pub can_retry: bool,
    /// The refinement this draft was asked for.
    pub refinement: Option<Refinement>,
    /// The session drafts for the practice field.
    pub practice: bool,
}

impl Default for OverlayView {
    fn default() -> Self {
        Self {
            phase: Phase::Reading,
            text: String::new(),
            provider: None,
            locality: None,
            citations: 0,
            excluded_sensitive: 0,
            message: None,
            verified: None,
            method: None,
            can_retry: false,
            refinement: None,
            practice: false,
        }
    }
}

/// The daemon accepted `draft.begin`.
#[derive(Debug, Clone, PartialEq)]
pub struct Started {
    pub stream_id: String,
    pub provider: String,
    pub locality: Locality,
    pub citations: u32,
    pub excluded_sensitive: u32,
    /// The draft may draw on items marked sensitive: it is never kept in the history.
    pub sensitive: bool,
    /// The [`Machine::generation`] the request was made for: a result for an older request (a
    /// retry cancelled before the daemon answered) is cancelled, never shown.
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The trigger fired. Ignored while a draft is in progress.
    Begin,
    /// Right after `Begin`: this session drafts for the practice field.
    Practice,
    Started(Started),
    Delta {
        stream_id: String,
        text: String,
    },
    Done {
        stream_id: String,
        error: Option<String>,
    },
    /// Enter: `None` accepts the text as shown, `Some` the user's edit.
    Accept(Option<String>),
    /// Esc.
    Cancel,
    /// A new draft of the same context.
    Retry,
    /// A new draft of `base` (the user's edit; `None` for the text shown) with the refinement.
    Refine {
        kind: Refinement,
        base: Option<String>,
    },
    /// Insert a draft from the history instead of the one shown.
    InsertPrevious(String),
    /// The overlay lost the keyboard to another window (the user clicked or switched away):
    /// close it, without taking the keyboard back.
    FocusLost,
    /// The trigger fired while busy: a draft on screen comes back with the keyboard.
    Refocus,
    Inserted {
        verified: bool,
        method: InsertMethod,
    },
    /// Anything failed; the message is an i18n key or raw text.
    Error(String),
    /// Refused locally or by the daemon; the message is an i18n key.
    Refused(String),
    /// The daemon connection dropped: no `chat.done` will come for a running stream.
    Disconnected,
    /// A [`Effect::HideAfter`] timer ran out.
    Expire {
        generation: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Place the overlay near the caret and show it; `focus` gives it the keyboard (needed for
    /// Enter/Tab/Esc), otherwise it is shown without taking focus where the platform allows.
    Show {
        focus: bool,
    },
    Hide,
    /// Feed [`Event::Expire`] back after `ms`.
    HideAfter {
        generation: u64,
        ms: u64,
    },
    /// `draft.cancel`.
    CancelStream(String),
    /// Hand focus back to the target app and insert this text.
    Insert(String),
    /// `draft.feedback`.
    Feedback(DraftFeedback),
    /// Ask the daemon for a new draft of the session's context: as before (Retry), or as a
    /// rewrite of this text with the refinement's instruction. Its results carry `generation`.
    Redraft {
        generation: u64,
        refine: Option<RefineRequest>,
    },
    /// Give the keyboard back to the field the draft was read from (after Esc or a timeout).
    RestoreFocus,
    /// Keep this draft in the history (never for practice or sensitive drafts).
    Remember(String),
}

/// The result of one transition.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    /// The view changed and should be emitted.
    pub changed: bool,
    pub effects: Vec<Effect>,
}

impl Step {
    fn changed(effects: Vec<Effect>) -> Self {
        Self {
            changed: true,
            effects,
        }
    }
    fn effects(effects: Vec<Effect>) -> Self {
        Self {
            changed: false,
            effects,
        }
    }
    fn merge(&mut self, other: Step) {
        self.changed |= other.changed;
        self.effects.extend(other.effects);
    }
}

#[derive(Debug, Default)]
pub struct Machine {
    view: OverlayView,
    /// A session is open (from `Begin` until it is cancelled or expires).
    active: bool,
    generation: u64,
    stream_id: Option<String>,
    /// The daemon stream is still producing.
    streaming: bool,
    /// Deltas and dones that arrived before `Started` (any stream; filtered on `Started`).
    buffered: Vec<Event>,
    buffered_bytes: usize,
    /// Outcome and text of the accepted draft while inserting (`None` for a previous draft).
    accepted: Option<(String, String)>,
    /// The session drafts for the practice field: no feedback, no history.
    practice: bool,
    /// A draft was started in this session, so its request can be repeated.
    requested: bool,
    /// A draft of this session may draw on sensitive items.
    sensitive: bool,
    /// The overlay was shown with the keyboard and not hidden since.
    focused: bool,
    /// The error on screen came from inserting (the target already has focus): no retry.
    insert_failed: bool,
    /// The refinement being drafted, checked when its draft is done.
    refining: Option<RefineRequest>,
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn view(&self) -> &OverlayView {
        &self.view
    }

    /// Bumped by every `Begin`, `Retry` and `Refine`: the token of the current request.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The session (the last one, once closed) drafts for the practice field.
    pub fn practice(&self) -> bool {
        self.practice
    }

    /// A draft of this session may draw on items marked sensitive.
    pub fn sensitive(&self) -> bool {
        self.sensitive
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.active
    }

    /// A draft is in progress, or the overlay holds the keyboard (a retryable error waiting for
    /// the user): a new trigger is ignored.
    pub fn busy(&self) -> bool {
        self.active
            && (self.focused
                || matches!(
                    self.view.phase,
                    Phase::Reading | Phase::Streaming | Phase::Done | Phase::Inserting
                ))
    }

    fn hide_after(&self, ms: u64) -> Effect {
        Effect::HideAfter {
            generation: self.generation,
            ms,
        }
    }

    fn feedback(
        &self,
        outcome: &str,
        final_text: Option<String>,
        verified: Option<bool>,
    ) -> Option<Effect> {
        if self.practice {
            return None;
        }
        self.stream_id.as_ref().map(|id| {
            Effect::Feedback(DraftFeedback {
                stream_id: id.clone(),
                outcome: outcome.into(),
                final_text,
                insert_verified: verified,
            })
        })
    }

    fn show(&mut self, focus: bool) -> Effect {
        self.focused |= focus;
        Effect::Show { focus }
    }

    /// Hide the overlay; hand the keyboard back to the field if the overlay had it.
    fn hide(&mut self, effects: &mut Vec<Effect>) {
        effects.push(Effect::Hide);
        if std::mem::take(&mut self.focused) {
            effects.push(Effect::RestoreFocus);
        }
    }

    fn can_retry(&self) -> bool {
        self.active
            && self.requested
            && match self.view.phase {
                Phase::Streaming | Phase::Done => true,
                Phase::Error => !self.insert_failed,
                _ => false,
            }
    }

    fn sync_retry(&mut self) {
        self.view.can_retry = self.can_retry();
    }

    /// Show an error: with the keyboard and without a timeout when it can be retried (the user
    /// decides), else briefly without focus.
    fn error_effects(&mut self) -> Vec<Effect> {
        self.sync_retry();
        if self.view.can_retry && self.focused {
            vec![self.show(true)]
        } else {
            vec![self.show(false), self.hide_after(ERROR_MS)]
        }
    }

    /// Remember the complete draft on screen before it is replaced or dismissed.
    fn remember_shown(&self, effects: &mut Vec<Effect>) {
        if self.view.phase == Phase::Done {
            effects.extend(self.remember(&self.view.text));
        }
    }

    fn remember(&self, text: &str) -> Option<Effect> {
        (!self.practice && !self.sensitive && !text.trim().is_empty())
            .then(|| Effect::Remember(text.to_owned()))
    }

    pub fn apply(&mut self, event: Event) -> Step {
        let mut step = self.apply_inner(event);
        let before = self.view.can_retry;
        self.sync_retry();
        step.changed |= before != self.view.can_retry;
        step
    }

    /// Retry and Refine: back to reading, a new request for the same context.
    fn redraft(&mut self, refine: Option<RefineRequest>) -> Step {
        let mut effects = Vec::new();
        if matches!(self.view.phase, Phase::Streaming | Phase::Done) {
            self.stop_stream(&mut effects);
            // A retry rejects the draft; a refinement keeps its substance, so it says nothing
            // about the user's voice either way (the refined draft's own outcome is reported).
            if refine.is_none() {
                effects.extend(self.feedback("rejected", None, None));
            }
            self.remember_shown(&mut effects);
        }
        self.new_request();
        self.view = OverlayView {
            refinement: refine.as_ref().map(|r| r.kind),
            practice: self.practice,
            ..OverlayView::default()
        };
        self.refining = refine.clone();
        effects.push(Effect::Redraft {
            generation: self.generation,
            refine,
        });
        Step::changed(effects)
    }

    fn apply_inner(&mut self, event: Event) -> Step {
        match event {
            Event::Begin => {
                if self.busy() {
                    return Step::default();
                }
                self.new_request();
                self.view = OverlayView::default();
                self.active = true;
                self.practice = false;
                self.requested = false;
                self.sensitive = false;
                self.refining = None;
                // A refusal or error still on screen was shown without the keyboard (a focused
                // one makes the machine busy).
                self.focused = false;
                Step::changed(Vec::new())
            }
            Event::Practice => {
                if !self.active || self.view.phase != Phase::Reading || self.requested {
                    return Step::default();
                }
                self.practice = true;
                self.view.practice = true;
                Step::changed(Vec::new())
            }
            Event::Started(s) => {
                if !self.active
                    || self.view.phase != Phase::Reading
                    || self.stream_id.is_some()
                    || s.generation != self.generation
                {
                    // Cancelled (or refused, or replaced by a newer request) while in flight.
                    return Step::effects(vec![Effect::CancelStream(s.stream_id)]);
                }
                self.view.phase = Phase::Streaming;
                self.view.provider = Some(s.provider);
                self.view.locality = Some(s.locality);
                self.view.citations = s.citations;
                self.view.excluded_sensitive = s.excluded_sensitive;
                self.stream_id = Some(s.stream_id.clone());
                self.streaming = true;
                self.requested = true;
                self.sensitive |= s.sensitive;
                let show = self.show(true);
                let mut step = Step::changed(vec![show]);
                self.buffered_bytes = 0;
                for ev in std::mem::take(&mut self.buffered) {
                    let ours = matches!(&ev, Event::Delta { stream_id, .. } | Event::Done { stream_id, .. } if *stream_id == s.stream_id);
                    if ours {
                        step.merge(self.apply(ev));
                    }
                }
                step
            }
            Event::Delta { stream_id, text } => {
                if self.awaiting_start() {
                    self.buffer(Event::Delta { stream_id, text });
                    return Step::default();
                }
                if self.view.phase == Phase::Streaming
                    && self.stream_id.as_deref() == Some(stream_id.as_str())
                {
                    self.view.text.push_str(&text);
                    return Step::changed(Vec::new());
                }
                Step::default()
            }
            Event::Done { stream_id, error } => {
                if self.awaiting_start() {
                    self.buffer(Event::Done { stream_id, error });
                    return Step::default();
                }
                if self.stream_id.as_deref() != Some(stream_id.as_str()) || !self.streaming {
                    return Step::default();
                }
                self.streaming = false;
                if self.view.phase != Phase::Streaming {
                    return Step::default();
                }
                match error {
                    None => {
                        if let Some(r) = self.refining.clone()
                            && !r.kind.accepts(&r.base, &self.view.text)
                        {
                            if !r.strict {
                                // One stricter try before showing a miss.
                                return self.redraft(Some(RefineRequest { strict: true, ..r }));
                            }
                            self.view.message = Some(REFINE_MISSED.into());
                        }
                        self.view.phase = Phase::Done;
                        Step::changed(Vec::new())
                    }
                    // Cancelled by someone else (our own cancels stop `streaming` first).
                    Some(e) if e.trim() == super::errors::CANCELLED => {
                        self.active = false;
                        let mut effects = Vec::new();
                        self.hide(&mut effects);
                        Step::effects(effects)
                    }
                    Some(e) => {
                        self.view.phase = Phase::Error;
                        self.view.message = Some(super::errors::message_for(&e));
                        let effects = self.error_effects();
                        Step::changed(effects)
                    }
                }
            }
            Event::Accept(edited) => {
                if !self.active || !matches!(self.view.phase, Phase::Streaming | Phase::Done) {
                    return Step::default();
                }
                let shown = self.view.text.clone();
                let final_text = edited.unwrap_or_else(|| shown.clone());
                if final_text.trim().is_empty() {
                    return self.apply(Event::Cancel);
                }
                let outcome = if final_text == shown {
                    "accepted"
                } else {
                    "edited"
                };
                let mut effects = Vec::new();
                self.stop_stream(&mut effects);
                self.view.text = final_text.clone();
                self.view.phase = Phase::Inserting;
                self.accepted = Some((outcome.into(), final_text.clone()));
                // The insertion hands the keyboard back itself.
                self.focused = false;
                effects.push(Effect::Hide);
                effects.extend(self.remember(&final_text));
                effects.push(Effect::Insert(final_text));
                Step::changed(effects)
            }
            Event::InsertPrevious(text) => {
                // Also after a draft error that kept the keyboard; not after a failed insertion
                // (the target already has it).
                let drafting = matches!(self.view.phase, Phase::Streaming | Phase::Done);
                let open = drafting
                    || (self.view.phase == Phase::Error && self.focused && !self.insert_failed);
                if !self.active || !open || text.trim().is_empty() {
                    return Step::default();
                }
                let mut effects = Vec::new();
                self.stop_stream(&mut effects);
                if drafting {
                    effects.extend(self.feedback("rejected", None, None));
                    self.remember_shown(&mut effects);
                }
                self.view.text = text.clone();
                self.view.phase = Phase::Inserting;
                self.accepted = None;
                self.focused = false;
                effects.push(Effect::Hide);
                // Moves it to the top of the history.
                effects.extend(self.remember(&text));
                effects.push(Effect::Insert(text));
                Step::changed(effects)
            }
            Event::FocusLost => {
                let open = matches!(self.view.phase, Phase::Streaming | Phase::Done)
                    || (self.view.phase == Phase::Error && !self.insert_failed);
                if !self.active || !self.focused || !open {
                    return Step::default();
                }
                // The user went elsewhere on purpose: leave the keyboard where they put it.
                self.focused = false;
                self.apply_inner(Event::Cancel)
            }
            Event::Refocus => {
                let open = matches!(self.view.phase, Phase::Streaming | Phase::Done)
                    || (self.view.phase == Phase::Error && !self.insert_failed);
                if !self.active || !open {
                    return Step::default();
                }
                Step::effects(vec![self.show(true)])
            }
            Event::Retry => {
                if !self.can_retry() {
                    return Step::default();
                }
                self.redraft(None)
            }
            Event::Refine { kind, base } => {
                if !self.can_retry() || !matches!(self.view.phase, Phase::Streaming | Phase::Done) {
                    return Step::default();
                }
                let base = base
                    .filter(|b| !b.trim().is_empty())
                    .unwrap_or_else(|| self.view.text.clone());
                if base.trim().is_empty() {
                    return Step::default();
                }
                self.redraft(Some(RefineRequest {
                    kind,
                    base,
                    strict: false,
                }))
            }
            Event::Cancel => {
                if !self.active || self.view.phase == Phase::Inserting {
                    return Step::default();
                }
                let mut effects = Vec::new();
                if matches!(self.view.phase, Phase::Streaming | Phase::Done) {
                    self.stop_stream(&mut effects);
                    effects.extend(self.feedback("rejected", None, None));
                    self.remember_shown(&mut effects);
                }
                self.streaming = false;
                self.active = false;
                self.buffered.clear();
                self.hide(&mut effects);
                Step::effects(effects)
            }
            Event::Inserted { verified, method } => {
                if !self.active || self.view.phase != Phase::Inserting {
                    return Step::default();
                }
                self.view.phase = Phase::Inserted;
                self.view.verified = Some(verified);
                self.view.method = Some(method.into());
                let mut effects: Vec<Effect> = match self.accepted.take() {
                    Some((outcome, text)) => self
                        .feedback(&outcome, Some(text), Some(verified))
                        .into_iter()
                        .collect(),
                    None => Vec::new(),
                };
                effects.push(self.hide_after(INSERTED_MS));
                Step::changed(effects)
            }
            Event::Error(message) => {
                if !self.active
                    || !matches!(
                        self.view.phase,
                        Phase::Reading | Phase::Streaming | Phase::Done | Phase::Inserting
                    )
                {
                    return Step::default();
                }
                let mut effects = Vec::new();
                self.stop_stream(&mut effects);
                if self.view.phase == Phase::Inserting {
                    if let Some((outcome, text)) = self.accepted.take() {
                        effects.extend(self.feedback(&outcome, Some(text), Some(false)));
                    }
                    self.view.verified = Some(false);
                    self.insert_failed = true;
                }
                self.view.phase = Phase::Error;
                self.view.message = Some(message);
                self.buffered.clear();
                effects.extend(self.error_effects());
                Step::changed(effects)
            }
            Event::Refused(key) => {
                if !self.active || self.view.phase != Phase::Reading {
                    return Step::default();
                }
                self.view.phase = Phase::Refused;
                self.view.message = Some(key);
                self.buffered.clear();
                // Keeps the keyboard if a retry had it, so the timeout can hand it back.
                let show = self.show(self.focused);
                Step::changed(vec![show, self.hide_after(REFUSED_MS)])
            }
            Event::Disconnected => {
                if !self.active || self.view.phase != Phase::Streaming {
                    // Reading: the pending `draft.begin` fails on its own. Done: the draft is
                    // complete and can still be inserted.
                    return Step::default();
                }
                self.streaming = false;
                self.view.phase = Phase::Error;
                self.view.message = Some(super::errors::ERROR_OFFLINE.into());
                let effects = self.error_effects();
                Step::changed(effects)
            }
            Event::Expire { generation } => {
                if generation != self.generation
                    || !self.active
                    || !matches!(
                        self.view.phase,
                        Phase::Refused | Phase::Error | Phase::Inserted
                    )
                {
                    return Step::default();
                }
                self.active = false;
                let mut effects = Vec::new();
                self.hide(&mut effects);
                Step::effects(effects)
            }
        }
    }

    fn stop_stream(&mut self, effects: &mut Vec<Effect>) {
        if std::mem::take(&mut self.streaming)
            && let Some(id) = &self.stream_id
        {
            effects.push(Effect::CancelStream(id.clone()));
        }
    }

    /// A new request: bump the generation and forget the previous stream.
    fn new_request(&mut self) {
        self.generation += 1;
        self.stream_id = None;
        self.streaming = false;
        self.buffered.clear();
        self.buffered_bytes = 0;
        self.accepted = None;
        self.insert_failed = false;
    }

    fn awaiting_start(&self) -> bool {
        self.active && self.view.phase == Phase::Reading && self.stream_id.is_none()
    }

    fn buffer(&mut self, ev: Event) {
        let len = match &ev {
            Event::Delta { text, .. } => text.len(),
            _ => 0,
        };
        if self.buffered.len() < MAX_BUFFERED && self.buffered_bytes + len <= MAX_BUFFERED_BYTES {
            self.buffered_bytes += len;
            self.buffered.push(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Started` for the machine's current request.
    fn started(m: &Machine, id: &str) -> Event {
        Event::Started(Started {
            stream_id: id.into(),
            provider: "local".into(),
            locality: Locality::OnDevice,
            citations: 2,
            excluded_sensitive: 1,
            sensitive: false,
            generation: m.generation(),
        })
    }

    fn redraft(m: &Machine, refine: Option<(Refinement, String)>) -> Effect {
        Effect::Redraft {
            generation: m.generation(),
            refine: refine.map(|(kind, base)| RefineRequest {
                kind,
                base,
                strict: false,
            }),
        }
    }

    fn delta(id: &str, t: &str) -> Event {
        Event::Delta {
            stream_id: id.into(),
            text: t.into(),
        }
    }

    fn done(id: &str) -> Event {
        Event::Done {
            stream_id: id.into(),
            error: None,
        }
    }

    fn streaming(id: &str) -> Machine {
        let mut m = Machine::new();
        assert!(m.apply(Event::Begin).changed);
        let s = m.apply(started(&m, id));
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        m
    }

    fn feedback(id: &str, outcome: &str, text: Option<&str>, verified: Option<bool>) -> Effect {
        Effect::Feedback(DraftFeedback {
            stream_id: id.into(),
            outcome: outcome.into(),
            final_text: text.map(Into::into),
            insert_verified: verified,
        })
    }

    #[test]
    fn happy_path_accept_unchanged() {
        let mut m = streaming("s1");
        assert_eq!(m.view().phase, Phase::Streaming);
        assert_eq!(m.view().provider.as_deref(), Some("local"));
        assert_eq!(m.view().citations, 2);
        assert!(m.apply(delta("s1", "Hi ")).changed);
        assert!(!m.apply(delta("other", "noise")).changed);
        m.apply(delta("s1", "there"));
        assert_eq!(m.view().text, "Hi there");
        assert!(m.busy());
        assert_eq!(m.apply(Event::Begin), Step::default(), "ignored while busy");
        m.apply(done("s1"));
        assert_eq!(m.view().phase, Phase::Done);
        let s = m.apply(Event::Accept(None));
        assert_eq!(
            s.effects,
            vec![
                Effect::Hide,
                Effect::Remember("Hi there".into()),
                Effect::Insert("Hi there".into())
            ]
        );
        assert_eq!(m.view().phase, Phase::Inserting);
        assert_eq!(
            m.apply(Event::Cancel),
            Step::default(),
            "no cancel mid-insert"
        );
        let s = m.apply(Event::Inserted {
            verified: true,
            method: InsertMethod::Accessibility,
        });
        assert_eq!(m.view().phase, Phase::Inserted);
        assert_eq!(m.view().method, Some(Method::Accessibility));
        assert_eq!(m.view().verified, Some(true));
        let generation = m.generation;
        assert_eq!(
            s.effects,
            vec![
                feedback("s1", "accepted", Some("Hi there"), Some(true)),
                Effect::HideAfter {
                    generation,
                    ms: INSERTED_MS
                }
            ]
        );
        assert!(!m.busy());
        assert_eq!(
            m.apply(Event::Expire { generation }).effects,
            vec![Effect::Hide]
        );
        assert!(!m.is_active());
    }

    #[test]
    fn accept_while_streaming_cancels_the_rest_and_edits_are_reported() {
        let mut m = streaming("s2");
        m.apply(delta("s2", "Draft"));
        let s = m.apply(Event::Accept(Some("My edit".into())));
        assert_eq!(
            s.effects,
            vec![
                Effect::CancelStream("s2".into()),
                Effect::Hide,
                Effect::Remember("My edit".into()),
                Effect::Insert("My edit".into())
            ]
        );
        // Late deltas and the done of the cancelled stream change nothing.
        assert!(!m.apply(delta("s2", "more")).changed);
        assert!(!m.apply(done("s2")).changed);
        let s = m.apply(Event::Inserted {
            verified: false,
            method: InsertMethod::Clipboard,
        });
        assert_eq!(
            s.effects[0],
            feedback("s2", "edited", Some("My edit"), Some(false))
        );
        // Accepting an "edit" equal to the shown text is a plain accept.
        let mut m = streaming("s3");
        m.apply(delta("s3", "Same"));
        m.apply(done("s3"));
        m.apply(Event::Accept(Some("Same".into())));
        let s = m.apply(Event::Inserted {
            verified: true,
            method: InsertMethod::Accessibility,
        });
        assert_eq!(
            s.effects[0],
            feedback("s3", "accepted", Some("Same"), Some(true))
        );
    }

    #[test]
    fn cancel_rejects_and_cancels_the_stream() {
        let mut m = streaming("s4");
        let s = m.apply(Event::Cancel);
        assert_eq!(
            s.effects,
            vec![
                Effect::CancelStream("s4".into()),
                feedback("s4", "rejected", None, None),
                Effect::Hide,
                Effect::RestoreFocus
            ]
        );
        assert!(!m.is_active());
        // After done: no stream to cancel, still rejected.
        let mut m = streaming("s5");
        m.apply(done("s5"));
        assert_eq!(
            m.apply(Event::Cancel).effects,
            vec![
                feedback("s5", "rejected", None, None),
                Effect::Hide,
                Effect::RestoreFocus
            ]
        );
        // Accepting nothing is a cancel.
        let mut m = streaming("s6");
        assert_eq!(
            m.apply(Event::Accept(Some("  ".into()))).effects,
            vec![
                Effect::CancelStream("s6".into()),
                feedback("s6", "rejected", None, None),
                Effect::Hide,
                Effect::RestoreFocus
            ]
        );
    }

    #[test]
    fn deltas_before_started_are_replayed() {
        let mut m = Machine::new();
        m.apply(Event::Begin);
        assert!(!m.apply(delta("s7", "Early ")).changed);
        m.apply(delta("chat-x", "someone else's"));
        m.apply(delta("s7", "bird"));
        m.apply(done("s7"));
        let s = m.apply(started(&m, "s7"));
        assert!(s.changed);
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        assert_eq!(m.view().text, "Early bird");
        assert_eq!(m.view().phase, Phase::Done);
    }

    #[test]
    fn refusals_and_errors_show_briefly_without_focus() {
        let mut m = Machine::new();
        m.apply(Event::Begin);
        let s = m.apply(Event::Refused("doubletap.refused.secure".into()));
        assert_eq!(m.view().phase, Phase::Refused);
        assert_eq!(
            m.view().message.as_deref(),
            Some("doubletap.refused.secure")
        );
        let generation = m.generation;
        assert_eq!(
            s.effects,
            vec![
                Effect::Show { focus: false },
                Effect::HideAfter {
                    generation,
                    ms: REFUSED_MS
                }
            ]
        );
        // A new trigger may start while the refusal is still shown; the old timer is stale.
        assert!(!m.busy());
        m.apply(Event::Begin);
        assert_eq!(m.apply(Event::Expire { generation }), Step::default());
        assert!(m.is_active());

        // A stream error.
        let mut m = streaming("s8");
        let s = m.apply(Event::Done {
            stream_id: "s8".into(),
            error: Some("no provider could draft".into()),
        });
        assert_eq!(m.view().phase, Phase::Error);
        assert_eq!(
            m.view().message.as_deref(),
            Some("doubletap.error.no_provider")
        );
        // It can be retried: it keeps the keyboard and stays until the user decides.
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        assert!(m.view().can_retry);

        // An insertion error still reports feedback, unverified.
        let mut m = streaming("s9");
        m.apply(delta("s9", "x"));
        m.apply(Event::Accept(None));
        let s = m.apply(Event::Error("doubletap.error.insert_failed".into()));
        assert_eq!(
            s.effects[0],
            feedback("s9", "accepted", Some("x"), Some(false))
        );
        assert_eq!(s.effects[1], Effect::Show { focus: false });
        assert_eq!(m.view().verified, Some(false));

        // An error while streaming cancels the stream.
        let mut m = streaming("s10");
        let s = m.apply(Event::Error("boom".into()));
        assert_eq!(s.effects[0], Effect::CancelStream("s10".into()));
        // Refusals only apply while reading.
        assert_eq!(m.apply(Event::Refused("x".into())), Step::default());
    }

    #[test]
    fn cancelled_elsewhere_and_disconnects() {
        // A stream cancelled by someone else closes quietly, without feedback.
        let mut m = streaming("c1");
        m.apply(delta("c1", "x"));
        let s = m.apply(Event::Done {
            stream_id: "c1".into(),
            error: Some("cancelled".into()),
        });
        assert_eq!(s.effects, vec![Effect::Hide, Effect::RestoreFocus]);
        assert!(!m.is_active());

        // A dropped connection ends a running stream with the offline error.
        let mut m = streaming("c2");
        let s = m.apply(Event::Disconnected);
        assert_eq!(m.view().phase, Phase::Error);
        assert_eq!(m.view().message.as_deref(), Some("doubletap.error.offline"));
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        assert!(
            !m.apply(done("c2")).changed,
            "no cancel or done after the drop"
        );
        // A finished draft survives a drop and can still be accepted.
        let mut m = streaming("c3");
        m.apply(delta("c3", "ok"));
        m.apply(done("c3"));
        assert_eq!(m.apply(Event::Disconnected), Step::default());
        assert_eq!(
            m.apply(Event::Accept(None)).effects,
            vec![
                Effect::Hide,
                Effect::Remember("ok".into()),
                Effect::Insert("ok".into())
            ]
        );
        // While reading, the pending call reports the failure itself.
        let mut m = Machine::new();
        m.apply(Event::Begin);
        assert_eq!(m.apply(Event::Disconnected), Step::default());
    }

    #[test]
    fn idle_machine_ignores_everything_but_begin() {
        let mut m = Machine::new();
        for ev in [
            delta("a", "b"),
            done("a"),
            Event::Accept(None),
            Event::Cancel,
            Event::Inserted {
                verified: true,
                method: InsertMethod::Accessibility,
            },
            Event::Error("e".into()),
            Event::Refused("r".into()),
            Event::Disconnected,
            Event::Expire { generation: 0 },
            Event::Practice,
            Event::Retry,
            Event::Refine {
                kind: Refinement::Shorter,
                base: None,
            },
            Event::InsertPrevious("old".into()),
        ] {
            assert_eq!(m.apply(ev.clone()), Step::default(), "{ev:?}");
        }
    }

    #[test]
    fn retry_rejects_the_draft_and_asks_for_a_new_one() {
        let mut m = streaming("r1");
        m.apply(delta("r1", "First"));
        m.apply(done("r1"));
        assert!(m.view().can_retry);
        let s = m.apply(Event::Retry);
        assert!(s.changed);
        assert_eq!(
            s.effects,
            vec![
                feedback("r1", "rejected", None, None),
                Effect::Remember("First".into()),
                redraft(&m, None)
            ]
        );
        assert_eq!(m.view().phase, Phase::Reading);
        assert_eq!(m.view().text, "");
        assert!(!m.view().can_retry, "not while reading");
        assert!(m.busy(), "a new trigger waits for the retry");
        // Deltas of the old stream are ignored; the new stream takes over.
        assert!(!m.apply(delta("r1", "late")).changed);
        let s = m.apply(started(&m, "r2"));
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        assert_eq!(
            m.view().text,
            "",
            "the old stream's late delta was not replayed"
        );
        m.apply(delta("r2", "Second"));
        assert_eq!(m.view().text, "Second");
        // Retry while streaming cancels the running stream.
        let s = m.apply(Event::Retry);
        assert_eq!(s.effects[0], Effect::CancelStream("r2".into()));
        assert_eq!(s.effects.last(), Some(&redraft(&m, None)));
        // Esc while reading the retry still hands focus back.
        assert_eq!(
            m.apply(Event::Cancel).effects,
            vec![Effect::Hide, Effect::RestoreFocus]
        );
    }

    #[test]
    fn refinements_rewrite_the_shown_or_edited_text() {
        let mut m = streaming("f1");
        m.apply(delta("f1", "A long draft"));
        m.apply(done("f1"));
        let s = m.apply(Event::Refine {
            kind: Refinement::Shorter,
            base: None,
        });
        assert_eq!(
            s.effects.last(),
            Some(&redraft(
                &m,
                Some((Refinement::Shorter, "A long draft".into()))
            ))
        );
        assert_eq!(m.view().refinement, Some(Refinement::Shorter));
        m.apply(started(&m, "f2"));
        assert_eq!(m.view().refinement, Some(Refinement::Shorter));
        m.apply(delta("f2", "Short"));
        // The user's edit is the base when given; a blank edit falls back to the text shown.
        let s = m.apply(Event::Refine {
            kind: Refinement::Formal,
            base: Some("My words".into()),
        });
        assert_eq!(
            s.effects.last(),
            Some(&redraft(&m, Some((Refinement::Formal, "My words".into()))))
        );
        m.apply(started(&m, "f3"));
        m.apply(delta("f3", "Shown"));
        let s = m.apply(Event::Refine {
            kind: Refinement::Casual,
            base: Some("  ".into()),
        });
        assert_eq!(
            s.effects.last(),
            Some(&redraft(&m, Some((Refinement::Casual, "Shown".into()))))
        );
        // Nothing to refine yet.
        let mut m = streaming("f4");
        assert_eq!(
            m.apply(Event::Refine {
                kind: Refinement::Longer,
                base: None
            }),
            Step::default()
        );
        assert!(
            Refinement::Longer.instruction(10, false).contains("longer"),
            "instructions name what they do"
        );
        assert_eq!(
            serde_json::from_value::<Refinement>(serde_json::json!("formal")).unwrap(),
            Refinement::Formal
        );
    }

    #[test]
    fn errors_after_a_draft_can_be_retried_but_insert_failures_cannot() {
        let mut m = streaming("e1");
        m.apply(Event::Done {
            stream_id: "e1".into(),
            error: Some("boom".into()),
        });
        assert!(m.view().can_retry);
        assert_eq!(
            m.apply(Event::Retry).effects,
            vec![redraft(&m, None)],
            "no feedback or history for a failed draft"
        );
        // The retried request fails: still retryable, still with the keyboard.
        let s = m.apply(Event::Error("doubletap.error.draft_failed".into()));
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        assert!(m.view().can_retry);
        // A failed insertion is final.
        let mut m = streaming("e2");
        m.apply(delta("e2", "x"));
        m.apply(Event::Accept(None));
        m.apply(Event::Error("doubletap.error.insert_failed".into()));
        assert!(!m.view().can_retry);
        assert_eq!(m.apply(Event::Retry), Step::default());
        // A first draft that never started cannot be retried either.
        let mut m = Machine::new();
        m.apply(Event::Begin);
        let s = m.apply(Event::Error("doubletap.error.offline".into()));
        assert!(!m.view().can_retry);
        assert!(matches!(
            s.effects[..],
            [Effect::Show { focus: false }, Effect::HideAfter { .. }]
        ));
    }

    #[test]
    fn insert_previous_replaces_the_draft_without_feedback_for_it() {
        let mut m = streaming("p1");
        m.apply(delta("p1", "New"));
        let s = m.apply(Event::InsertPrevious("Old draft".into()));
        assert_eq!(
            s.effects,
            vec![
                Effect::CancelStream("p1".into()),
                feedback("p1", "rejected", None, None),
                Effect::Hide,
                Effect::Remember("Old draft".into()),
                Effect::Insert("Old draft".into())
            ]
        );
        assert_eq!(m.view().phase, Phase::Inserting);
        let s = m.apply(Event::Inserted {
            verified: true,
            method: InsertMethod::Accessibility,
        });
        assert!(
            matches!(s.effects[..], [Effect::HideAfter { .. }]),
            "no accepted/edited feedback for a previous draft"
        );
        // After a draft error that kept the keyboard: no feedback for the failed draft.
        let mut m = streaming("p2");
        m.apply(Event::Error("doubletap.error.draft_failed".into()));
        assert_eq!(
            m.apply(Event::InsertPrevious("Older".into())).effects,
            vec![
                Effect::Hide,
                Effect::Remember("Older".into()),
                Effect::Insert("Older".into())
            ]
        );
    }

    #[test]
    fn practice_sessions_send_no_feedback_and_keep_no_history() {
        let mut m = Machine::new();
        m.apply(Event::Begin);
        assert!(m.apply(Event::Practice).changed);
        assert!(m.view().practice);
        m.apply(started(&m, "q1"));
        m.apply(delta("q1", "Practice draft"));
        m.apply(done("q1"));
        let s = m.apply(Event::Retry);
        assert_eq!(s.effects, vec![redraft(&m, None)]);
        assert!(m.view().practice, "a retry stays in practice");
        m.apply(started(&m, "q2"));
        m.apply(delta("q2", "Again"));
        let s = m.apply(Event::Accept(None));
        assert_eq!(
            s.effects,
            vec![
                Effect::CancelStream("q2".into()),
                Effect::Hide,
                Effect::Insert("Again".into())
            ]
        );
        let s = m.apply(Event::Inserted {
            verified: true,
            method: InsertMethod::Accessibility,
        });
        assert!(matches!(s.effects[..], [Effect::HideAfter { .. }]));
        // The next real session is not practice.
        let g = m.generation;
        m.apply(Event::Expire { generation: g });
        m.apply(Event::Begin);
        assert!(!m.view().practice);
    }

    #[test]
    fn sensitive_drafts_are_never_remembered() {
        let mut m = Machine::new();
        m.apply(Event::Begin);
        m.apply(Event::Started(Started {
            stream_id: "z1".into(),
            provider: "local".into(),
            locality: Locality::OnDevice,
            citations: 1,
            excluded_sensitive: 0,
            sensitive: true,
            generation: m.generation(),
        }));
        m.apply(delta("z1", "Your PIN is 4821"));
        m.apply(done("z1"));
        // Sticky for the session: a retry's draft may restate it.
        m.apply(Event::Retry);
        m.apply(started(&m, "z2"));
        m.apply(delta("z2", "Again"));
        let s = m.apply(Event::Accept(None));
        assert!(!s.effects.iter().any(|e| matches!(e, Effect::Remember(_))));
    }

    #[test]
    fn a_cancelled_retry_never_reaches_the_next_session() {
        let mut m = streaming("a1");
        m.apply(delta("a1", "For app A"));
        m.apply(done("a1"));
        m.apply(Event::Retry);
        let stale = started(&m, "a2");
        m.apply(Event::Cancel);
        // A new trigger in another app, while A's retry is still in flight.
        assert!(m.apply(Event::Begin).changed);
        assert_eq!(
            m.apply(stale),
            Step::effects(vec![Effect::CancelStream("a2".into())]),
            "the old request's draft is cancelled, not shown"
        );
        assert_eq!(m.view().phase, Phase::Reading);
        // The new session's own draft still starts.
        let s = m.apply(started(&m, "b1"));
        assert_eq!(s.effects, vec![Effect::Show { focus: true }]);
        // A newer retry replaces an older one in flight too.
        m.apply(done("b1"));
        m.apply(Event::Retry);
        let older = started(&m, "b2");
        m.apply(Event::Error("doubletap.error.draft_failed".into()));
        m.apply(Event::Retry);
        assert_eq!(
            m.apply(older).effects,
            vec![Effect::CancelStream("b2".into())]
        );
    }

    #[test]
    fn a_focused_error_keeps_the_trigger_from_starting_over_it() {
        let mut m = streaming("k1");
        m.apply(Event::Done {
            stream_id: "k1".into(),
            error: Some("boom".into()),
        });
        assert!(
            m.busy(),
            "the overlay holds the keyboard: Esc or Retry first"
        );
        assert_eq!(m.apply(Event::Begin), Step::default());
        assert_eq!(
            m.apply(Event::Cancel).effects,
            vec![Effect::Hide, Effect::RestoreFocus]
        );
        assert!(!m.busy());
        // A refinement sends no feedback for the draft it rewrites.
        let mut m = streaming("k2");
        m.apply(delta("k2", "Text"));
        m.apply(done("k2"));
        let s = m.apply(Event::Refine {
            kind: Refinement::Shorter,
            base: None,
        });
        assert!(!s.effects.iter().any(|e| matches!(e, Effect::Feedback(_))));
    }

    #[test]
    fn losing_the_keyboard_closes_the_draft_and_the_trigger_always_recovers() {
        // JJ1-2: the user clicks back into the target while the draft is done.
        let mut m = streaming("j1");
        m.apply(delta("j1", "Done draft"));
        m.apply(done("j1"));
        assert!(m.busy());
        let s = m.apply(Event::FocusLost);
        assert_eq!(
            s.effects,
            vec![
                feedback("j1", "rejected", None, None),
                Effect::Remember("Done draft".into()),
                Effect::Hide
            ],
            "closed, kept in the history, the keyboard left where the user put it"
        );
        assert!(!m.busy(), "the next trigger starts a new draft");
        assert!(m.apply(Event::Begin).changed);
        // A focus loss while reading, inserting or closed changes nothing.
        assert_eq!(m.apply(Event::FocusLost), Step::default());
        let mut m = streaming("j2");
        m.apply(delta("j2", "x"));
        m.apply(Event::Accept(None));
        assert_eq!(m.apply(Event::FocusLost), Step::default());
        // The trigger while a draft is on screen brings it back with the keyboard.
        let mut m = streaming("j3");
        m.apply(Event::FocusLost);
        let mut m2 = streaming("j4");
        assert_eq!(
            m2.apply(Event::Refocus).effects,
            vec![Effect::Show { focus: true }]
        );
        assert_eq!(m.apply(Event::Refocus), Step::default(), "nothing open");
        let mut m3 = Machine::new();
        m3.apply(Event::Begin);
        assert_eq!(
            m3.apply(Event::Refocus),
            Step::default(),
            "reading finishes on its own"
        );
    }

    #[test]
    fn a_refinement_that_misses_is_tried_once_more_then_flagged() {
        let base = "Thanks for the update, I can make the meeting on Thursday at noon.";
        let mut m = streaming("v1");
        m.apply(delta("v1", base));
        m.apply(done("v1"));
        m.apply(Event::Refine {
            kind: Refinement::Shorter,
            base: None,
        });
        m.apply(started(&m, "v2"));
        m.apply(delta("v2", base));
        // Unchanged: a stricter second try, not a "done".
        let s = m.apply(done("v2"));
        assert_eq!(m.view().phase, Phase::Reading);
        assert_eq!(
            s.effects.last(),
            Some(&Effect::Redraft {
                generation: m.generation(),
                refine: Some(RefineRequest {
                    kind: Refinement::Shorter,
                    base: base.into(),
                    strict: true
                })
            })
        );
        m.apply(started(&m, "v3"));
        m.apply(delta(
            "v3",
            "Thanks for the update, I can make the meeting on Thursday at noon, sure.",
        ));
        m.apply(done("v3"));
        assert_eq!(m.view().phase, Phase::Done);
        assert_eq!(
            m.view().message.as_deref(),
            Some(REFINE_MISSED),
            "not shorter: said so"
        );
        // A rewrite that does what was asked is just done.
        let mut m = streaming("w1");
        m.apply(delta("w1", base));
        m.apply(done("w1"));
        m.apply(Event::Refine {
            kind: Refinement::Shorter,
            base: None,
        });
        m.apply(started(&m, "w2"));
        m.apply(delta("w2", "Thursday at noon works."));
        m.apply(done("w2"));
        assert_eq!(m.view().phase, Phase::Done);
        assert_eq!(m.view().message, None);
    }

    #[test]
    fn refinement_checks_count_words_in_every_script() {
        assert_eq!(length_units("Hello there,  Ada!"), 3);
        assert_eq!(length_units("会議は木曜日です"), 8);
        assert_eq!(length_units("OK 了解"), 3);
        assert!(
            !Refinement::Formal.accepts("Hi  there", "Hi there"),
            "whitespace only"
        );
        assert!(Refinement::Formal.accepts("Hi there", "Good afternoon."));
        assert!(Refinement::Longer.accepts("Hi there", "Hi there, see you soon."));
        assert!(!Refinement::Longer.accepts("Hi there, see you", "Hi."));
        assert!(!Refinement::Shorter.accepts("Hi there", ""));
        assert_eq!(Refinement::Shorter.target_units(1), 1);
        assert_eq!(Refinement::Shorter.target_units(10), 6);
    }
}
