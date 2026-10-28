//! Adapters from `nugget-voice` (synchronous, store-backed) to the engine's async traits.
//!
//! - [`ProfileVoice`] serves [`crate::VoiceSource`] from any [`nugget_voice::VoiceSource`], for
//!   example an `Arc<RwLock<VoiceProfile>>` that the daemon refreshes in the background.
//! - [`VoiceFeedback`] passes draft outcomes to a [`nugget_voice::FeedbackSink`], for example
//!   [`nugget_voice::StoreFeedbackSink`], which keeps final texts in the encrypted store.
//!
//! Both run the synchronous call on a blocking thread (`tokio::task::spawn_blocking`), so a
//! profile lookup (store search) or a store write never stalls the async runtime.

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
    use std::sync::{Mutex, RwLock};

    use nugget_voice::{Channel, StaticVoice, StyleStats, VoiceProfile};

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

    #[test]
    fn query_maps_cloud_to_exclude_sensitive() {
        let q = profile_query(&query(true));
        assert_eq!(q.channel, Channel::Email);
        assert_eq!(q.recipients, vec!["Sam Lee <sam@acme.example>"]);
        assert_eq!(q.thread_text, "offsite venue");
        assert!(q.exclude_sensitive);
        assert!(!profile_query(&query(false)).exclude_sensitive);
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
    async fn profile_voice_serves_hints() {
        let hints = VoiceHints {
            channel: None,
            stats: StyleStats {
                samples: 7,
                median_words: 12.0,
                ..Default::default()
            },
            exemplars: vec!["hey Sam! sounds good".into()],
        };
        let rec = Arc::new(Recording(Mutex::new(Vec::new()), StaticVoice(hints)));
        let voice = ProfileVoice::from_arc(rec.clone());
        let h = VoiceSource::hints(&voice, &query(true)).await.unwrap();
        assert_eq!(h.channel, Some(Channel::Email));
        assert_eq!(h.stats.samples, 7);
        assert_eq!(h.exemplars, vec!["hey Sam! sounds good"]);
        assert_eq!(voice.status().samples, 7);
        let seen = rec.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].exclude_sensitive);
        assert_eq!(seen[0].thread_text, "offsite venue");

        // Usable as the engine's trait object.
        let dynamic: Arc<dyn VoiceSource> = Arc::new(ProfileVoice::new(StaticVoice::default()));
        let h = dynamic.hints(&query(false)).await.unwrap();
        assert_eq!(h.channel, Some(Channel::Email));
    }

    #[tokio::test]
    async fn profile_voice_over_a_shared_refreshable_profile() {
        let store = nugget_store::Store::open_in_memory().unwrap();
        let profile = VoiceProfile::build(
            &store,
            nugget_voice::MeIdentity::default(),
            nugget_voice::VoiceConfig::default(),
        )
        .unwrap();
        let shared = Arc::new(RwLock::new(profile));
        let voice: ProfileVoice<Arc<RwLock<VoiceProfile>>> = ProfileVoice::new(shared.clone());
        let h = VoiceSource::hints(&voice, &query(true)).await.unwrap();
        assert_eq!(h.channel, Some(Channel::Email));
        assert!(h.exemplars.is_empty());
        // The background refresher takes the write lock between drafts.
        shared.write().unwrap().refresh().unwrap();
        assert_eq!(voice.status().samples, 0);
        let _: Arc<dyn VoiceSource> = Arc::new(voice);
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<(DraftFeedback, FeedbackContext)>>);

    impl nugget_voice::FeedbackSink for Sink {
        fn record(&self, feedback: &DraftFeedback, ctx: &FeedbackContext) -> Result<bool> {
            self.0.lock().unwrap().push((feedback.clone(), ctx.clone()));
            Ok(true)
        }
    }

    fn record(
        outcome: FeedbackOutcome,
        final_text: Option<&str>,
        sensitive: bool,
    ) -> FeedbackRecord {
        FeedbackRecord {
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
        }
    }

    #[tokio::test]
    async fn feedback_maps_records() {
        let sink = Arc::new(Sink::default());
        let fb: Arc<dyn FeedbackSink> = Arc::new(VoiceFeedback::from_arc(sink.clone()));
        fb.record(record(
            FeedbackOutcome::Edited,
            Some("Thursday works, see you at 3."),
            false,
        ))
        .await
        .unwrap();
        fb.record(record(FeedbackOutcome::Accepted, None, true))
            .await
            .unwrap();
        fb.record(record(FeedbackOutcome::Rejected, None, false))
            .await
            .unwrap();
        let got = sink.0.lock().unwrap().clone();
        assert_eq!(got.len(), 3);
        let (d, c) = &got[0];
        assert_eq!(d.stream_id, "s1");
        assert_eq!(d.outcome, "edited");
        assert_eq!(
            d.final_text.as_deref(),
            Some("Thursday works, see you at 3.")
        );
        assert_eq!(d.insert_verified, Some(true));
        assert_eq!(c.channel, Channel::Chat);
        assert_eq!(c.recipients, vec!["jamie"]);
        assert!(!c.sensitive);
        // Accepted unchanged: the draft is what was sent.
        assert_eq!(got[1].0.outcome, "accepted");
        assert_eq!(got[1].0.final_text.as_deref(), Some("Thursday works."));
        assert!(got[1].1.sensitive);
        // Rejected: no final text.
        assert_eq!(got[2].0.outcome, "rejected");
        assert_eq!(got[2].0.final_text, None);
    }

    #[tokio::test]
    async fn feedback_reaches_the_store_sink_and_profile() {
        let store = nugget_store::Store::open_in_memory().unwrap();
        let fb = VoiceFeedback::new(nugget_voice::StoreFeedbackSink::new(store.clone()));
        FeedbackSink::record(
            &fb,
            record(FeedbackOutcome::Edited, Some("sure jamie, 5 works"), false),
        )
        .await
        .unwrap();
        let p = VoiceProfile::build(
            &store,
            nugget_voice::MeIdentity::default(),
            nugget_voice::VoiceConfig::default(),
        )
        .unwrap();
        assert_eq!(p.status().samples, 1);
    }

    #[tokio::test]
    async fn sink_errors_propagate() {
        struct Failing;
        impl nugget_voice::FeedbackSink for Failing {
            fn record(&self, _: &DraftFeedback, _: &FeedbackContext) -> Result<bool> {
                Err(Error::Invalid("no".into()))
            }
        }
        let fb = VoiceFeedback::new(Failing);
        let r = FeedbackSink::record(&fb, record(FeedbackOutcome::Accepted, None, false)).await;
        assert!(matches!(r, Err(Error::Invalid(_))));
    }
}
