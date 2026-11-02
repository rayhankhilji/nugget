//! Ghost text: the suggestion drawn translucently at the caret of any app, as a pure state
//! machine. The runtime glue (key tap, reads, the window, insertion) is in [`super::ghost_flow`].
//!
//! # Lifecycle
//!
//! 1. **Trigger.** Automatic: every key press (the key tap reports only that *a* key was
//!    pressed, never which) restarts a [`DEBOUNCE`] timer; when it runs out the field is read
//!    and, if every guard passes, a `continue` draft starts. On demand: Double Tap starts a
//!    `reply` draft at once.
//! 2. **Streaming.** Deltas are appended to the suggestion and shown as they arrive. The key
//!    tap is made active (Tab, accept-word and Esc consumed) only while something is visible.
//! 3. **Typing over it.** A key press while a suggestion is visible re-reads the field. When
//!    the text before the caret is the anchor plus a prefix of the suggestion, that prefix is
//!    consumed and the rest stays; otherwise the suggestion disappears and its stream is
//!    cancelled ([`advance`]).
//! 4. **Accept / dismiss.** Tab inserts the rest of the suggestion, accept-word its next word
//!    ([`next_word`]), Esc dismisses it (and automatic suggestions pause until the text
//!    changes). A click, a focus change or another field dismisses it too.
//!
//! Every request carries a generation; anything that arrives for an older one (a slow
//! `draft.begin`, a late read) is dropped, and a stream started for it is cancelled.

use std::time::Duration;

use nugget_a11y::CaretGeometry;
use serde::Serialize;

/// Typing pause before an automatic suggestion is requested.
pub const DEBOUNCE: Duration = Duration::from_millis(400);
/// After a key press over a visible suggestion, wait this long before reading the field back,
/// so the app has applied the key.
pub const REREAD_DELAY: Duration = Duration::from_millis(40);
/// While a suggestion is visible, the field is read again this often to follow it when it
/// scrolls or moves, and to notice a focus change.
pub const FOLLOW_EVERY: Duration = Duration::from_millis(250);

/// What started a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A typing pause (`continue` draft).
    Auto,
    /// Double Tap (`reply` draft).
    DoubleTap,
}

/// Where a suggestion belongs: the text before the caret when it was requested, and what
/// identifies the field.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub before: String,
    pub field: super::flow::FieldSig,
    pub app_key: String,
    pub geometry: CaretGeometry,
    pub rtl: bool,
}

/// A read of the focused field, reduced to what the machine compares.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub before: String,
    pub field: super::flow::FieldSig,
    pub app_key: String,
    pub geometry: Option<CaretGeometry>,
}

/// How the text before the caret relates to the suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Nothing was typed since the anchor.
    Same,
    /// The user typed the first `n` bytes of the suggestion.
    Advanced(usize),
    /// Something else: the suggestion no longer applies.
    Diverged,
}

/// Compare the text before the caret now with the anchor and the suggestion so far.
pub fn advance(anchor: &str, suggestion: &str, now: &str) -> Match {
    let Some(typed) = now.strip_prefix(anchor) else {
        return Match::Diverged;
    };
    if typed.is_empty() {
        Match::Same
    } else if suggestion.starts_with(typed) {
        Match::Advanced(typed.len())
    } else {
        Match::Diverged
    }
}

/// The next word of `rest` for accept-word: leading whitespace, then up to the next whitespace
/// (punctuation stays with its word). Never splits a character.
pub fn next_word(rest: &str) -> &str {
    let start = rest.len() - rest.trim_start().len();
    let end = rest[start..]
        .find(char::is_whitespace)
        .map_or(rest.len(), |i| start + i);
    &rest[..end]
}

/// What the ghost window shows (sent to it as is).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GhostView {
    pub visible: bool,
    /// The rest of the suggestion (what Tab would insert).
    pub text: String,
    /// More may still arrive.
    pub streaming: bool,
    pub rtl: bool,
    /// Accept-word is available (the rest has more than one word).
    pub words: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    /// A request is being read or started.
    Pending,
    /// A suggestion (possibly still empty) for the current generation.
    Live {
        stream: Option<String>,
        text: String,
        /// Bytes of `text` already typed or accepted.
        consumed: usize,
        done: bool,
        /// An accept-word insert is in flight: key presses are ours.
        inserting: bool,
    },
    /// Tab: the rest is being inserted.
    Inserting,
}

/// Something for the runtime to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Show (or update) the window at the anchor's geometry.
    Show(GhostView),
    Hide,
    SetTapActive(bool),
    CancelStream(String),
    /// Start (or restart) the typing-pause timer; it reports back with this token.
    Debounce(u64),
    /// Read the field back after [`REREAD_DELAY`] and report a [`Event::Reread`].
    Reread {
        generation: u64,
    },
    /// Insert `text` at the caret, if the text before the caret is still `expect_before`.
    Insert {
        generation: u64,
        text: String,
        expect_before: String,
        whole: bool,
    },
    /// The suggestion was accepted (`draft.feedback`).
    Accepted {
        stream_id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A key press (identity unknown).
    Typed,
    Click,
    Accept,
    AcceptWord,
    Dismiss,
    /// A request starts (guards passed); returns its generation via [`Machine::generation`].
    Request {
        origin: Origin,
        anchor: Anchor,
    },
    /// The request could not start (refused, failed, no field).
    Failed {
        generation: u64,
    },
    Started {
        generation: u64,
        stream_id: String,
    },
    Delta {
        stream_id: String,
        text: String,
    },
    Done {
        stream_id: String,
        error: bool,
    },
    Reread {
        generation: u64,
        reading: Option<Reading>,
    },
    Inserted {
        generation: u64,
        ok: bool,
    },
    /// Focus left the field, the app or the kill switch engaged: drop everything.
    Reset,
}

/// The ghost text machine.
#[derive(Debug)]
pub struct Machine {
    phase: Phase,
    generation: u64,
    /// Restarted on every key press; a pause fires only for the latest.
    pause_token: u64,
    anchor: Option<Anchor>,
    origin: Origin,
    /// After Esc: no automatic suggestion while the text before the caret is still this.
    suppressed: Option<String>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            generation: 0,
            pause_token: 0,
            anchor: None,
            origin: Origin::Auto,
            suppressed: None,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The latest typing-pause token ([`Effect::Debounce`]).
    pub fn pause_token(&self) -> u64 {
        self.pause_token
    }

    /// Whether nothing is pending or shown (a pause may start an automatic request).
    pub fn idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// Whether a suggestion is on screen.
    pub fn visible(&self) -> bool {
        matches!(&self.phase, Phase::Live { text, consumed, .. } if text.len() > *consumed)
    }

    pub fn anchor(&self) -> Option<&Anchor> {
        self.anchor.as_ref()
    }

    /// Whether an automatic suggestion may start for this text (not right after Esc on it).
    pub fn auto_allowed(&self, before: &str) -> bool {
        self.suppressed.as_deref() != Some(before)
    }

    pub fn view(&self) -> GhostView {
        match &self.phase {
            Phase::Live {
                text,
                consumed,
                done,
                ..
            } if text.len() > *consumed => {
                let rest = &text[*consumed..];
                GhostView {
                    visible: true,
                    text: rest.to_owned(),
                    streaming: !done,
                    rtl: self.anchor.as_ref().is_some_and(|a| a.rtl),
                    words: next_word(rest).len() < rest.trim_end().len(),
                }
            }
            _ => GhostView::default(),
        }
    }

    pub fn apply(&mut self, event: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        match event {
            Event::Typed => match &mut self.phase {
                Phase::Idle | Phase::Pending => {
                    if self.phase == Phase::Pending {
                        // The user kept typing: whatever starts for the old request is stale.
                        self.generation += 1;
                        self.phase = Phase::Idle;
                        self.anchor = None;
                    }
                    self.pause_token += 1;
                    fx.push(Effect::Debounce(self.pause_token));
                }
                Phase::Live { inserting, .. } => {
                    if !*inserting {
                        fx.push(Effect::Reread {
                            generation: self.generation,
                        });
                    }
                }
                Phase::Inserting => {}
            },
            Event::Click | Event::Reset => self.clear(&mut fx),
            Event::Dismiss => {
                self.suppressed = self.anchor.as_ref().map(|a| self.current_before(a));
                self.clear(&mut fx);
            }
            Event::Request { origin, anchor } => {
                self.clear(&mut fx);
                self.generation += 1;
                self.origin = origin;
                self.suppressed = None;
                self.anchor = Some(anchor);
                self.phase = Phase::Pending;
            }
            Event::Failed { generation } => {
                if generation == self.generation && self.phase == Phase::Pending {
                    self.phase = Phase::Idle;
                    self.anchor = None;
                }
            }
            Event::Started {
                generation,
                stream_id,
            } => {
                if generation == self.generation && self.phase == Phase::Pending {
                    self.phase = Phase::Live {
                        stream: Some(stream_id),
                        text: String::new(),
                        consumed: 0,
                        done: false,
                        inserting: false,
                    };
                } else {
                    fx.push(Effect::CancelStream(stream_id));
                }
            }
            Event::Delta { stream_id, text } => {
                let was = self.visible();
                if let Phase::Live {
                    stream: Some(s),
                    text: t,
                    done: false,
                    ..
                } = &mut self.phase
                    && *s == stream_id
                {
                    t.push_str(&text);
                    if self.visible() {
                        if !was {
                            fx.push(Effect::SetTapActive(true));
                        }
                        fx.push(Effect::Show(self.view()));
                    }
                }
            }
            Event::Done { stream_id, error } => {
                if let Phase::Live {
                    stream: Some(s),
                    done,
                    ..
                } = &mut self.phase
                    && *s == stream_id
                {
                    *done = true;
                    if error || !self.visible() {
                        // A failed or empty suggestion just goes away (no error UI over
                        // someone else's field).
                        self.clear_keep_stream(&mut fx);
                    } else {
                        fx.push(Effect::Show(self.view()));
                    }
                }
            }
            Event::Reread {
                generation,
                reading,
            } => {
                if generation != self.generation || !matches!(self.phase, Phase::Live { .. }) {
                    return fx;
                }
                self.reread(reading, &mut fx);
            }
            Event::Accept => self.accept(false, &mut fx),
            Event::AcceptWord => self.accept(true, &mut fx),
            Event::Inserted { generation, ok } => {
                if generation != self.generation {
                    return fx;
                }
                match &mut self.phase {
                    Phase::Inserting => {
                        self.phase = Phase::Idle;
                        self.anchor = None;
                    }
                    Phase::Live { inserting, .. } => {
                        *inserting = false;
                        if ok {
                            // Read back where the caret is now.
                            fx.push(Effect::Reread {
                                generation: self.generation,
                            });
                        } else {
                            self.clear(&mut fx);
                        }
                    }
                    _ => {}
                }
            }
        }
        fx
    }

    fn current_before(&self, a: &Anchor) -> String {
        match &self.phase {
            Phase::Live { text, consumed, .. } => format!("{}{}", a.before, &text[..*consumed]),
            _ => a.before.clone(),
        }
    }

    fn reread(&mut self, reading: Option<Reading>, fx: &mut Vec<Effect>) {
        let Some(anchor) = self.anchor.as_mut() else {
            return;
        };
        let Some(r) = reading else {
            return self.clear(fx);
        };
        if r.app_key != anchor.app_key || !anchor.field.matches(&r.field) {
            return self.clear(fx);
        }
        let Phase::Live {
            text,
            consumed,
            done,
            ..
        } = &mut self.phase
        else {
            return;
        };
        match advance(&anchor.before, text, &r.before) {
            Match::Diverged => {
                self.clear(fx);
                // The user is typing something else: suggest again after the next pause.
                self.pause_token += 1;
                fx.push(Effect::Debounce(self.pause_token));
            }
            m @ (Match::Same | Match::Advanced(_)) => {
                if let Match::Advanced(n) = m {
                    *consumed = n;
                }
                if let Some(g) = r.geometry {
                    anchor.geometry = g;
                }
                let finished = *done && *consumed >= text.len();
                if finished {
                    self.clear(fx);
                } else if self.visible() {
                    fx.push(Effect::Show(self.view()));
                }
            }
        }
    }

    fn accept(&mut self, word: bool, fx: &mut Vec<Effect>) {
        let Some(anchor) = self.anchor.as_ref() else {
            return;
        };
        let expect_before = self.current_before(anchor);
        let generation = self.generation;
        let Phase::Live {
            stream,
            text,
            consumed,
            done,
            inserting,
        } = &mut self.phase
        else {
            return;
        };
        if *inserting || *consumed >= text.len() {
            return;
        }
        let rest = &text[*consumed..];
        let piece = if word { next_word(rest) } else { rest };
        if piece.is_empty() {
            return;
        }
        let piece = piece.to_owned();
        let whole = !word || (*done && *consumed + piece.len() >= text.len());
        if let Some(id) = stream.clone() {
            fx.push(Effect::Accepted { stream_id: id });
        }
        if whole {
            if !*done && let Some(id) = stream.take() {
                // What is shown is what goes in; the rest of the stream is not wanted.
                fx.push(Effect::CancelStream(id));
            }
            self.phase = Phase::Inserting;
            fx.push(Effect::SetTapActive(false));
            fx.push(Effect::Hide);
        } else {
            *consumed += piece.len();
            *inserting = true;
            fx.push(Effect::Show(self.view()));
        }
        fx.push(Effect::Insert {
            generation,
            text: piece,
            expect_before,
            whole,
        });
    }

    /// Drop the suggestion: cancel its stream if it is still running, hide, release the keys.
    fn clear(&mut self, fx: &mut Vec<Effect>) {
        if let Phase::Live {
            stream: Some(id),
            done: false,
            ..
        } = &self.phase
        {
            fx.push(Effect::CancelStream(id.clone()));
        }
        self.clear_keep_stream(fx);
    }

    fn clear_keep_stream(&mut self, fx: &mut Vec<Effect>) {
        let shown = self.visible();
        let busy = !matches!(self.phase, Phase::Idle);
        if busy {
            self.generation += 1;
        }
        self.phase = Phase::Idle;
        self.anchor = None;
        if shown || busy {
            fx.push(Effect::SetTapActive(false));
            fx.push(Effect::Hide);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::flow::FieldSig;
    use super::*;
    use nugget_a11y::{CaretSource, Rect, TextStyle};

    fn geometry(x: f64) -> CaretGeometry {
        CaretGeometry {
            caret: Rect {
                x,
                y: 100.0,
                width: 1.0,
                height: 16.0,
            },
            source: CaretSource::Range,
            field: None,
            style: TextStyle::default(),
        }
    }

    fn sig() -> FieldSig {
        FieldSig {
            role: "text_area".into(),
            label: Some("Message".into()),
            placeholder: None,
            caret: None,
        }
    }

    fn anchor(before: &str) -> Anchor {
        Anchor {
            before: before.into(),
            field: sig(),
            app_key: "com.tinyspeck.slackmacgap".into(),
            geometry: geometry(100.0),
            rtl: false,
        }
    }

    fn reading(before: &str) -> Reading {
        Reading {
            before: before.into(),
            field: sig(),
            app_key: "com.tinyspeck.slackmacgap".into(),
            geometry: Some(geometry(120.0)),
        }
    }

    /// A machine showing `text` for "I'll send" (stream "s1", still streaming).
    fn showing(text: &str) -> Machine {
        let mut m = Machine::new();
        m.apply(Event::Request {
            origin: Origin::Auto,
            anchor: anchor("I'll send"),
        });
        let g = m.generation();
        m.apply(Event::Started {
            generation: g,
            stream_id: "s1".into(),
        });
        let fx = m.apply(Event::Delta {
            stream_id: "s1".into(),
            text: text.into(),
        });
        assert_eq!(fx[0], Effect::SetTapActive(true));
        m
    }

    fn reread(m: &mut Machine, before: &str) -> Vec<Effect> {
        let fx = m.apply(Event::Typed);
        let generation = m.generation();
        assert_eq!(fx, vec![Effect::Reread { generation }]);
        m.apply(Event::Reread {
            generation,
            reading: Some(reading(before)),
        })
    }

    #[test]
    fn prefix_matching() {
        assert_eq!(advance("Hi", " there", "Hi"), Match::Same);
        assert_eq!(advance("Hi", " there", "Hi th"), Match::Advanced(3));
        assert_eq!(advance("Hi", " there", "Hi there"), Match::Advanced(6));
        assert_eq!(advance("Hi", " there", "Hi tx"), Match::Diverged);
        // Deleting into the anchor, or typing past what has streamed so far, diverges.
        assert_eq!(advance("Hi", " there", "H"), Match::Diverged);
        assert_eq!(advance("Hi", " th", "Hi the"), Match::Diverged);
        // Case matters (the inserted text must be exactly what is shown).
        assert_eq!(advance("Hi", " there", "Hi T"), Match::Diverged);
        assert_eq!(advance("", "café au lait", "café"), Match::Advanced(5));
    }

    #[test]
    fn words() {
        assert_eq!(next_word(" the deck by Friday."), " the");
        assert_eq!(next_word("deck, by"), "deck,");
        assert_eq!(next_word("Friday."), "Friday.");
        assert_eq!(next_word("  "), "  ");
        assert_eq!(next_word("明日送ります。"), "明日送ります。");
    }

    #[test]
    fn typing_debounces_and_a_pending_request_goes_stale() {
        let mut m = Machine::new();
        assert_eq!(m.apply(Event::Typed), vec![Effect::Debounce(1)]);
        assert_eq!(m.apply(Event::Typed), vec![Effect::Debounce(2)]);
        m.apply(Event::Request {
            origin: Origin::Auto,
            anchor: anchor("I'll send"),
        });
        let stale = m.generation();
        // The user types again before draft.begin answered.
        assert_eq!(m.apply(Event::Typed), vec![Effect::Debounce(3)]);
        assert!(m.idle());
        // The begin for the old generation comes back: its stream is cancelled at once.
        let fx = m.apply(Event::Started {
            generation: stale,
            stream_id: "old".into(),
        });
        assert_eq!(fx, vec![Effect::CancelStream("old".into())]);
        assert!(!m.visible());
    }

    #[test]
    fn deltas_stream_into_the_view() {
        let mut m = showing(" the");
        let fx = m.apply(Event::Delta {
            stream_id: "s1".into(),
            text: " deck".into(),
        });
        assert_eq!(fx.len(), 1);
        let Effect::Show(v) = &fx[0] else { panic!() };
        assert_eq!(v.text, " the deck");
        assert!(v.streaming && v.words);
        // Another stream's deltas are ignored.
        assert!(
            m.apply(Event::Delta {
                stream_id: "zzz".into(),
                text: "x".into()
            })
            .is_empty()
        );
        let fx = m.apply(Event::Done {
            stream_id: "s1".into(),
            error: false,
        });
        let Effect::Show(v) = &fx[0] else { panic!() };
        assert!(!v.streaming);
    }

    #[test]
    fn typing_the_suggestion_consumes_it_and_anything_else_drops_it() {
        let mut m = showing(" the deck");
        let fx = reread(&mut m, "I'll send th");
        let Effect::Show(v) = &fx[0] else {
            panic!("{fx:?}")
        };
        assert_eq!(v.text, "e deck");
        assert_eq!(
            m.anchor().unwrap().geometry.caret.x,
            120.0,
            "follows the caret"
        );
        // A key that changed nothing (an arrow at the end, Shift) keeps it.
        let fx = reread(&mut m, "I'll send th");
        assert!(matches!(fx[0], Effect::Show(_)));
        // A typo: hidden, the stream cancelled, keys released, a new pause timer.
        let fx = reread(&mut m, "I'll send tx");
        assert_eq!(
            fx,
            vec![
                Effect::CancelStream("s1".into()),
                Effect::SetTapActive(false),
                Effect::Hide,
                Effect::Debounce(1),
            ]
        );
        assert!(!m.visible() && m.idle());
    }

    #[test]
    fn typing_all_of_a_finished_suggestion_hides_it() {
        let mut m = showing(" it");
        m.apply(Event::Done {
            stream_id: "s1".into(),
            error: false,
        });
        let fx = reread(&mut m, "I'll send it");
        assert_eq!(fx, vec![Effect::SetTapActive(false), Effect::Hide]);
        assert!(m.idle());
    }

    #[test]
    fn another_field_or_app_dismisses() {
        let mut m = showing(" the deck");
        m.apply(Event::Typed);
        let mut r = reading("I'll send");
        r.app_key = "com.apple.mail".into();
        let g = m.generation();
        let fx = m.apply(Event::Reread {
            generation: g,
            reading: Some(r),
        });
        assert!(fx.contains(&Effect::Hide));
        let mut m = showing(" the deck");
        let g = m.generation();
        let fx = m.apply(Event::Reread {
            generation: g,
            reading: None,
        });
        assert!(fx.contains(&Effect::CancelStream("s1".into())));
    }

    #[test]
    fn tab_inserts_the_rest_and_cancels_the_stream() {
        let mut m = showing(" the deck");
        reread(&mut m, "I'll send th");
        let g = m.generation();
        let fx = m.apply(Event::Accept);
        assert_eq!(
            fx,
            vec![
                Effect::Accepted {
                    stream_id: "s1".into()
                },
                Effect::CancelStream("s1".into()),
                Effect::SetTapActive(false),
                Effect::Hide,
                Effect::Insert {
                    generation: g,
                    text: "e deck".into(),
                    expect_before: "I'll send th".into(),
                    whole: true,
                },
            ]
        );
        // Our own insertion's key events are ignored while it runs.
        assert!(m.apply(Event::Typed).is_empty());
        assert!(m.apply(Event::Accept).is_empty());
        m.apply(Event::Inserted {
            generation: g,
            ok: true,
        });
        assert!(m.idle());
    }

    #[test]
    fn accept_word_inserts_one_word_and_keeps_the_rest() {
        let mut m = showing(" the deck by Friday.");
        m.apply(Event::Done {
            stream_id: "s1".into(),
            error: false,
        });
        let g = m.generation();
        let fx = m.apply(Event::AcceptWord);
        assert!(matches!(&fx[1], Effect::Show(v) if v.text == " deck by Friday."));
        assert_eq!(
            fx[2],
            Effect::Insert {
                generation: g,
                text: " the".into(),
                expect_before: "I'll send".into(),
                whole: false,
            }
        );
        // While it goes in, keys are ours; then the field is read back.
        assert!(m.apply(Event::Typed).is_empty());
        assert_eq!(
            m.apply(Event::Inserted {
                generation: g,
                ok: true
            }),
            vec![Effect::Reread { generation: g }]
        );
        let fx = m.apply(Event::Reread {
            generation: g,
            reading: Some(reading("I'll send the")),
        });
        assert!(matches!(&fx[0], Effect::Show(v) if v.text == " deck by Friday."));
        // The last word of a finished suggestion is a whole accept.
        for _ in 0..2 {
            m.apply(Event::AcceptWord);
            let g = m.generation();
            m.apply(Event::Inserted {
                generation: g,
                ok: true,
            });
        }
        let fx = m.apply(Event::AcceptWord);
        assert!(fx.contains(&Effect::Hide), "{fx:?}");
        assert!(matches!(
            fx.last(),
            Some(Effect::Insert { text, whole: true, .. }) if text == " Friday."
        ));
    }

    #[test]
    fn esc_dismisses_and_pauses_auto_for_that_text() {
        let mut m = showing(" the deck");
        let fx = m.apply(Event::Dismiss);
        assert_eq!(
            fx,
            vec![
                Effect::CancelStream("s1".into()),
                Effect::SetTapActive(false),
                Effect::Hide
            ]
        );
        assert!(!m.auto_allowed("I'll send"));
        assert!(m.auto_allowed("I'll send it"));
        // Nothing to accept once dismissed.
        assert!(m.apply(Event::Accept).is_empty());
    }

    #[test]
    fn empty_or_failed_suggestions_vanish_quietly() {
        let mut m = Machine::new();
        m.apply(Event::Request {
            origin: Origin::DoubleTap,
            anchor: anchor(""),
        });
        let g = m.generation();
        m.apply(Event::Started {
            generation: g,
            stream_id: "s".into(),
        });
        let fx = m.apply(Event::Done {
            stream_id: "s".into(),
            error: false,
        });
        assert_eq!(fx, vec![Effect::SetTapActive(false), Effect::Hide]);
        let mut m = showing(" x");
        let fx = m.apply(Event::Done {
            stream_id: "s1".into(),
            error: true,
        });
        assert!(fx.contains(&Effect::Hide) && !m.visible());
        // A refused request just returns to idle.
        let mut m = Machine::new();
        m.apply(Event::Request {
            origin: Origin::Auto,
            anchor: anchor("x"),
        });
        let g = m.generation();
        m.apply(Event::Failed { generation: g });
        assert!(m.idle());
    }

    #[test]
    fn click_and_reset_drop_everything() {
        for e in [Event::Click, Event::Reset] {
            let mut m = showing(" the");
            let fx = m.apply(e);
            assert!(fx.contains(&Effect::CancelStream("s1".into())));
            assert!(fx.contains(&Effect::SetTapActive(false)));
            assert!(m.idle());
        }
        // Late events of the dropped request change nothing.
        let mut m = showing(" the");
        let g = m.generation();
        m.apply(Event::Reset);
        assert!(
            m.apply(Event::Reread {
                generation: g,
                reading: Some(reading("I'll send"))
            })
            .is_empty()
        );
    }
}
