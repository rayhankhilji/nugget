//! The user's writing voice, learned from messages they sent.
//!
//! **Contract (M3):** [`VoiceHints`], [`Channel`], [`StyleStats`]. Work package M3-D builds the
//! profile from the store (sent mail, chat messages marked as the user's, draft feedback) and
//! selects exemplars per channel and recipient.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    Chat,
    Comment,
    Document,
    Other,
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
