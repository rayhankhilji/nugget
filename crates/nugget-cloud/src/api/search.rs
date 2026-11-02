//! `POST /v1/search`: hybrid search with facets and a timeline (see [`crate::search`]).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use time::OffsetDateTime;

use super::{ApiError, ApiResult, AppState, user};
use crate::intel;
use crate::search::{self, SearchRequest, SearchResponse};

const MAX_QUERY_CHARS: usize = 500;

#[derive(Deserialize)]
pub(super) struct Body {
    #[serde(flatten)]
    req: SearchRequest,
    /// The desktop's IANA zone, for dates like "yesterday".
    tz: Option<String>,
}

pub(super) async fn search(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<Body>,
) -> ApiResult<Json<SearchResponse>> {
    let u = user(&s, &h).await?;
    if b.req.q.chars().count() > MAX_QUERY_CHARS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_query",
            "search for at most 500 characters",
        ));
    }
    if let Some((from, to)) = b.req.from.zip(b.req.to)
        && from >= to
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_range",
            "`from` must be before `to`",
        ));
    }
    let tz = super::cards::timezone(&s, u.id, b.tz.as_deref()).await?;
    let now = OffsetDateTime::now_utc();
    let offset = intel::utc_offset(tz.as_deref(), now);
    Ok(Json(
        search::search(&s.db, &s.gateway, u.id, &b.req, offset, now).await?,
    ))
}
