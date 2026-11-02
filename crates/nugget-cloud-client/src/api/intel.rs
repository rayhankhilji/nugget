//! Search, "Needs you" cards and the morning brief (`/v1/search`, `/v1/cards`, `/v1/brief`).
//! Cards keep every field the service sends (`extra`), so the desktop gets the proactive card
//! as is: `title_text`/`detail_text` LocalText keys, evidence, factors, actions.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Api, Citation};
use crate::{CloudError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub q: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// The user's IANA zone, for phrases like "yesterday".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub item_id: String,
    pub source: String,
    pub kind: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub occurred_at: Option<String>,
    pub snippet: String,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCount {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facets {
    pub sources: Vec<FacetCount>,
    pub people: Vec<FacetCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineBucket {
    pub start: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeline {
    /// `day` or `month`.
    pub bucket: String,
    pub points: Vec<TimelineBucket>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDates {
    pub from: Option<String>,
    pub to: Option<String>,
    pub phrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// `hybrid`, `text` or `browse`.
    pub mode: String,
    pub query: String,
    pub dates: SearchDates,
    pub results: Vec<SearchResult>,
    pub total: i64,
    pub facets: Facets,
    pub timeline: Timeline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardEvidence {
    /// `{source, key}`; `key` is the cloud item id.
    pub item: Value,
    pub quote: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub from_user: bool,
}

/// A "Needs you" card (nugget-proactive's card).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    /// `unanswered_thread`, `commitment`, `meeting_prep`, `renewal`, `follow_up`.
    pub kind: String,
    pub title: String,
    pub detail: String,
    /// `{key, args}` for the desktop's i18n.
    #[serde(default)]
    pub title_text: Option<Value>,
    #[serde(default)]
    pub detail_text: Option<Value>,
    pub evidence: Vec<CardEvidence>,
    pub due: Option<String>,
    pub rank: f64,
    /// `new`, `seen`, `snoozed`, `accepted`, `dismissed`, `done`, `expired`.
    pub state: String,
    pub sensitive: bool,
    /// The rest (people, factors, actions, learned, history, …) as sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cards {
    pub cards: Vec<Card>,
    pub last_refresh: Option<String>,
    /// Only from [`Api::refresh_cards`]: what the run did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
}

/// Why a card was dismissed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DismissReason {
    NotRelevant,
    AlreadyHandled,
    WrongInfo,
    NotImportant,
    Other,
}

/// What to do with a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardAction {
    Accept,
    Done,
    Dismiss {
        reason: DismissReason,
        note: Option<String>,
    },
    /// Until an RFC 3339 time.
    Snooze {
        until: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    /// The user's local date.
    pub day: String,
    pub timezone: String,
    /// Contains `[n]` markers for `citations`; None without a model or quota.
    pub prose: Option<String>,
    pub citations: Vec<Citation>,
    /// nugget-proactive's brief: `top`, `meetings`, `overdue`, `renewals`, `open_cards`.
    pub brief: Value,
    pub created_at: String,
}

#[derive(Deserialize)]
struct BriefEnvelope {
    brief: Option<Brief>,
}

#[derive(Deserialize)]
struct CardEnvelope {
    card: Option<Card>,
}

/// Card ids are short hex hashes; refuse anything else before it goes into a path.
fn card_id(id: &str) -> Result<&str> {
    if !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(id)
    } else {
        Err(CloudError::Invalid("not a valid card id".into()))
    }
}

/// `?tz=<zone>`, percent-encoded.
fn tz_query(tz: Option<&str>) -> String {
    match tz.filter(|t| !t.is_empty()) {
        Some(t) => {
            let enc: String = t
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        char::from(b).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect();
            format!("?tz={enc}")
        }
        None => String::new(),
    }
}

impl Api {
    /// `POST /v1/search`.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        let body = serde_json::to_value(request).map_err(|e| CloudError::Invalid(e.to_string()))?;
        self.json(Method::POST, "search", Some(body)).await
    }

    /// `GET /v1/cards`: open cards, best first.
    pub async fn cards(&self, tz: Option<&str>) -> Result<Cards> {
        self.json(Method::GET, &format!("cards{}", tz_query(tz)), None)
            .await
    }

    /// `POST /v1/cards/refresh`: run knowledge and cards now.
    pub async fn refresh_cards(&self, tz: Option<&str>) -> Result<Cards> {
        self.json(
            Method::POST,
            &format!("cards/refresh{}", tz_query(tz)),
            Some(json!({})),
        )
        .await
    }

    /// `POST /v1/cards/seen`.
    pub async fn cards_seen(&self, ids: &[String]) -> Result<()> {
        self.send(Method::POST, "cards/seen", Some(json!({ "ids": ids })))
            .await
            .map(drop)
    }

    /// `POST /v1/cards/{id}/{accept|done|dismiss|snooze}`: the card afterwards.
    pub async fn card_action(&self, id: &str, action: &CardAction) -> Result<Option<Card>> {
        let id = card_id(id)?;
        let (name, body) = match action {
            CardAction::Accept => ("accept", json!({})),
            CardAction::Done => ("done", json!({})),
            CardAction::Dismiss { reason, note } => {
                ("dismiss", json!({ "reason": reason, "note": note }))
            }
            CardAction::Snooze { until } => ("snooze", json!({ "until": until })),
        };
        let env: CardEnvelope = self
            .json(Method::POST, &format!("cards/{id}/{name}"), Some(body))
            .await?;
        Ok(env.card)
    }

    /// `GET /v1/brief`: today's brief (from 07:00 local), else the latest one, if any.
    pub async fn brief(&self, tz: Option<&str>) -> Result<Option<Brief>> {
        let env: BriefEnvelope = self
            .json(Method::GET, &format!("brief{}", tz_query(tz)), None)
            .await?;
        Ok(env.brief)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::signed_in;
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn card() -> Value {
        json!({
            "id": "abc123", "dedup_key": "unanswered:x", "kind": "unanswered_thread",
            "title": "Reply to Dana", "detail": "Signed contract",
            "title_text": {"key": "proactive.card.unansweredTitle", "args": {"name": "Dana"}},
            "evidence": [{"item": {"source": "gmail", "key": "i1"}, "quote": "Could you send it?", "url": null}],
            "people": ["dana@x.example"], "due": null, "score": 0.7, "rank": 0.7,
            "factors": [], "actions": [], "state": "new", "sensitive": false
        })
    }

    #[tokio::test]
    async fn search_cards_and_brief() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/search"))
            .and(header("authorization", "Bearer AT"))
            .and(body_json(json!({"q": "contract", "sources": ["gmail"], "tz": "Europe/London"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "mode": "hybrid", "query": "contract",
                "dates": {"from": null, "to": null, "phrase": null},
                "results": [{"item_id": "i1", "source": "gmail", "kind": "email", "title": "Signed contract",
                             "author": "Dana", "url": null, "occurred_at": "2026-10-30T09:00:00Z",
                             "snippet": "Could you send…", "score": 0.9}],
                "total": 1,
                "facets": {"sources": [{"value": "gmail", "count": 1}], "people": [{"value": "Dana", "count": 1}]},
                "timeline": {"bucket": "month", "points": [{"start": "2026-10-01T00:00:00Z", "count": 1}]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/cards"))
            .and(query_param("tz", "America/New_York"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"cards": [card()], "last_refresh": "2026-11-02T07:00:00Z"}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/cards/abc123/dismiss"))
            .and(body_json(
                json!({"reason": "already_handled", "note": null}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"card": card()})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/cards/abc123/snooze"))
            .and(body_json(json!({"until": "2026-11-04T09:00:00Z"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"card": card()})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/brief"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"brief": {
                "day": "2026-11-02", "timezone": "Europe/London", "prose": "Dana waits [1].",
                "citations": [{"n": 1, "item_id": "i1", "title": "Signed contract", "source": "gmail",
                               "url": null, "occurred_at": null, "snippet": "Needs you: …"}],
                "brief": {"top": []}, "created_at": "2026-11-02T07:00:01Z"
            }})))
            .mount(&server)
            .await;

        let r = api
            .search(&SearchRequest {
                q: "contract".into(),
                sources: vec!["gmail".into()],
                tz: Some("Europe/London".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(r.results[0].title.as_deref(), Some("Signed contract"));
        assert_eq!(r.facets.sources[0].count, 1);
        assert_eq!(r.timeline.bucket, "month");

        let c = api.cards(Some("America/New_York")).await.unwrap();
        assert_eq!(c.cards[0].kind, "unanswered_thread");
        assert_eq!(
            c.cards[0].title_text.as_ref().unwrap()["key"],
            "proactive.card.unansweredTitle"
        );
        assert_eq!(
            c.cards[0].extra["people"][0], "dana@x.example",
            "unknown fields are kept"
        );
        let back = serde_json::to_value(&c.cards[0]).unwrap();
        assert_eq!(back["dedup_key"], "unanswered:x");

        api.card_action(
            "abc123",
            &CardAction::Dismiss {
                reason: DismissReason::AlreadyHandled,
                note: None,
            },
        )
        .await
        .unwrap();
        api.card_action(
            "abc123",
            &CardAction::Snooze {
                until: "2026-11-04T09:00:00Z".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            api.card_action("../x", &CardAction::Accept).await,
            Err(CloudError::Invalid(_))
        ));

        let b = api.brief(None).await.unwrap().unwrap();
        assert_eq!(b.citations[0].n, 1);
        assert!(b.prose.unwrap().contains("[1]"));
    }

    #[test]
    fn tz_is_encoded() {
        assert_eq!(
            tz_query(Some("America/Port-au-Prince")),
            "?tz=America%2FPort-au-Prince"
        );
        assert_eq!(tz_query(None), "");
    }
}
