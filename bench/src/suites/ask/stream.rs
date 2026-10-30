//! One streamed answer: the provider stream through the product's [`AnswerFilter`], with the
//! timings, token counts, prompt-cache hits and stop reason the server reports.

use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::ipc::AnswerCheck;
use nugget_infer::{ChatEvent, ChatRequest, Provider, StopReason};
use nuggetd::ask::AnswerFilter;

use crate::suites::draft::failure_kind;

/// One streamed answer.
#[derive(Debug, Clone, Default)]
pub struct AnswerRun {
    /// The model's own text (what arrived before a failure, for the dump).
    pub raw: String,
    /// What the filter forwarded (what the user sees).
    pub text: String,
    /// The product's post-check; `None` for a failed request.
    pub check: Option<AnswerCheck>,
    /// Model time to first token: from the request (retrieval excluded) to the first non-empty
    /// text the filter forwards; `None` when it forwarded nothing.
    pub ttft: Option<Duration>,
    /// From the request to the end of the stream (the timeout for a timed-out request).
    pub total: Duration,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Prompt tokens served from the server's cache (`ChatEvent::Stats`).
    pub cached_tokens: Option<u32>,
    /// `end_turn`, `max_tokens`, ...
    pub stop_reason: Option<String>,
    /// `(kind, message)`: `timeout`, `context_overflow` or `error`.
    pub failure: Option<(&'static str, String)>,
}

impl AnswerRun {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }

    pub fn timed_out(&self) -> bool {
        matches!(self.failure, Some(("timeout", _)))
    }

    /// The answer stopped at the output cap.
    pub fn length_stop(&self) -> bool {
        self.stop_reason.as_deref() == Some("max_tokens")
    }
}

/// The report spelling of a stop reason.
pub fn stop_name(r: &StopReason) -> String {
    match r {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::StopSequence => "stop_sequence".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::Refusal => "refusal".into(),
        StopReason::ContentFilter => "content_filter".into(),
        StopReason::Other(s) => s.clone(),
    }
}

/// Stream `req` from `provider` through `filter` within `timeout`; times from `started` (just
/// before the request). A timeout or an error becomes [`AnswerRun::failure`], never a panic.
pub async fn stream_answer(
    provider: &dyn Provider,
    req: ChatRequest,
    filter: &mut AnswerFilter,
    timeout: Duration,
    started: Instant,
) -> AnswerRun {
    let mut run = AnswerRun::default();
    let result = {
        let r = &mut run;
        let f = &mut *filter;
        let work = async move {
            let mut stream = provider.chat(req).await?;
            while let Some(ev) = stream.next().await {
                match ev? {
                    ChatEvent::TextDelta(d) => {
                        r.raw.push_str(&d);
                        let out = f.push(&d);
                        if r.ttft.is_none() && !out.trim().is_empty() {
                            r.ttft = Some(started.elapsed());
                        }
                        r.text.push_str(&out);
                    }
                    ChatEvent::Stats(s) => {
                        r.input_tokens = r.input_tokens.or(s.input_tokens);
                        r.output_tokens = r.output_tokens.or(s.output_tokens);
                        r.cached_tokens = r.cached_tokens.or(s.cached_input_tokens);
                    }
                    ChatEvent::Done {
                        stop_reason,
                        input_tokens,
                        output_tokens,
                    } => {
                        r.input_tokens = input_tokens.or(r.input_tokens);
                        r.output_tokens = output_tokens.or(r.output_tokens);
                        r.stop_reason = Some(stop_name(&stop_reason));
                        break;
                    }
                    _ => {}
                }
            }
            Ok::<_, nugget_core::Error>(())
        };
        tokio::time::timeout(timeout, work).await
    };
    match result {
        Err(_) => {
            run.failure = Some((
                "timeout",
                format!("no complete answer within {} s", timeout.as_secs()),
            ));
            run.total = timeout;
            return run;
        }
        Ok(Err(e)) => {
            let m = e.to_string();
            run.failure = Some((failure_kind(&m), m));
        }
        Ok(Ok(())) => {
            let tail = filter.finish();
            if run.ttft.is_none() && !tail.trim().is_empty() {
                run.ttft = Some(started.elapsed());
            }
            run.text.push_str(&tail);
            run.check = Some(filter.check());
        }
    }
    run.total = started.elapsed();
    run
}
