//! Search, "Needs you" cards and the morning brief from the cloud (`/v1/search`, `/v1/cards`,
//! `/v1/brief`). The page passes its IANA zone (`tz`) so "today" and "yesterday" are the user's.

use nugget_cloud_client::{
    Brief, CardAction, CardOutcome, Cards, CloudError, DismissReason, SearchRequest, SearchResponse,
};
use tauri::{AppHandle, State};

use super::{Cloud, Result, checked};

const MAX_QUERY_LEN: usize = 500;
const MAX_ID_LEN: usize = 128;
const MAX_SEEN: usize = 200;

/// An IANA zone name (`Europe/London`), or nothing.
fn check_tz(tz: &Option<String>) -> Result<()> {
    let ok = tz.as_deref().is_none_or(|t| {
        !t.is_empty()
            && t.len() <= 64
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c))
    });
    if ok {
        Ok(())
    } else {
        Err(CloudError::Invalid("not a time zone".into()).into())
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID_LEN || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(CloudError::Invalid("not a valid card".into()).into());
    }
    Ok(())
}

/// `action`: `accept`, `done`, `dismiss` (with `reason`, and `note` for `other`) or `snooze`
/// (with `until`, RFC 3339).
/// `accept` (optionally `index`: the card action to propose, and `body`: the reply text), `done`,
/// `dismiss` (with `reason`, and `note` for `other`) or `snooze` (with `until`, RFC 3339).
struct ActionArgs {
    reason: Option<String>,
    note: Option<String>,
    until: Option<String>,
    index: Option<u32>,
    body: Option<String>,
}

const MAX_BODY_LEN: usize = 100_000;

fn card_action(action: &str, a: ActionArgs) -> Result<CardAction> {
    let ActionArgs {
        reason,
        note,
        until,
        index,
        body,
    } = a;
    let bad = |m: &str| -> super::CloudCmdError { CloudError::Invalid(m.into()).into() };
    Ok(match action {
        "accept" => {
            if body.as_ref().is_some_and(|b| b.len() > MAX_BODY_LEN) {
                return Err(bad("the reply is too long"));
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
                _ => return Err(bad("not a dismiss reason")),
            };
            let note = note.map(|n| n.chars().take(200).collect());
            CardAction::Dismiss { reason, note }
        }
        "snooze" => CardAction::Snooze {
            until: until
                .filter(|u| !u.is_empty() && u.len() <= 64)
                .ok_or_else(|| bad("snooze needs a time"))?,
        },
        _ => return Err(bad("not a card action")),
    })
}

#[tauri::command]
pub async fn cloud_search(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    request: SearchRequest,
) -> Result<SearchResponse> {
    if request.q.chars().count() > MAX_QUERY_LEN {
        return Err(CloudError::Invalid("search for at most 500 characters".into()).into());
    }
    check_tz(&request.tz)?;
    let api = cloud.api()?;
    checked(&app, api.search(&request).await)
}

#[tauri::command]
pub async fn cloud_cards(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    tz: Option<String>,
) -> Result<Cards> {
    check_tz(&tz)?;
    let api = cloud.api()?;
    checked(&app, api.cards(tz.as_deref()).await)
}

#[tauri::command]
pub async fn cloud_cards_refresh(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    tz: Option<String>,
) -> Result<Cards> {
    check_tz(&tz)?;
    let api = cloud.api()?;
    checked(&app, api.refresh_cards(tz.as_deref()).await)
}

#[tauri::command]
pub async fn cloud_cards_seen(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    ids: Vec<String>,
) -> Result<()> {
    if ids.len() > MAX_SEEN {
        return Err(CloudError::Invalid("too many cards".into()).into());
    }
    for id in &ids {
        check_id(id)?;
    }
    let api = cloud.api()?;
    checked(&app, api.cards_seen(&ids).await)
}

// Each argument is a named key of the page's `invoke` call.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cloud_card_action(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    id: String,
    action: String,
    reason: Option<String>,
    note: Option<String>,
    until: Option<String>,
    index: Option<u32>,
    body: Option<String>,
) -> Result<CardOutcome> {
    check_id(&id)?;
    let action = card_action(
        &action,
        ActionArgs {
            reason,
            note,
            until,
            index,
            body,
        },
    )?;
    let api = cloud.api()?;
    checked(&app, api.card_action(&id, &action).await)
}

#[tauri::command]
pub async fn cloud_brief(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    tz: Option<String>,
) -> Result<Option<Brief>> {
    check_tz(&tz)?;
    let api = cloud.api()?;
    checked(&app, api.brief(tz.as_deref()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(reason: Option<&str>, index: Option<u32>) -> ActionArgs {
        ActionArgs {
            reason: reason.map(str::to_owned),
            note: None,
            until: None,
            index,
            body: None,
        }
    }

    #[test]
    fn inputs_are_checked() {
        assert!(check_tz(&Some("America/Port-au-Prince".into())).is_ok());
        assert!(check_tz(&None).is_ok());
        assert!(check_tz(&Some("../etc".into())).is_err());
        assert!(check_id("abc123").is_ok());
        assert!(check_id("a/b").is_err());
        assert_eq!(
            card_action("dismiss", args(Some("wrong_info"), None)).unwrap(),
            CardAction::Dismiss {
                reason: DismissReason::WrongInfo,
                note: None
            }
        );
        assert!(card_action("dismiss", args(Some("meh"), None)).is_err());
        assert!(card_action("snooze", args(None, None)).is_err());
        assert!(card_action("explode", args(None, None)).is_err());
        assert_eq!(
            card_action("accept", args(None, Some(1))).unwrap(),
            CardAction::Accept {
                action: Some(1),
                body: None
            }
        );
    }
}
