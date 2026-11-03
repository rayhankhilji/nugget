//! What the desktop shell's `cloud_*` commands do, without Tauri: the input checks (shared with
//! `apps/desktop/src-tauri/src/cloud.rs`, which calls these) and [`dispatch`], which runs one
//! command against an [`Api`] and says what the shell would do next ([`Effect`]: open a page,
//! tell the page the session changed, hand nuggetd a fresh token).
//!
//! The desktop uses the checks; `dispatch` exists so the cloud screens can be driven end to end
//! without the webview shell (the `e2e_shell` example, used by `apps/desktop/e2e`), through the
//! same client code the app ships.

use serde_json::{Value, json};

use crate::{
    ActionKind, ActionStatus, Api, AskRequest, CardAction, CloudError, DismissReason, Interval,
    ModelTier, ProposeAction, Provider, SearchRequest,
};

pub const MAX_QUESTION_LEN: usize = 4_000;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;
/// Serialized size of an action's input.
pub const MAX_ACTION_INPUT_BYTES: usize = 256 * 1024;
/// An RFC 3339 time is about 30 characters; this leaves room and stops abuse.
pub const MAX_TIME_LEN: usize = 64;
pub const MAX_QUERY_LEN: usize = 500;
pub const MAX_SEEN: usize = 200;
pub const MAX_REPLY_LEN: usize = 100_000;

type R<T> = Result<T, CloudError>;

fn invalid<T>(m: impl Into<String>) -> R<T> {
    Err(CloudError::Invalid(m.into()))
}

/// A Composio toolkit slug (`gmail`, `googlecalendar`).
pub fn check_toolkit(toolkit: &str) -> R<()> {
    let ok = !toolkit.is_empty()
        && toolkit.len() <= 64
        && toolkit
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(())
    } else {
        invalid("not a known app")
    }
}

pub fn check_action_kind(kind: &str) -> R<ActionKind> {
    ActionKind::parse(kind)
}

pub fn check_action_status(status: &str) -> R<ActionStatus> {
    ActionStatus::parse(status)
}

pub fn check_action_id(id: &str) -> R<()> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return invalid("not a valid action");
    }
    Ok(())
}

/// The approval echoes the hex SHA-256 the server showed with the preview.
pub fn check_digest(digest: &str) -> R<()> {
    let ok = digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        invalid("not a valid action digest")
    }
}

pub fn check_idempotency_key(key: &str) -> R<()> {
    let ok = !key.is_empty()
        && key.len() <= MAX_IDEMPOTENCY_KEY_LEN
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-:.".contains(c));
    if ok {
        Ok(())
    } else {
        invalid("not a valid idempotency key")
    }
}

pub fn check_action_input(input: &Value) -> R<()> {
    if !input.is_object() {
        return invalid("an action's input must be an object");
    }
    let size = serde_json::to_vec(input).map_or(usize::MAX, |v| v.len());
    if size > MAX_ACTION_INPUT_BYTES {
        return invalid("the action is too large");
    }
    Ok(())
}

/// An edit: a small object of fields some action kind lets the user change (the server
/// enforces which ones apply to this action's kind).
pub fn check_action_edit(input: &Value) -> R<()> {
    check_action_input(input)?;
    let fields = input.as_object().map(|o| o.keys()).into_iter().flatten();
    for field in fields {
        if !ActionKind::any_editable(field) {
            return invalid(format!("{field} can't be edited"));
        }
    }
    Ok(())
}

/// A snooze end: an RFC 3339 time.
pub fn check_snooze_until(until: &str) -> R<()> {
    let ok = until.len() <= MAX_TIME_LEN
        && time::OffsetDateTime::parse(until, &time::format_description::well_known::Rfc3339)
            .is_ok();
    if ok {
        Ok(())
    } else {
        invalid("not a valid time")
    }
}

/// A question for Ask, trimmed; and its conversation id and model tier.
pub fn check_question(question: &str) -> R<String> {
    let q = question.trim().to_owned();
    if q.is_empty() || q.chars().count() > MAX_QUESTION_LEN {
        return invalid("ask a question (up to 4,000 characters)");
    }
    Ok(q)
}

pub fn check_conversation(id: &Option<String>) -> R<()> {
    if id
        .as_ref()
        .is_some_and(|c| c.is_empty() || c.len() > MAX_ID_LEN)
    {
        return invalid("not a valid conversation");
    }
    Ok(())
}

pub fn check_model(model: Option<&str>) -> R<Option<ModelTier>> {
    match model {
        None => Ok(None),
        Some("fast") => Ok(Some(ModelTier::Fast)),
        Some("smart") => Ok(Some(ModelTier::Smart)),
        Some(_) => invalid("not a known model"),
    }
}

pub fn check_connection_id(id: &str) -> R<()> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return invalid("not a valid connection");
    }
    Ok(())
}

pub fn check_tz(tz: &Option<String>) -> R<()> {
    let ok = tz.as_deref().is_none_or(|t| {
        !t.is_empty()
            && t.len() <= 64
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c))
    });
    if ok {
        Ok(())
    } else {
        invalid("not a time zone")
    }
}

/// A card id, as the cloud issues them (short hex, from the card's dedup key).
pub fn check_card_id(id: &str) -> R<()> {
    let ok =
        !id.is_empty() && id.len() <= MAX_ID_LEN && id.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        invalid("not a valid card")
    }
}

pub fn check_search(request: &SearchRequest) -> R<()> {
    if request.q.chars().count() > MAX_QUERY_LEN {
        return invalid("search for at most 500 characters");
    }
    check_tz(&request.tz)
}

/// The optional parts of a card action, as the page sends them.
#[derive(Debug, Default, Clone)]
pub struct CardActionArgs {
    pub reason: Option<String>,
    pub note: Option<String>,
    pub until: Option<String>,
    pub index: Option<u32>,
    pub body: Option<String>,
}

pub fn card_action(action: &str, a: CardActionArgs) -> R<CardAction> {
    let CardActionArgs {
        reason,
        note,
        until,
        index,
        body,
    } = a;
    Ok(match action {
        "accept" => {
            if body.as_ref().is_some_and(|b| b.len() > MAX_REPLY_LEN) {
                return invalid("the reply is too long");
            }
            CardAction::Accept {
                action: index,
                body,
            }
        }
        "done" => CardAction::Done,
        "dismiss" => {
            let reason = match reason.as_deref().unwrap_or("not_relevant") {
                "not_relevant" => DismissReason::NotRelevant,
                "already_handled" => DismissReason::AlreadyHandled,
                "wrong_info" => DismissReason::WrongInfo,
                "not_important" => DismissReason::NotImportant,
                "other" => DismissReason::Other,
                _ => return invalid("not a dismiss reason"),
            };
            let note = note.map(|n| n.chars().take(200).collect());
            CardAction::Dismiss { reason, note }
        }
        "snooze" => CardAction::Snooze {
            until: match until.filter(|u| !u.is_empty() && u.len() <= MAX_TIME_LEN) {
                Some(u) => u,
                None => return invalid("snooze needs a time"),
            },
        },
        _ => return invalid("not a card action"),
    })
}

// ---- dispatch -----------------------------------------------------------------------------------

/// What the shell does after a command, besides answering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Open this `https:` page in the browser (sign-in, consent, Checkout, the portal).
    OpenWeb(String),
    /// Tell the page the session changed (`nugget:cloud-session`).
    Session {
        signed_in: bool,
        error: Option<String>,
    },
    /// Hand nuggetd a fresh token, or withdraw it (`cloud.llm.session`).
    Handoff,
}

fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn arg_string(args: &Value, k: &str) -> Option<String> {
    arg_str(args, k).map(str::to_owned)
}

fn need(args: &Value, k: &str) -> R<String> {
    arg_string(args, k).ok_or_else(|| CloudError::Invalid(format!("missing {k}")))
}

fn to_value<T: serde::Serialize>(v: T) -> R<Value> {
    serde_json::to_value(v).map_err(|e| CloudError::Invalid(e.to_string()))
}

/// Run one `cloud_*` command as the shell does (args camelCased, as the page sends them). A
/// "signed out" answer from the server also yields the session and handoff effects, like the
/// shell's `checked`.
pub async fn dispatch(api: &Api, cmd: &str, args: &Value) -> (R<Value>, Vec<Effect>) {
    let mut effects = Vec::new();
    let r = run(api, cmd, args, &mut effects).await;
    if let Err(CloudError::NotSignedIn) = &r {
        effects.push(Effect::Session {
            signed_in: false,
            error: None,
        });
        effects.push(Effect::Handoff);
    }
    (r, effects)
}

async fn run(api: &Api, cmd: &str, a: &Value, fx: &mut Vec<Effect>) -> R<Value> {
    match cmd {
        "cloud_session" => {
            let s = api.session().status().await;
            Ok(json!({"configured": true, "signed_in": s.signed_in, "email": s.email}))
        }
        "cloud_sign_in" => match (arg_str(a, "provider"), arg_str(a, "email")) {
            (Some(p), None) => {
                let url = api.session().begin_oauth(Provider::parse(p)?).await?;
                fx.push(Effect::OpenWeb(url));
                Ok(Value::Null)
            }
            (None, Some(email)) => {
                api.session().begin_magic_link(email).await?;
                Ok(Value::Null)
            }
            _ => invalid("choose a provider or enter an email"),
        },
        "cloud_sign_out" => {
            let revoked = api.session().sign_out().await?;
            fx.push(Effect::Session {
                signed_in: false,
                error: None,
            });
            fx.push(Effect::Handoff);
            Ok(json!({"revoked": revoked}))
        }
        "cloud_connections_list" => to_value(api.connections().await?),
        "cloud_connect" => {
            let toolkit = need(a, "toolkit")?;
            check_toolkit(&toolkit)?;
            let start = api.connect(&toolkit).await?;
            fx.push(Effect::OpenWeb(start.redirect_url));
            Ok(Value::Null)
        }
        "cloud_disconnect" => {
            let id = need(a, "id")?;
            check_connection_id(&id)?;
            api.disconnect(&id).await?;
            Ok(Value::Null)
        }
        "cloud_sync" => to_value(api.sync().await?),
        "cloud_ask" => {
            let question = check_question(&need(a, "question")?)?;
            let conversation_id = arg_string(a, "conversationId");
            check_conversation(&conversation_id)?;
            let model = check_model(arg_str(a, "model"))?;
            to_value(
                api.ask(&AskRequest {
                    question,
                    conversation_id,
                    model,
                })
                .await?,
            )
        }
        "cloud_apps" => to_value(api.apps().await?),
        "cloud_actions_list" => {
            let status = arg_str(a, "status").map(check_action_status).transpose()?;
            let snoozed = a
                .get("includeSnoozed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            to_value(
                api.actions(status.map(ActionStatus::as_str), snoozed)
                    .await?,
            )
        }
        "cloud_action_propose" => {
            let kind = check_action_kind(&need(a, "kind")?)?;
            let toolkit = need(a, "toolkit")?;
            check_toolkit(&toolkit)?;
            let input = a.get("input").cloned().unwrap_or(Value::Null);
            check_action_input(&input)?;
            let idempotency_key = need(a, "idempotencyKey")?;
            check_idempotency_key(&idempotency_key)?;
            to_value(
                api.propose_action(&ProposeAction {
                    kind,
                    toolkit,
                    input,
                    idempotency_key,
                })
                .await?,
            )
        }
        "cloud_action_preview" | "cloud_action_status" | "cloud_action_cancel" => {
            let id = need(a, "id")?;
            check_action_id(&id)?;
            to_value(match cmd {
                "cloud_action_preview" => api.action_preview(&id).await?,
                "cloud_action_status" => api.action(&id).await?,
                _ => api.cancel_action(&id).await?,
            })
        }
        "cloud_action_approve" => {
            let (id, digest) = (need(a, "id")?, need(a, "digest")?);
            check_action_id(&id)?;
            check_digest(&digest)?;
            to_value(api.approve_action(&id, &digest).await?)
        }
        "cloud_action_update" => {
            let id = need(a, "id")?;
            check_action_id(&id)?;
            let input = a.get("input").cloned().unwrap_or(Value::Null);
            check_action_edit(&input)?;
            to_value(api.update_action(&id, &input).await?)
        }
        "cloud_action_snooze" => {
            let id = need(a, "id")?;
            check_action_id(&id)?;
            let until = arg_string(a, "until");
            if let Some(u) = &until {
                check_snooze_until(u)?;
            }
            to_value(api.snooze_action(&id, until.as_deref()).await?)
        }
        "cloud_me" => to_value(api.me().await?),
        "cloud_billing_plans" => to_value(api.plans().await?),
        "cloud_billing_checkout" => {
            let plan = need(a, "plan")?;
            let interval = Interval::parse(&need(a, "interval")?)?;
            let link = api.checkout(&plan, interval).await?;
            fx.push(Effect::OpenWeb(link.url));
            Ok(Value::Null)
        }
        "cloud_billing_preview" => {
            let plan = need(a, "plan")?;
            let interval = Interval::parse(&need(a, "interval")?)?;
            to_value(api.billing_preview(&plan, interval).await?)
        }
        "cloud_billing_portal" => {
            let link = api.portal().await?;
            fx.push(Effect::OpenWeb(link.url));
            Ok(Value::Null)
        }
        "cloud_delete_account" => {
            api.delete_account().await?;
            fx.push(Effect::Session {
                signed_in: false,
                error: None,
            });
            fx.push(Effect::Handoff);
            Ok(Value::Null)
        }
        "cloud_search" => {
            let request: SearchRequest =
                serde_json::from_value(a.get("request").cloned().unwrap_or(Value::Null))
                    .map_err(|e| CloudError::Invalid(e.to_string()))?;
            check_search(&request)?;
            to_value(api.search(&request).await?)
        }
        "cloud_cards" | "cloud_cards_refresh" | "cloud_brief" => {
            let tz = arg_string(a, "tz");
            check_tz(&tz)?;
            match cmd {
                "cloud_cards" => to_value(api.cards(tz.as_deref()).await?),
                "cloud_cards_refresh" => to_value(api.refresh_cards(tz.as_deref()).await?),
                _ => to_value(api.brief(tz.as_deref()).await?),
            }
        }
        "cloud_cards_seen" => {
            let ids: Vec<String> = a
                .get("ids")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if ids.len() > MAX_SEEN {
                return invalid("too many cards");
            }
            for id in &ids {
                check_card_id(id)?;
            }
            api.cards_seen(&ids).await?;
            Ok(Value::Null)
        }
        "cloud_card_action" => {
            let id = need(a, "id")?;
            check_card_id(&id)?;
            let action = card_action(
                &need(a, "action")?,
                CardActionArgs {
                    reason: arg_string(a, "reason"),
                    note: arg_string(a, "note"),
                    until: arg_string(a, "until"),
                    index: a
                        .get("index")
                        .and_then(Value::as_u64)
                        .and_then(|i| u32::try_from(i).ok()),
                    body: arg_string(a, "body"),
                },
            )?;
            to_value(api.card_action(&id, &action).await?)
        }
        other => invalid(format!("unknown command {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolkits_are_slugs() {
        assert!(check_toolkit("gmail").is_ok());
        assert!(check_toolkit("googlecalendar").is_ok());
        for bad in ["", "Gmail", "../x", "a b", &"a".repeat(65)] {
            assert!(check_toolkit(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn action_kinds_and_statuses_are_known() {
        assert_eq!(
            check_action_kind("send_mail").unwrap(),
            ActionKind::SendMail
        );
        assert_eq!(
            check_action_kind("create_page").unwrap(),
            ActionKind::CreatePage
        );
        for bad in ["", "SEND_MAIL", "delete_mail", "send_mail "] {
            assert!(check_action_kind(bad).is_err(), "{bad}");
        }
        assert_eq!(
            check_action_status("pending").unwrap(),
            ActionStatus::Pending
        );
        for bad in ["", "unknown", "Pending", "pending&x=1"] {
            assert!(check_action_status(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ids_digests_and_keys() {
        assert!(check_action_id("6f1c0b8e-2b7d-4a52-9a43-0e0f6a2c1d11").is_ok());
        assert!(check_action_id("").is_err());
        assert!(check_action_id(&"a".repeat(129)).is_err());
        assert!(check_digest(&"0a".repeat(32)).is_ok());
        for bad in [
            String::new(),
            "0A".repeat(32),
            "0a".repeat(31),
            "0a".repeat(33),
            "g".repeat(64),
        ] {
            assert!(check_digest(&bad).is_err(), "{bad}");
        }
        for ok in ["k", "ui:2026-11-02T10:00:00.123_ab-1", &"a".repeat(128)] {
            assert!(check_idempotency_key(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a/b", "é", &"a".repeat(129)] {
            assert!(check_idempotency_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn action_inputs_and_edits() {
        assert!(
            check_action_input(&json!({ "to": ["a@b.c"], "subject": "s", "body": "b" })).is_ok()
        );
        for bad in [json!(null), json!([]), json!("x"), json!(1)] {
            assert!(check_action_input(&bad).is_err(), "{bad}");
        }
        assert!(
            check_action_input(&json!({ "body": "x".repeat(MAX_ACTION_INPUT_BYTES) })).is_err()
        );
        for ok in [
            json!({}),
            json!({ "subject": "s", "cc": ["a@b.c"] }),
            json!({ "text": "hi" }),
            json!({ "title": "t", "markdown": "# m" }),
            json!({ "start": "2026-11-02T10:00:00Z", "attendees": [], "timezone": "Europe/London" }),
        ] {
            assert!(check_action_edit(&ok).is_ok(), "{ok}");
        }
        for bad in [
            json!(null),
            json!({ "event_id": "e1" }),
            json!({ "channel": "C1" }),
            json!({ "parent_id": "p" }),
            json!({ "thread_id": "t", "body": "b" }),
        ] {
            assert!(check_action_edit(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn snooze_times_are_rfc3339() {
        for ok in [
            "2026-11-02T10:10:00Z",
            "2026-11-02T10:10:00.123+01:00",
            "2026-11-02t10:10:00z",
        ] {
            assert!(check_snooze_until(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "tomorrow",
            "2026-11-02",
            "2026-11-02 10:10",
            "2026-13-02T10:10:00Z",
        ] {
            assert!(check_snooze_until(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn questions_models_zones_and_cards() {
        assert_eq!(check_question("  hi ").unwrap(), "hi");
        assert!(check_question(" ").is_err());
        assert!(check_question(&"x".repeat(4_001)).is_err());
        assert_eq!(check_model(Some("smart")).unwrap(), Some(ModelTier::Smart));
        assert!(check_model(Some("huge")).is_err());
        assert!(check_tz(&Some("America/Port-au-Prince".into())).is_ok());
        assert!(check_tz(&None).is_ok());
        assert!(check_tz(&Some("../etc".into())).is_err());
        assert!(check_card_id("abc123").is_ok());
        assert!(check_card_id("a/b").is_err());
        assert_eq!(
            card_action(
                "dismiss",
                CardActionArgs {
                    reason: Some("wrong_info".into()),
                    ..Default::default()
                }
            )
            .unwrap(),
            CardAction::Dismiss {
                reason: DismissReason::WrongInfo,
                note: None
            }
        );
        assert!(card_action("snooze", CardActionArgs::default()).is_err());
        assert!(card_action("explode", CardActionArgs::default()).is_err());
    }
}
