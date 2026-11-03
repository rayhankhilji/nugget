//! Search, "Needs you" cards and the morning brief from the cloud (`/v1/search`, `/v1/cards`,
//! `/v1/brief`). The page passes its IANA zone (`tz`) so "today" and "yesterday" are the user's.

use nugget_cloud_client::{Brief, CardOutcome, Cards, CloudError, SearchRequest, SearchResponse};
use tauri::{AppHandle, State};

use super::{Cloud, Result, checked};
// The input checks are shared with the e2e shell (`nugget_cloud_client::shell`).
use nugget_cloud_client::shell::{
    CardActionArgs, MAX_SEEN, card_action, check_card_id as check_id, check_search, check_tz,
};

#[tauri::command]
pub async fn cloud_search(
    app: AppHandle,
    cloud: State<'_, Cloud>,
    request: SearchRequest,
) -> Result<SearchResponse> {
    check_search(&request)?;
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
        CardActionArgs {
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
