//! Draft feedback: the text the user finally sent, kept (encrypted, in the store) as a sample
//! of their voice.

use nugget_core::Result;
use nugget_core::ipc::DraftFeedback;
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use crate::Channel;

/// Source id of feedback items in the store.
pub const FEEDBACK_SOURCE: &str = nugget_core::ids::VOICE_SAMPLES_SOURCE;
/// `metadata.kind` of feedback items.
pub const FEEDBACK_KIND: &str = "voice_feedback";
/// Longest final text kept (characters).
const MAX_FEEDBACK_CHARS: usize = 20_000;
/// Chunk size for feedback text (characters), so it is searchable for exemplar retrieval.
const CHUNK_CHARS: usize = 1_500;

/// Where a draft went, known to the drafter but not part of [`DraftFeedback`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackContext {
    pub channel: Channel,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// The draft drew on sensitive context, or the field is sensitive: the stored sample is
    /// marked sensitive and never used as an exemplar for cloud models.
    #[serde(default)]
    pub sensitive: bool,
}

/// Receives draft feedback (implemented by the voice profile's store-backed sink).
pub trait FeedbackSink: Send + Sync {
    /// Record `feedback`. Returns whether a sample was stored: only `accepted` and `edited`
    /// outcomes with a non-empty `final_text` are kept.
    fn record(&self, feedback: &DraftFeedback, ctx: &FeedbackContext) -> Result<bool>;
}

/// Stores final texts as items of source [`FEEDBACK_SOURCE`] in the (encrypted) store, with
/// one text chunk per ~1,500 characters. [`crate::VoiceProfile::refresh`] picks them up.
#[derive(Debug, Clone)]
pub struct StoreFeedbackSink {
    store: Store,
}

impl StoreFeedbackSink {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

impl FeedbackSink for StoreFeedbackSink {
    fn record(&self, feedback: &DraftFeedback, ctx: &FeedbackContext) -> Result<bool> {
        if !matches!(feedback.outcome.as_str(), "accepted" | "edited") {
            return Ok(false);
        }
        let Some(text) = feedback
            .final_text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            return Ok(false);
        };
        let text: String = text.chars().take(MAX_FEEDBACK_CHARS).collect();
        if feedback.stream_id.trim().is_empty() {
            return Err(nugget_core::Error::Invalid(
                "draft feedback without a stream id".into(),
            ));
        }
        let hash = content_hash(&text);
        let mut item = NewItem::new(
            FEEDBACK_SOURCE,
            format!("feedback/{}", feedback.stream_id),
            hash.clone(),
        )
        .with_mime("text/plain")
        .with_text(text.clone())
        .with_version(hash)
        .with_modified_at(OffsetDateTime::now_utc())
        .with_metadata(json!({
            "kind": FEEDBACK_KIND,
            "from_me": true,
            "channel": ctx.channel.as_str(),
            "recipients": ctx.recipients,
            "outcome": feedback.outcome,
            "insert_verified": feedback.insert_verified,
            "sensitive": ctx.sensitive,
            "junk": false,
        }));
        if let Some(s) = ctx.subject.as_deref().filter(|s| !s.trim().is_empty()) {
            item = item.with_title(s.trim());
        }
        self.store.upsert_item_with_chunks(&item, &chunk(&text))?;
        Ok(true)
    }
}

fn chunk(text: &str) -> Vec<NewChunk> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for para in text.split("\n\n") {
        if !cur.is_empty() && cur.chars().count() + para.chars().count() > CHUNK_CHARS {
            out.push(NewChunk::text(std::mem::take(&mut cur)));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    if !cur.trim().is_empty() {
        out.push(NewChunk::text(cur));
    }
    out
}
