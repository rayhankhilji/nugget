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

use nugget_a11y::InsertMethod;
use nugget_core::capability::Locality;
use nugget_core::ipc::DraftFeedback;
use serde::Serialize;

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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The trigger fired. Ignored while a draft is in progress.
    Begin,
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
    Inserted {
        verified: bool,
        method: InsertMethod,
    },
    /// Anything failed; the message is an i18n key or raw text.
    Error(String),
    /// Refused locally or by the daemon; the message is an i18n key.
    Refused(String),
    /// A [`Effect::HideAfter`] timer ran out.
    Expire { generation: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Place the overlay near the caret and show it; `focus` gives it the keyboard (needed for
    /// Enter/Tab/Esc), otherwise it is shown without taking focus where the platform allows.
    Show { focus: bool },
    Hide,
    /// Feed [`Event::Expire`] back after `ms`.
    HideAfter { generation: u64, ms: u64 },
    /// `draft.cancel`.
    CancelStream(String),
    /// Hand focus back to the target app and insert this text.
    Insert(String),
    /// `draft.feedback`.
    Feedback(DraftFeedback),
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
    /// Outcome and text of the accepted draft while inserting.
    accepted: Option<(String, String)>,
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn view(&self) -> &OverlayView {
        &self.view
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }

    /// A draft is in progress: a new trigger is ignored.
    pub fn busy(&self) -> bool {
        self.active
            && matches!(
                self.view.phase,
                Phase::Reading | Phase::Streaming | Phase::Done | Phase::Inserting
            )
    }

    fn hide_after(&self, ms: u64) -> Effect {
        Effect::HideAfter {
            generation: self.generation,
            ms,
        }
    }

    fn feedback(&self, outcome: &str, final_text: Option<String>, verified: Option<bool>) -> Option<Effect> {
        self.stream_id.as_ref().map(|id| {
            Effect::Feedback(DraftFeedback {
                stream_id: id.clone(),
                outcome: outcome.into(),
                final_text,
                insert_verified: verified,
            })
        })
    }

    pub fn apply(&mut self, event: Event) -> Step {
        match event {
            Event::Begin => {
                if self.busy() {
                    return Step::default();
                }
                self.generation += 1;
                self.view = OverlayView::default();
                self.active = true;
                self.stream_id = None;
                self.streaming = false;
                self.buffered.clear();
                self.buffered_bytes = 0;
                self.accepted = None;
                Step::changed(Vec::new())
            }
            Event::Started(s) => {
                if !self.active || self.view.phase != Phase::Reading || self.stream_id.is_some() {
                    // Cancelled (or refused) while the request was in flight.
                    return Step::effects(vec![Effect::CancelStream(s.stream_id)]);
                }
                self.view.phase = Phase::Streaming;
                self.view.provider = Some(s.provider);
                self.view.locality = Some(s.locality);
                self.view.citations = s.citations;
                self.view.excluded_sensitive = s.excluded_sensitive;
                self.stream_id = Some(s.stream_id.clone());
                self.streaming = true;
                let mut step = Step::changed(vec![Effect::Show { focus: true }]);
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
                        self.view.phase = Phase::Done;
                        Step::changed(Vec::new())
                    }
                    Some(e) => {
                        self.view.phase = Phase::Error;
                        self.view.message = Some(super::errors::message_for(&e));
                        Step::changed(vec![self.hide_after(ERROR_MS)])
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
                if self.streaming
                    && let Some(id) = &self.stream_id
                {
                    effects.push(Effect::CancelStream(id.clone()));
                    self.streaming = false;
                }
                self.view.text = final_text.clone();
                self.view.phase = Phase::Inserting;
                self.accepted = Some((outcome.into(), final_text.clone()));
                effects.push(Effect::Hide);
                effects.push(Effect::Insert(final_text));
                Step::changed(effects)
            }
            Event::Cancel => {
                if !self.active || self.view.phase == Phase::Inserting {
                    return Step::default();
                }
                let mut effects = Vec::new();
                if matches!(self.view.phase, Phase::Streaming | Phase::Done) {
                    if self.streaming
                        && let Some(id) = &self.stream_id
                    {
                        effects.push(Effect::CancelStream(id.clone()));
                    }
                    effects.extend(self.feedback("rejected", None, None));
                }
                self.streaming = false;
                self.active = false;
                self.buffered.clear();
                effects.push(Effect::Hide);
                Step::effects(effects)
            }
            Event::Inserted { verified, method } => {
                if !self.active || self.view.phase != Phase::Inserting {
                    return Step::default();
                }
                self.view.phase = Phase::Inserted;
                self.view.verified = Some(verified);
                self.view.method = Some(method.into());
                let (outcome, text) = self.accepted.take().unwrap_or_default();
                let mut effects: Vec<Effect> =
                    self.feedback(&outcome, Some(text), Some(verified)).into_iter().collect();
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
                if self.streaming
                    && let Some(id) = &self.stream_id
                {
                    effects.push(Effect::CancelStream(id.clone()));
                }
                self.streaming = false;
                if self.view.phase == Phase::Inserting {
                    let (outcome, text) = self.accepted.take().unwrap_or_default();
                    effects.extend(self.feedback(&outcome, Some(text), Some(false)));
                    self.view.verified = Some(false);
                }
                self.view.phase = Phase::Error;
                self.view.message = Some(message);
                self.buffered.clear();
                effects.push(Effect::Show { focus: false });
                effects.push(self.hide_after(ERROR_MS));
                Step::changed(effects)
            }
            Event::Refused(key) => {
                if !self.active || self.view.phase != Phase::Reading {
                    return Step::default();
                }
                self.view.phase = Phase::Refused;
                self.view.message = Some(key);
                self.buffered.clear();
                Step::changed(vec![
                    Effect::Show { focus: false },
                    self.hide_after(REFUSED_MS),
                ])
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
                Step::effects(vec![Effect::Hide])
            }
        }
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

    fn started(id: &str) -> Event {
        Event::Started(Started {
            stream_id: id.into(),
            provider: "local".into(),
            locality: Locality::OnDevice,
            citations: 2,
            excluded_sensitive: 1,
        })
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
        let s = m.apply(started(id));
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
            vec![Effect::Hide, Effect::Insert("Hi there".into())]
        );
        assert_eq!(m.view().phase, Phase::Inserting);
        assert_eq!(m.apply(Event::Cancel), Step::default(), "no cancel mid-insert");
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
        assert_eq!(s.effects[0], feedback("s3", "accepted", Some("Same"), Some(true)));
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
                Effect::Hide
            ]
        );
        assert!(!m.is_active());
        // After done: no stream to cancel, still rejected.
        let mut m = streaming("s5");
        m.apply(done("s5"));
        assert_eq!(
            m.apply(Event::Cancel).effects,
            vec![feedback("s5", "rejected", None, None), Effect::Hide]
        );
        // Accepting nothing is a cancel.
        let mut m = streaming("s6");
        assert_eq!(
            m.apply(Event::Accept(Some("  ".into()))).effects,
            vec![
                Effect::CancelStream("s6".into()),
                feedback("s6", "rejected", None, None),
                Effect::Hide
            ]
        );
    }

    #[test]
    fn started_after_cancel_is_cancelled() {
        let mut m = Machine::new();
        m.apply(Event::Begin);
        assert_eq!(m.apply(Event::Cancel).effects, vec![Effect::Hide]);
        assert_eq!(
            m.apply(started("late")),
            Step::effects(vec![Effect::CancelStream("late".into())])
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
        let s = m.apply(started("s7"));
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
        assert_eq!(m.view().message.as_deref(), Some("doubletap.refused.secure"));
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
            error: Some("invalid input: drafting is refused in password and secure fields".into()),
        });
        assert_eq!(m.view().phase, Phase::Error);
        assert_eq!(m.view().message.as_deref(), Some("doubletap.refused.secure"));
        assert!(matches!(s.effects[..], [Effect::HideAfter { ms: ERROR_MS, .. }]));

        // An insertion error still reports feedback, unverified.
        let mut m = streaming("s9");
        m.apply(delta("s9", "x"));
        m.apply(Event::Accept(None));
        let s = m.apply(Event::Error("doubletap.error.focus_lost".into()));
        assert_eq!(s.effects[0], feedback("s9", "accepted", Some("x"), Some(false)));
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
    fn view_serializes_per_contract() {
        let mut m = streaming("s11");
        m.apply(delta("s11", "Hey"));
        let v = serde_json::to_value(m.view()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "phase": "streaming",
                "text": "Hey",
                "provider": "local",
                "locality": "on_device",
                "citations": 2,
                "excluded_sensitive": 1,
                "message": null,
                "verified": null,
                "method": null
            })
        );
        assert_eq!(serde_json::to_value(Method::Clipboard).unwrap(), "clipboard");
        assert_eq!(serde_json::to_value(Phase::Inserted).unwrap(), "inserted");
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
            Event::Expire { generation: 0 },
        ] {
            assert_eq!(m.apply(ev.clone()), Step::default(), "{ev:?}");
        }
    }
}
