//! `nugget_proactive::Drafter` backed by the Double Tap engine (`nugget-draft`): the same
//! provider choice, budgeted retrieval with the sensitive gate, voice and post-processing as a
//! draft typed into a mail field. The card's evidence quotes are passed as the thread, which the
//! engine fences as untrusted data.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nugget_core::ipc::{
    AppInfo, DraftContext, DraftRequest as FieldDraft, FieldInfo, ThreadMessage,
};
use nugget_core::{Error, Result};
use nugget_infer::ChatEvent;
use nugget_proactive::{DraftRequest, Drafter};

/// Longest a proactive draft may take end to end.
const DRAFT_TIMEOUT: Duration = Duration::from_secs(180);
/// Evidence quotes passed as the thread at most.
const MAX_THREAD: usize = 12;

pub(crate) struct EngineDrafter {
    engine: nugget_draft::Drafter,
}

impl EngineDrafter {
    pub(crate) fn new(engine: nugget_draft::Drafter) -> Self {
        Self { engine }
    }
}

/// The field context a mail reply would have: an email body in a mail app, with subject,
/// recipients and the evidence as the thread.
pub(crate) fn field_request(req: &DraftRequest) -> FieldDraft {
    let thread = req
        .context
        .iter()
        .filter(|q| !q.trim().is_empty())
        .take(MAX_THREAD)
        .map(|q| ThreadMessage {
            author: None,
            text: q.clone(),
            from_me: Some(false),
            timestamp: None,
        })
        .collect();
    let intent = req.intent.trim();
    FieldDraft {
        context: DraftContext {
            app: AppInfo {
                id: None,
                name: "Mail".into(),
                window_title: None,
                url: None,
            },
            field: FieldInfo {
                role: "text_area".into(),
                label: Some("Message body".into()),
                ..Default::default()
            },
            thread,
            recipients: req.to.clone(),
            subject: Some(req.subject.clone()),
            screenshot_png_base64: None,
        },
        instruction: (!intent.is_empty()).then(|| intent.to_owned()),
        provider: None,
        max_tokens: None,
    }
}

#[async_trait]
impl Drafter for EngineDrafter {
    async fn draft_reply(&self, req: &DraftRequest) -> Result<String> {
        let run = async {
            let (_, mut stream) = self.engine.begin(field_request(req)).await?;
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                match ev? {
                    ChatEvent::TextDelta(t) => text.push_str(&t),
                    ChatEvent::Done { .. } => return Ok(text),
                    _ => {}
                }
            }
            Err(Error::Provider(
                "the draft ended before it was complete".into(),
            ))
        };
        let text = tokio::time::timeout(DRAFT_TIMEOUT, run)
            .await
            .map_err(|_| Error::Provider("drafting the reply timed out".into()))??;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(Error::Provider(
                "the drafter returned an empty reply".into(),
            ));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_draft::FieldKind;

    #[test]
    fn the_field_context_is_an_email_reply() {
        let req = DraftRequest {
            reply_to: ItemRef {
                source: SourceId::new("mail"),
                key: ItemKey::new("1"),
            },
            to: vec!["ana@acme.io".into()],
            subject: "Re: Pricing".into(),
            intent: "Send the pricing sheet".into(),
            context: vec!["Could you send pricing?".into(), " ".into()],
        };
        let f = field_request(&req);
        assert_eq!(
            nugget_draft::gate(&f.context, &[]).unwrap(),
            FieldKind::EmailBody
        );
        assert_eq!(f.context.thread.len(), 1);
        assert_eq!(f.context.recipients, vec!["ana@acme.io".to_string()]);
        assert_eq!(f.instruction.as_deref(), Some("Send the pricing sheet"));
    }
}
