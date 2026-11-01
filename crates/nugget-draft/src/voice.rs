//! Adapters from `nugget-voice` (synchronous, store-backed) to the engine's async traits, run on
//! a blocking thread.

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::ipc::{DraftFeedback, VoiceStatus};
use nugget_core::{Error, Result};
use nugget_voice::{FeedbackContext, VoiceHints};

use crate::engine::{FeedbackOutcome, FeedbackRecord, FeedbackSink, VoiceQuery, VoiceSource};

/// The engine's [`VoiceQuery`] as `nugget-voice` asks it: `topic` becomes the thread text,
/// and a draft for a cloud provider excludes sensitive exemplars.
pub fn profile_query(query: &VoiceQuery) -> nugget_voice::VoiceQuery {
    nugget_voice::VoiceQuery::new(query.channel)
        .with_recipients(query.recipients.iter().cloned())
        .with_thread(query.topic.clone())
        .allow_sensitive(!query.for_cloud)
}

/// [`crate::VoiceSource`] over a synchronous [`nugget_voice::VoiceSource`].
pub struct ProfileVoice<T> {
    source: Arc<T>,
}

impl<T> ProfileVoice<T> {
    pub fn new(source: T) -> Self {
        Self {
            source: Arc::new(source),
        }
    }

    /// Share a source that is also used elsewhere (e.g. refreshed by a background task).
    pub fn from_arc(source: Arc<T>) -> Self {
        Self { source }
    }

    pub fn source(&self) -> &Arc<T> {
        &self.source
    }
}

impl<T: nugget_voice::VoiceSource> ProfileVoice<T> {
    /// The profile's status (synchronous; cheap for `VoiceProfile`).
    pub fn status(&self) -> VoiceStatus {
        self.source.status()
    }
}

impl<T> Clone for ProfileVoice<T> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
        }
    }
}

impl<T> std::fmt::Debug for ProfileVoice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileVoice").finish_non_exhaustive()
    }
}

#[async_trait]
impl<T: nugget_voice::VoiceSource + 'static> VoiceSource for ProfileVoice<T> {
    async fn hints(&self, query: &VoiceQuery) -> Result<VoiceHints> {
        let q = profile_query(query);
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || source.hints(&q))
            .await
            .map_err(|e| Error::Other(format!("voice lookup failed: {e}")))
    }
}

/// [`crate::FeedbackSink`] over a synchronous [`nugget_voice::FeedbackSink`].
///
/// A draft accepted unchanged without a `final_text` is recorded with the draft as its final
/// text (it is what was inserted). A record whose draft drew on sensitive passages is stored as
/// sensitive, so it never becomes an exemplar for cloud models.
pub struct VoiceFeedback<S> {
    sink: Arc<S>,
}

impl<S> VoiceFeedback<S> {
    pub fn new(sink: S) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }

    pub fn from_arc(sink: Arc<S>) -> Self {
        Self { sink }
    }
}

impl<S> Clone for VoiceFeedback<S> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<S> std::fmt::Debug for VoiceFeedback<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceFeedback").finish_non_exhaustive()
    }
}

/// A [`FeedbackRecord`] as `nugget-voice` takes it.
pub fn feedback_parts(record: FeedbackRecord) -> (DraftFeedback, FeedbackContext) {
    let final_text = match (record.outcome, record.final_text) {
        (_, Some(t)) => Some(t),
        (FeedbackOutcome::Accepted, None) if !record.draft.trim().is_empty() => Some(record.draft),
        _ => None,
    };
    (
        DraftFeedback {
            stream_id: record.stream_id,
            outcome: record.outcome.as_str().to_owned(),
            final_text,
            insert_verified: record.insert_verified,
        },
        FeedbackContext {
            channel: record.channel,
            recipients: record.recipients,
            subject: None,
            sensitive: record.sensitive,
        },
    )
}

#[async_trait]
impl<S: nugget_voice::FeedbackSink + 'static> FeedbackSink for VoiceFeedback<S> {
    async fn record(&self, record: FeedbackRecord) -> Result<()> {
        let (fb, ctx) = feedback_parts(record);
        let sink = self.sink.clone();
        tokio::task::spawn_blocking(move || sink.record(&fb, &ctx))
            .await
            .map_err(|e| Error::Other(format!("voice feedback failed: {e}")))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use nugget_voice::{Channel, StaticVoice, StyleStats};

    use super::*;
    use crate::FieldKind;

    fn query(for_cloud: bool) -> VoiceQuery {
        VoiceQuery {
            channel: Channel::Email,
            kind: FieldKind::EmailBody,
            app_id: Some("com.apple.mail".into()),
            recipients: vec!["Sam Lee <sam@acme.example>".into()],
            topic: "offsite venue".into(),
            for_cloud,
        }
    }

    /// Records every query, answers with fixed hints.
    struct Recording(Mutex<Vec<nugget_voice::VoiceQuery>>, StaticVoice);

    impl nugget_voice::VoiceSource for Recording {
        fn hints(&self, query: &nugget_voice::VoiceQuery) -> VoiceHints {
            self.0.lock().unwrap().push(query.clone());
            self.1.hints(query)
        }
        fn status(&self) -> VoiceStatus {
            self.1.status()
        }
    }

    #[tokio::test]
    async fn profile_voice_serves_hints_and_cloud_excludes_sensitive() {
        assert!(!profile_query(&query(false)).exclude_sensitive);
        let hints = VoiceHints {
            channel: None,
            stats: StyleStats {
                samples: 7,
                ..Default::default()
            },
            exemplars: vec!["hey Sam! sounds good".into()],
        };
        let rec = Arc::new(Recording(Mutex::new(Vec::new()), StaticVoice(hints)));
        let voice = ProfileVoice::from_arc(rec.clone());
        let h = VoiceSource::hints(&voice, &query(true)).await.unwrap();
        assert_eq!(h.channel, Some(Channel::Email));
        assert_eq!(h.exemplars, vec!["hey Sam! sounds good"]);
        assert_eq!(voice.status().samples, 7);
        let seen = rec.0.lock().unwrap().clone();
        assert!(seen[0].exclude_sensitive);
        assert_eq!(seen[0].thread_text, "offsite venue");
        assert_eq!(seen[0].recipients, vec!["Sam Lee <sam@acme.example>"]);
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<(DraftFeedback, FeedbackContext)>>);

    impl nugget_voice::FeedbackSink for Sink {
        fn record(&self, feedback: &DraftFeedback, ctx: &FeedbackContext) -> Result<bool> {
            self.0.lock().unwrap().push((feedback.clone(), ctx.clone()));
            Ok(true)
        }
    }

    #[tokio::test]
    async fn feedback_maps_records() {
        let sink = Arc::new(Sink::default());
        let fb: Arc<dyn FeedbackSink> = Arc::new(VoiceFeedback::from_arc(sink.clone()));
        let record = |outcome, final_text: Option<&str>, sensitive| FeedbackRecord {
            stream_id: "s1".into(),
            outcome,
            field_kind: FieldKind::ChatMessage,
            channel: Channel::Chat,
            app_id: Some("com.tinyspeck.slackmacgap".into()),
            recipients: vec!["jamie".into()],
            draft: "Thursday works.".into(),
            final_text: final_text.map(str::to_owned),
            insert_verified: Some(true),
            sensitive,
        };
        for r in [
            record(
                FeedbackOutcome::Edited,
                Some("Thursday works, see you at 3."),
                false,
            ),
            record(FeedbackOutcome::Accepted, None, true),
            record(FeedbackOutcome::Rejected, None, false),
        ] {
            fb.record(r).await.unwrap();
        }
        let got = sink.0.lock().unwrap().clone();
        let (d, c) = &got[0];
        assert_eq!(
            (d.outcome.as_str(), d.insert_verified),
            ("edited", Some(true))
        );
        assert_eq!(
            d.final_text.as_deref(),
            Some("Thursday works, see you at 3.")
        );
        assert_eq!(
            (c.channel, c.recipients.clone(), c.sensitive),
            (Channel::Chat, vec!["jamie".into()], false)
        );
        // Accepted unchanged: the draft is what was sent, and stays marked sensitive.
        assert_eq!(got[1].0.final_text.as_deref(), Some("Thursday works."));
        assert!(got[1].1.sensitive);
        assert_eq!(
            (got[2].0.outcome.as_str(), got[2].0.final_text.clone()),
            ("rejected", None)
        );
    }
}
