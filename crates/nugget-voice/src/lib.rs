//! The user's writing voice, learned from messages they sent.
//!
//! **Contract (M3):** [`VoiceHints`], [`Channel`], [`StyleStats`]. Work package M3-D builds the
//! profile from the store (sent mail, chat messages marked as the user's, draft feedback) and
//! selects exemplars per channel and recipient.
//!
//! # Overview
//! - [`MeIdentity`]: who "me" is. Configured addresses and names, plus inference from the most
//!   frequent `From` address of sent mail (Gmail `SENT` label, an IMAP Sent folder, a `Sent`
//!   tag from `X-Gmail-Labels`).
//! - [`VoiceProfile::build`] reads the user's own messages from the store: sent mail bodies
//!   (the main text only; `nugget-extract` moves quoted replies and signatures into metadata,
//!   and leftovers are stripped again here), chat messages flagged `from_me`, Markdown notes
//!   and draft feedback recorded by [`StoreFeedbackSink`]. [`VoiceProfile::refresh`] is
//!   incremental: only items whose connector version changed are read again.
//! - [`StyleStats`] per channel and per frequent recipient (see [`stats`] for definitions).
//! - [`VoiceSource::hints`] returns stats plus 3–6 short exemplars, chosen by recipient, then
//!   channel, then topic similarity to the current thread (hybrid store search restricted to
//!   the user's own items, plus a local term cosine), biased to recent messages.
//!   [`VoiceQuery::exclude_sensitive`] keeps sensitive items out of exemplars sent to cloud
//!   models.
//! - [`style_distance`] and [`similarity`] score how close a text is to a style (`bench draft`).

mod feedback;
mod me;
mod profile;
pub mod stats;
mod text;

use std::sync::{Arc, PoisonError, RwLock};

use serde::{Deserialize, Serialize};

pub use feedback::{
    FEEDBACK_KIND, FEEDBACK_SOURCE, FeedbackContext, FeedbackSink, StoreFeedbackSink,
};
pub use me::{MeIdentity, parse_address};
pub use nugget_core::ipc::VoiceStatus;
pub use profile::{RefreshReport, VOICE_CURSOR_SOURCE, VoiceConfig, VoiceProfile};
pub use stats::{similarity, style_distance};
pub use text::{normalize_greeting, normalize_signoff};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    Chat,
    Comment,
    Document,
    Other,
}

impl Channel {
    /// The snake_case name used in IPC (`email`, `chat`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Email => "email",
            Channel::Chat => "chat",
            Channel::Comment => "comment",
            Channel::Document => "document",
            Channel::Other => "other",
        }
    }

    /// Parse a snake_case name; unknown names are `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "email" => Channel::Email,
            "chat" => Channel::Chat,
            "comment" => Channel::Comment,
            "document" => Channel::Document,
            "other" => Channel::Other,
            _ => return None,
        })
    }
}

/// Measured habits, used both to prompt the model and to score drafts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StyleStats {
    pub samples: u32,
    pub median_words: f32,
    pub greetings: Vec<(String, f32)>,
    pub signoffs: Vec<(String, f32)>,
    /// 0 = casual, 1 = formal.
    pub formality: f32,
    pub emoji_per_message: f32,
    pub exclamations_per_message: f32,
    pub lowercase_start_rate: f32,
    pub languages: Vec<(String, f32)>,
}

/// What a draft prompt gets from the voice profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceHints {
    pub channel: Option<Channel>,
    pub stats: StyleStats,
    /// A few of the user's own past messages, most relevant first (recipient, topic).
    pub exemplars: Vec<String>,
}

/// A request for [`VoiceHints`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceQuery {
    pub channel: Channel,
    /// Recipients of the draft: addresses (`Sam Lee <sam@example.com>` or `sam@example.com`) or
    /// chat handles.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Text of the conversation being answered (used for topic similarity).
    #[serde(default)]
    pub thread_text: String,
    /// Embedding of `thread_text` from the store's embedder, for hybrid retrieval. Text-only
    /// search is used without it.
    #[serde(default)]
    pub thread_embedding: Option<Vec<f32>>,
    /// Leave out exemplars from sensitive items. **Set it whenever the prompt goes to a cloud
    /// model.** Defaults to `true`.
    #[serde(default = "default_true")]
    pub exclude_sensitive: bool,
}

fn default_true() -> bool {
    true
}

impl VoiceQuery {
    /// A query for `channel` that excludes sensitive exemplars.
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            recipients: Vec::new(),
            thread_text: String::new(),
            thread_embedding: None,
            exclude_sensitive: true,
        }
    }

    pub fn with_recipients<S: Into<String>>(mut self, r: impl IntoIterator<Item = S>) -> Self {
        self.recipients = r.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_thread(mut self, text: impl Into<String>) -> Self {
        self.thread_text = text.into();
        self
    }

    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.thread_embedding = Some(embedding);
        self
    }

    /// Whether sensitive exemplars may be used (only for on-device models).
    pub fn allow_sensitive(mut self, allow: bool) -> Self {
        self.exclude_sensitive = !allow;
        self
    }
}

/// What the drafter consumes: voice hints for a draft, and the profile's status.
pub trait VoiceSource: Send + Sync {
    fn hints(&self, query: &VoiceQuery) -> VoiceHints;
    fn status(&self) -> VoiceStatus;
}

impl<T: VoiceSource + ?Sized> VoiceSource for Arc<T> {
    fn hints(&self, query: &VoiceQuery) -> VoiceHints {
        (**self).hints(query)
    }
    fn status(&self) -> VoiceStatus {
        (**self).status()
    }
}

/// A profile shared with a background refresher.
impl<T: VoiceSource> VoiceSource for RwLock<T> {
    fn hints(&self, query: &VoiceQuery) -> VoiceHints {
        self.read()
            .unwrap_or_else(PoisonError::into_inner)
            .hints(query)
    }
    fn status(&self) -> VoiceStatus {
        self.read().unwrap_or_else(PoisonError::into_inner).status()
    }
}

/// Fixed hints, for tests and for running without a profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticVoice(pub VoiceHints);

impl VoiceSource for StaticVoice {
    fn hints(&self, query: &VoiceQuery) -> VoiceHints {
        VoiceHints {
            channel: Some(query.channel),
            ..self.0.clone()
        }
    }
    fn status(&self) -> VoiceStatus {
        VoiceStatus {
            samples: u64::from(self.0.stats.samples),
            channels: Vec::new(),
            last_built: None,
        }
    }
}
