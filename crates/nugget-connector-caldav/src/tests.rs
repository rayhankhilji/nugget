use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nugget_connector::{
    AuthKind, CancellationToken, Change, ChangeSink, Connector, ConnectorContext, ConnectorFactory,
    Content, KnownItems, SecretStore,
};
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

// ---------------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeStore {
    items: Mutex<HashMap<ItemKey, Option<String>>>,
    log: Mutex<Vec<Change>>,
    checkpoints: Mutex<Vec<Value>>,
}

impl FakeStore {
    fn seed(&self, key: &str, version: &str) {
        self.items
            .lock()
            .unwrap()
            .insert(ItemKey::new(key), Some(version.to_string()));
    }
    fn take_log(&self) -> Vec<Change> {
        std::mem::take(&mut self.log.lock().unwrap())
    }
}

#[async_trait]
impl ChangeSink for FakeStore {
    async fn send(&self, change: Change) -> Result<()> {
        match &change {
            Change::Upsert(d) => {
                self.items
                    .lock()
                    .unwrap()
                    .insert(d.key.clone(), d.version.clone());
            }
            Change::Delete(k) => {
                self.items.lock().unwrap().remove(k);
            }
        }
        self.log.lock().unwrap().push(change);
        Ok(())
    }
    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        self.checkpoints.lock().unwrap().push(cursor);
        Ok(())
    }
}

struct Known(Arc<FakeStore>);

#[async_trait]
impl KnownItems for Known {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(self.0.items.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);

impl SecretStore for Secrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[derive(Default)]
struct Egress(Mutex<Vec<EgressEvent>>);

impl EgressSink for Egress {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

const SOURCE: &str = "caldav:me";

/// Event store backing the dynamic multiget / ETag-listing responders: href → (etag, ics).
type Events = Arc<Mutex<HashMap<String, (String, String)>>>;

struct H {
    server: MockServer,
    store: Arc<FakeStore>,
    secrets: Arc<Secrets>,
    egress: Arc<Egress>,
    events: Events,
}

impl H {
    async fn new() -> Self {
        let secrets = Arc::new(Secrets::default());
        secrets.set(&password_account(SOURCE), "app-pass").unwrap();
        Self {
            server: MockServer::start().await,
            store: Arc::new(FakeStore::default()),
            secrets,
            egress: Arc::new(Egress::default()),
            events: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn connector_with(&self, settings: Value) -> CalDavConnector {
        let ctx = ConnectorContext {
            source: SourceId::new(SOURCE),
            paths: Paths::rooted(std::env::temp_dir().join("nugget-caldav-tests")),
            egress: self.egress.clone(),
            secrets: self.secrets.clone(),
            known: Arc::new(Known(self.store.clone())),
        };
        CalDavConnector::new(CalDavSettings::parse(&settings).unwrap(), ctx).unwrap()
    }

    fn connector(&self) -> CalDavConnector {
        self.connector_with(json!({"url": format!("{}/", self.server.uri()), "username": "me"}))
    }

    async fn sync(&self, cursor: Option<Value>) -> Result<Value> {
        self.connector()
            .sync(cursor, &*self.store, &CancellationToken::new())
            .await
    }

    fn event(&self, href: &str, etag: &str) {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{href}\r\nSUMMARY:Event {etag}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        self.events
            .lock()
            .unwrap()
            .insert(href.to_string(), (etag.to_string(), ics));
    }

    async fn requests(&self, m: &str) -> Vec<Request> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.method.as_str() == m)
            .collect()
    }

    /// Discovery: `/` → principal → home with a sync-capable "Work" calendar, an ETag-only
    /// "Tasks" calendar, an address book and a schedule inbox.
    async fn mount_discovery(&self) {
        Mock::given(method("PROPFIND"))
            .and(path("/"))
            .and(header("depth", "0"))
            .respond_with(ms207(&[resp(
                "/",
                "<D:resourcetype><D:collection/></D:resourcetype>\
                 <D:current-user-principal><D:href>/principals/me/</D:href></D:current-user-principal>",
            )]))
            .mount(&self.server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/principals/me/"))
            .and(header("depth", "0"))
            .respond_with(ms207(&[resp(
                "/principals/me/",
                "<D:resourcetype><D:principal/></D:resourcetype>\
                 <C:calendar-home-set><D:href>/calendars/me/</D:href></C:calendar-home-set>",
            )]))
            .mount(&self.server)
            .await;
        Mock::given(method("PROPFIND"))
            .and(path("/calendars/me/"))
            .and(header("depth", "1"))
            .respond_with(ms207(&[
                resp("/calendars/me/", "<D:resourcetype><D:collection/></D:resourcetype>"),
                resp(
                    "/calendars/me/work/",
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
                     <D:displayname>Work</D:displayname><CS:getctag>w1</CS:getctag>\
                     <D:sync-token>http://x/ns/sync/1</D:sync-token>\
                     <C:supported-calendar-component-set><C:comp name=\"VEVENT\"/></C:supported-calendar-component-set>",
                ),
                resp(
                    "/calendars/me/tasks/",
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
                     <D:displayname>Tasks &amp; Chores</D:displayname><CS:getctag>k1</CS:getctag>\
                     <C:supported-calendar-component-set><C:comp name=\"VTODO\"/></C:supported-calendar-component-set>",
                ),
                resp(
                    "/calendars/me/freebusy/",
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
                     <C:supported-calendar-component-set><C:comp name=\"VFREEBUSY\"/></C:supported-calendar-component-set>",
                ),
                resp(
                    "/calendars/me/contacts/",
                    "<D:resourcetype><D:collection/><A:addressbook xmlns:A=\"urn:ietf:params:xml:ns:carddav\"/></D:resourcetype>",
                ),
                resp(
                    "/calendars/me/inbox/",
                    "<D:resourcetype><D:collection/><C:schedule-inbox/></D:resourcetype>",
                ),
            ]))
            .mount(&self.server)
            .await;
    }

    /// `calendar-multiget` and Depth-1 ETag listings served from `events`.
    async fn mount_store(&self) {
        let ev = self.events.clone();
        Mock::given(method("REPORT"))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                let events = ev.lock().unwrap();
                let mut out = Vec::new();
                for href in body.split("<D:href>").skip(1) {
                    let href = href.split("</D:href>").next().unwrap_or("");
                    match events.get(href) {
                        Some((etag, ics)) => out.push(resp(
                            href,
                            &format!(
                                "<D:getetag>\"{etag}\"</D:getetag><C:calendar-data>{}</C:calendar-data>",
                                xml::escape(ics)
                            ),
                        )),
                        None => out.push(status_resp(href, 404)),
                    }
                }
                ms207(&out)
            })
            .mount(&self.server)
            .await;
        let ev = self.events.clone();
        Mock::given(method("PROPFIND"))
            .and(path("/calendars/me/tasks/"))
            .and(header("depth", "1"))
            .respond_with(move |_: &Request| {
                let events = ev.lock().unwrap();
                let mut out = vec![resp(
                    "/calendars/me/tasks/",
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>",
                )];
                for (href, (etag, _)) in events.iter() {
                    if href.starts_with("/calendars/me/tasks/") {
                        out.push(resp(
                            href,
                            &format!("<D:resourcetype/><D:getetag>\"{etag}\"</D:getetag>"),
                        ));
                    }
                }
                ms207(&out)
            })
            .mount(&self.server)
            .await;
    }

    async fn mount_sync(&self, token: &str, template: ResponseTemplate) {
        Mock::given(method("REPORT"))
            .and(path("/calendars/me/work/"))
            .and(header("depth", "0"))
            .and(body_string_contains("sync-collection"))
            .and(body_string_contains(format!(
                "<D:sync-token>{}</D:sync-token>",
                xml::escape(token)
            )))
            .respond_with(template)
            .mount(&self.server)
            .await;
    }
}

fn resp(href: &str, props: &str) -> String {
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{props}</D:prop>\
         <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
    )
}

fn status_resp(href: &str, code: u16) -> String {
    format!(
        "<D:response><D:href>{href}</D:href><D:status>HTTP/1.1 {code} X</D:status></D:response>"
    )
}

fn ms_body(responses: &[String], token: Option<&str>) -> String {
    let token = token
        .map(|t| format!("<D:sync-token>{t}</D:sync-token>"))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\" xmlns:CS=\"http://calendarserver.org/ns/\">{}{token}</D:multistatus>",
        responses.concat()
    )
}

fn ms207(responses: &[String]) -> ResponseTemplate {
    xml207(ms_body(responses, None))
}

fn xml207(body: String) -> ResponseTemplate {
    ResponseTemplate::new(207)
        .insert_header("content-type", "application/xml; charset=utf-8")
        .set_body_string(body)
}

fn etag_resp(href: &str, etag: &str) -> String {
    resp(href, &format!("<D:getetag>\"{etag}\"</D:getetag>"))
}

fn keys(log: &[Change], upserts: bool) -> Vec<String> {
    let mut k: Vec<String> = log
        .iter()
        .filter_map(|c| match (c, upserts) {
            (Change::Upsert(d), true) => Some(d.key.0.clone()),
            (Change::Delete(k), false) => Some(k.0.clone()),
            _ => None,
        })
        .collect();
    k.sort();
    k
}

const W: &str = "/calendars/me/work/";
const T: &str = "/calendars/me/tasks/";

// ---------------------------------------------------------------------------------------------
// Sync behaviour
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn discovery_and_initial_sync_collection_with_paging() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    for (n, e) in [("e1", "1"), ("e2", "1"), ("e3", "1")] {
        h.event(&format!("{W}{n}.ics"), e);
    }
    h.event(&format!("{T}t1.ics"), "a");
    // Page 1 (truncated: 507 on the collection), page 2 completes.
    h.mount_sync(
        "",
        xml207(ms_body(
            &[
                etag_resp(&format!("{W}e1.ics"), "1"),
                etag_resp(&format!("{W}e2.ics"), "1"),
                status_resp(W, 507),
            ],
            Some("tok-1"),
        )),
    )
    .await;
    h.mount_sync(
        "tok-1",
        xml207(ms_body(
            &[etag_resp(&format!("{W}e3.ics"), "1")],
            Some("tok-2"),
        )),
    )
    .await;

    let cursor = h.sync(None).await.unwrap();
    let log = h.store.take_log();
    assert_eq!(
        keys(&log, true),
        vec![
            "/calendars/me/tasks/t1.ics",
            "/calendars/me/work/e1.ics",
            "/calendars/me/work/e2.ics",
            "/calendars/me/work/e3.ics",
        ]
    );
    let doc = log
        .iter()
        .find_map(|c| match c {
            Change::Upsert(d) if d.key.0.ends_with("e1.ics") => Some(d.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(doc.mime, "text/calendar");
    assert_eq!(doc.version.as_deref(), Some("\"1\""));
    let Content::Bytes(b) = &doc.content else {
        panic!()
    };
    assert!(String::from_utf8_lossy(b).contains("BEGIN:VEVENT\r\nUID:/calendars/me/work/e1.ics"));
    assert_eq!(doc.metadata["calendar"], "Work");
    assert_eq!(doc.metadata["calendar_href"], W);
    assert_eq!(
        doc.url.as_deref(),
        Some(format!("{}{W}e1.ics", h.server.uri()).as_str())
    );

    assert_eq!(cursor["calendars"][W]["sync_token"], "tok-2");
    assert_eq!(cursor["calendars"][T]["ctag"], "k1");
    assert!(cursor["calendars"].get("/calendars/me/freebusy/").is_none());
    assert_eq!(cursor["calendars"].as_object().unwrap().len(), 2);
    // Basic auth on every request.
    for r in h.server.received_requests().await.unwrap() {
        let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("Basic "));
    }
}

#[tokio::test]
async fn incremental_sync_applies_changes_and_404_deletes() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    h.event(&format!("{W}e1.ics"), "2");
    h.store.seed(&format!("{W}e1.ics"), "\"1\"");
    h.store.seed(&format!("{W}e2.ics"), "\"1\"");
    h.store.seed(&format!("{T}t1.ics"), "\"a\"");
    h.mount_sync(
        "tok-2",
        xml207(ms_body(
            &[
                etag_resp(&format!("{W}e1.ics"), "2"),
                status_resp(&format!("{W}e2.ics"), 404),
            ],
            Some("tok-3"),
        )),
    )
    .await;
    let cursor = json!({"v": 1, "calendars": {
        W: {"sync_token": "tok-2"},
        T: {"ctag": "k1"},
    }});
    let cursor = h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    assert_eq!(keys(&log, true), vec![format!("{W}e1.ics")]);
    assert_eq!(keys(&log, false), vec![format!("{W}e2.ics")]);
    assert_eq!(cursor["calendars"][W]["sync_token"], "tok-3");
    // Tasks' ctag is unchanged: no listing.
    assert!(
        !h.requests("PROPFIND")
            .await
            .iter()
            .any(|r| r.url.path() == T)
    );
    // The page was checkpointed before the end of the pass.
    assert!(h.store.checkpoints.lock().unwrap().len() >= 3);
}

#[tokio::test]
async fn invalid_sync_token_triggers_full_resync_and_removes_unlisted_items() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    h.event(&format!("{W}e1.ics"), "1");
    h.event(&format!("{W}new.ics"), "1");
    h.store.seed(&format!("{W}e1.ics"), "\"1\"");
    h.store.seed(&format!("{W}stale.ics"), "\"1\"");
    h.mount_sync(
        "expired",
        ResponseTemplate::new(409).set_body_string(
            "<?xml version=\"1.0\"?><D:error xmlns:D=\"DAV:\"><D:valid-sync-token/></D:error>",
        ),
    )
    .await;
    h.mount_sync(
        "",
        xml207(ms_body(
            &[
                etag_resp(&format!("{W}e1.ics"), "1"),
                etag_resp(&format!("{W}new.ics"), "1"),
            ],
            Some("fresh"),
        )),
    )
    .await;
    let cursor = json!({"v": 1, "calendars": {W: {"sync_token": "expired"}, T: {"ctag": "k1"}}});
    let cursor = h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    // e1 is unchanged (same ETag): not re-downloaded.
    assert_eq!(keys(&log, true), vec![format!("{W}new.ics")]);
    assert_eq!(keys(&log, false), vec![format!("{W}stale.ics")]);
    assert_eq!(cursor["calendars"][W]["sync_token"], "fresh");
}

#[tokio::test]
async fn etag_fallback_lists_members_when_ctag_changes() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    // Work's sync REPORT is not implemented: falls back to the ETag listing too.
    h.mount_sync("tok", ResponseTemplate::new(501)).await;
    h.mount_sync("", ResponseTemplate::new(501)).await;
    let ev = h.events.clone();
    Mock::given(method("PROPFIND"))
        .and(path(W))
        .and(header("depth", "1"))
        .respond_with(move |_: &Request| {
            let events = ev.lock().unwrap();
            let mut out = vec![resp(
                W,
                "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>",
            )];
            for (href, (etag, _)) in events.iter() {
                if href.starts_with(W) {
                    out.push(etag_resp(href, etag));
                }
            }
            ms207(&out)
        })
        .mount(&h.server)
        .await;
    h.event(&format!("{T}t1.ics"), "a");
    h.event(&format!("{T}t2.ics"), "b");
    h.event(&format!("{W}w1.ics"), "1");
    h.store.seed(&format!("{T}t1.ics"), "\"a\"");
    h.store.seed(&format!("{T}t3.ics"), "\"c\"");
    // Changed ctag (k0 → k1) forces a listing.
    let cursor = json!({"v": 1, "calendars": {W: {"sync_token": "tok"}, T: {"ctag": "k0"}}});
    let cursor = h.sync(Some(cursor)).await.unwrap();
    let log = h.store.take_log();
    assert_eq!(
        keys(&log, true),
        vec![format!("{T}t2.ics"), format!("{W}w1.ics")]
    );
    assert_eq!(keys(&log, false), vec![format!("{T}t3.ics")]);
    assert_eq!(cursor["calendars"][T], json!({"ctag": "k1"}));
    assert_eq!(cursor["calendars"][W], json!({"ctag": "w1"}));

    // Second pass: Tasks' ctag matches → no listing for it.
    let before = h
        .requests("PROPFIND")
        .await
        .iter()
        .filter(|r| r.url.path() == T)
        .count();
    h.sync(Some(cursor)).await.unwrap();
    let after = h
        .requests("PROPFIND")
        .await
        .iter()
        .filter(|r| r.url.path() == T)
        .count();
    assert_eq!(before, after);
    assert!(keys(&h.store.take_log(), true).is_empty());
}

#[tokio::test]
async fn single_calendar_url_and_calendar_filter() {
    let h = H::new().await;
    Mock::given(method("PROPFIND"))
        .and(path(W))
        .and(header("depth", "0"))
        .respond_with(ms207(&[resp(
            W,
            "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
             <D:displayname>Work</D:displayname><D:sync-token>s</D:sync-token>",
        )]))
        .mount(&h.server)
        .await;
    h.mount_store().await;
    h.event(&format!("{W}e1.ics"), "1");
    h.mount_sync(
        "",
        xml207(ms_body(&[etag_resp(&format!("{W}e1.ics"), "1")], Some("t"))),
    )
    .await;
    let c = h.connector_with(json!({"url": format!("{}{W}", h.server.uri()), "username": "me"}));
    let cursor = c
        .sync(None, &*h.store, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(keys(&h.store.take_log(), true), vec![format!("{W}e1.ics")]);
    assert_eq!(cursor["calendars"][W]["sync_token"], "t");

    // Filter to "Tasks & Chores" by display name: Work's items become orphans and are deleted.
    let h2 = H::new().await;
    h2.mount_discovery().await;
    h2.mount_store().await;
    h2.store.seed(&format!("{W}e1.ics"), "\"1\"");
    let c = h2.connector_with(json!({
        "url": format!("{}/", h2.server.uri()), "username": "me", "calendars": ["tasks & chores"]
    }));
    let cursor = c
        .sync(None, &*h2.store, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        keys(&h2.store.take_log(), false),
        vec![format!("{W}e1.ics")]
    );
    assert_eq!(
        cursor["calendars"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec![T]
    );
}

#[tokio::test]
async fn egress_is_recorded_per_request_with_sync_caldav_feature() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    h.event(&format!("{T}t1.ics"), "a");
    h.mount_sync("", xml207(ms_body(&[], Some("t")))).await;
    h.sync(None).await.unwrap();
    let events = h.egress.0.lock().unwrap().clone();
    let n = h.server.received_requests().await.unwrap().len();
    assert_eq!(events.len(), n);
    let host = h.server.uri().trim_start_matches("http://").to_string();
    for e in &events {
        assert_eq!(e.feature, "sync:caldav");
        assert_eq!(e.host, host);
        assert!(e.bytes_sent > 0 && e.bytes_received > 0);
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let h = H::new().await;
    let other = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/dav/", other.uri())),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("PROPFIND"))
        .respond_with(ms207(&[]))
        .mount(&other)
        .await;
    let err = h.sync(None).await.unwrap_err();
    assert!(
        matches!(&err, Error::Network(m) if m.contains("redirect")),
        "{err:?}"
    );
    assert!(other.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cross_host_hrefs_are_refused() {
    let h = H::new().await;
    Mock::given(method("PROPFIND"))
        .and(path("/"))
        .respond_with(ms207(&[resp(
            "/",
            "<D:resourcetype><D:collection/></D:resourcetype>\
             <C:calendar-home-set><D:href>https://evil.example.com/cal/</D:href></C:calendar-home-set>",
        )]))
        .mount(&h.server)
        .await;
    let err = h.sync(None).await.unwrap_err();
    assert!(
        matches!(&err, Error::Invalid(m) if m.contains("evil.example.com")),
        "{err:?}"
    );
    assert_eq!(h.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn missing_password_or_401_is_unauthorized() {
    let h = H::new().await;
    h.secrets.delete(&password_account(SOURCE)).unwrap();
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert!(h.server.received_requests().await.unwrap().is_empty());

    h.secrets.set(&password_account(SOURCE), "bad").unwrap();
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(401).insert_header("www-authenticate", "Basic"))
        .mount(&h.server)
        .await;
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
}

#[tokio::test]
async fn empty_discovery_is_an_error_not_a_mass_delete() {
    let h = H::new().await;
    Mock::given(method("PROPFIND"))
        .and(path("/"))
        .and(header("depth", "0"))
        .respond_with(ms207(&[resp(
            "/",
            "<D:resourcetype><D:collection/></D:resourcetype>",
        )]))
        .mount(&h.server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/"))
        .and(header("depth", "1"))
        .respond_with(ms207(&[resp(
            "/",
            "<D:resourcetype><D:collection/></D:resourcetype>",
        )]))
        .mount(&h.server)
        .await;
    h.store.seed(&format!("{W}e1.ics"), "\"1\"");
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(h.store.take_log().is_empty());
}

#[tokio::test]
async fn malformed_server_xml_is_an_error() {
    let h = H::new().await;
    h.mount_discovery().await;
    h.mount_store().await;
    h.mount_sync(
        "",
        xml207("<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/x".into()),
    )
    .await;
    let err = h.sync(None).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

#[tokio::test]
async fn cancelled_sync_returns_cancelled() {
    let h = H::new().await;
    h.mount_discovery().await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = h
        .connector()
        .sync(None, &*h.store, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

// ---------------------------------------------------------------------------------------------
// Settings, URL policy, XML
// ---------------------------------------------------------------------------------------------

#[test]
fn factory_and_settings_validation() {
    let f = CalDavFactory::new();
    let info = f.info();
    assert_eq!(info.kind, "caldav");
    assert_eq!(info.auth, AuthKind::Password);
    assert_eq!(info.settings_schema["required"], json!(["url", "username"]));
    for ok in [
        json!({"url": "https://caldav.fastmail.com/dav/", "username": "u"}),
        json!({"url": "http://127.0.0.1:5232/u/", "username": "u"}),
        json!({"url": "http://localhost:5232", "username": "u"}),
        json!({"url": "http://[::1]:5232/", "username": "u", "calendars": ["Work"]}),
    ] {
        f.validate(&ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in [
        json!({"url": "http://caldav.example.com/", "username": "u"}),
        json!({"url": "ftp://example.com/", "username": "u"}),
        json!({"url": "https://u:p@example.com/", "username": "u"}),
        json!({"url": "not a url", "username": "u"}),
        json!({"url": "https://example.com/", "username": ""}),
        json!({"url": "https://example.com/", "username": "u", "calendars": []}),
        json!({"url": "https://example.com/", "username": "u", "password": "x"}),
    ] {
        assert!(f.validate(&bad).is_err(), "{bad}");
    }
    let s = CalDavSettings::parse(&json!({"url": "https://h.example/dav?x=1#f", "username": "u"}))
        .unwrap();
    assert_eq!(s.base_url().unwrap().as_str(), "https://h.example/dav/");
}

#[tokio::test]
async fn credential_host_policy_allows_only_same_host_or_https_siblings() {
    let h = H::new().await;
    let c = h.connector_with(json!({"url": "https://caldav.icloud.com/", "username": "u"}));
    let ok = |u: &str| c.check_url(&Url::parse(u).unwrap()).is_ok();
    assert!(ok("https://caldav.icloud.com/123/calendars/"));
    assert!(ok("https://p42-caldav.icloud.com/123/calendars/"));
    assert!(!ok("http://p42-caldav.icloud.com/"));
    assert!(!ok("https://p42-caldav.icloud.com:8443/"));
    assert!(!ok("https://evil.com/"));
    assert!(!ok("https://icloud.com.evil.com/"));
    assert!(!ok("https://user:pw@caldav.icloud.com/"));

    let c = h.connector_with(json!({"url": "https://fastmail.com/", "username": "u"}));
    assert!(
        c.check_url(&Url::parse("https://x.fastmail.com/").unwrap())
            .is_err()
    );
    assert!(
        c.check_url(&Url::parse("https://fastmail.com/a").unwrap())
            .is_ok()
    );
}

#[tokio::test]
async fn sibling_hosts_are_limited_to_known_partitioned_providers() {
    let h = H::new().await;
    // A dynamic-DNS name: every other `*.duckdns.org` belongs to someone else.
    let c = h.connector_with(json!({"url": "https://mycal.duckdns.org/dav/", "username": "u"}));
    let ok = |u: &str| c.check_url(&Url::parse(u).unwrap()).is_ok();
    assert!(ok("https://mycal.duckdns.org/dav/cal/"));
    assert!(!ok("https://attacker.duckdns.org/dav/"));
    // Likewise for any three-label base outside the allowlist.
    let c = h.connector_with(json!({"url": "https://dav.example.co.uk/", "username": "u"}));
    assert!(
        c.check_url(&Url::parse("https://evil.example.co.uk/").unwrap())
            .is_err()
    );
    // Hosts of one partitioned provider may not reach outside it.
    let c = h.connector_with(json!({"url": "https://caldav.icloud.com/", "username": "u"}));
    let ok = |u: &str| c.check_url(&Url::parse(u).unwrap()).is_ok();
    assert!(ok("https://p07-caldav.icloud.com/1/calendars/"));
    assert!(!ok("https://icloud.com.evil.org/"));
    assert!(!ok("https://xicloud.com/"));
}

#[test]
fn xml_resolves_namespaces_entities_and_cdata() {
    let doc = br#"<?xml version="1.0"?><!-- c --><m:multistatus xmlns:m="DAV:" xmlns="urn:ietf:params:xml:ns:caldav">
      <m:response><m:href>/a%20b/&amp;x.ics</m:href>
        <m:propstat><m:prop><calendar-data><![CDATA[BEGIN:VCALENDAR<>]]>&#x41;&#66;&lt;</calendar-data><m:getetag>"e"</m:getetag></m:prop>
        <m:status>HTTP/1.1 200 OK</m:status></m:propstat>
        <m:propstat><m:prop><m:displayname/></m:prop><m:status>HTTP/1.1 404 Not Found</m:status></m:propstat>
      </m:response><m:sync-token>t&amp;1</m:sync-token></m:multistatus>"#;
    let ms = xml::multistatus(doc).unwrap();
    assert_eq!(ms.sync_token.as_deref(), Some("t&1"));
    let r = &ms.responses[0];
    assert_eq!(r.href, "/a%20b/&x.ics");
    assert_eq!(r.propstats.len(), 2);
    assert_eq!(
        r.prop(CALDAV, "calendar-data").unwrap().text,
        "BEGIN:VCALENDAR<>AB<"
    );
    assert_eq!(r.prop_text(DAV, "getetag").as_deref(), Some("\"e\""));
    assert!(r.prop(DAV, "displayname").is_none());
    assert_eq!(
        xml::status_code("HTTP/1.1 507 Insufficient Storage"),
        Some(507)
    );
    assert_eq!(xml::status_code("garbage"), None);
    assert_eq!(xml::escape("<a&\"'>"), "&lt;a&amp;&quot;&apos;&gt;");
}

#[test]
fn hostile_xml_is_rejected_without_panicking() {
    let deep = format!("<a>{}{}</a>", "<b>".repeat(100), "</b>".repeat(100));
    let cases: Vec<Vec<u8>> = vec![
        // Entity expansion ("billion laughs") and external entities need a DTD: refused.
        br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]><D:multistatus xmlns:D="DAV:">&lol2;</D:multistatus>"#.to_vec(),
        br#"<!DOCTYPE x [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><x>&xxe;</x>"#.to_vec(),
        b"<x>&undefined;</x>".to_vec(),
        b"<x>&#0;</x>".to_vec(),
        b"<x>&#xFFFFFFFF;</x>".to_vec(),
        deep.into_bytes(),
        b"<a><b></a></b>".to_vec(),
        b"<a>".to_vec(),
        b"".to_vec(),
        b"just text".to_vec(),
        b"<a/><b/>".to_vec(),
        b"<p:a xmlns:q=\"x\"/>".to_vec(),
        vec![0xff, 0xfe, 0x00, b'<'],
        b"<a x='1' x='2'/>".to_vec(),
    ];
    for c in cases {
        assert!(xml::parse(&c).is_err(), "{:?}", String::from_utf8_lossy(&c));
    }
    // Not a multistatus.
    assert!(xml::multistatus(b"<x xmlns=\"DAV:\"/>").is_err());
    // Responses without an href are skipped, not fatal.
    let ms =
        xml::multistatus(b"<D:multistatus xmlns:D=\"DAV:\"><D:response/></D:multistatus>").unwrap();
    assert!(ms.responses.is_empty());
}

#[test]
fn a_long_namespace_is_not_copied_into_every_element() {
    // One 1 MiB namespace URI used by 100k empty elements: before, every element owned its own
    // copy of the URI (~100 GiB from a ~1.6 MB body).
    let long_ns = format!("urn:x:{}", "a".repeat(1024 * 1024));
    let mut doc = format!("<D:multistatus xmlns:D=\"DAV:\" xmlns:p=\"{long_ns}\">");
    doc.push_str(&"<p:a/>".repeat(100_000));
    doc.push_str("</D:multistatus>");
    let root = xml::parse(doc.as_bytes()).unwrap();
    assert_eq!(root.children.len(), 100_000);
    assert!(
        root.children
            .iter()
            .all(|c| c.ns.len() <= 256 && c.ns != long_ns)
    );
    // Ordinary namespaces are kept verbatim.
    assert!(root.is(DAV, "multistatus"));
}

#[test]
fn attributes_are_bounded() {
    let attrs: String = (0..100_001).map(|i| format!(" a{i}=\"\"")).collect();
    let doc = format!("<x{attrs}/>");
    let err = xml::parse(doc.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("too many attributes"), "{err}");
}

#[test]
fn cursor_round_trip_and_garbage() {
    assert_eq!(Cursor::load(Some(json!([1, 2]))), Cursor::fresh());
    assert_eq!(Cursor::load(Some(json!({"v": 9}))), Cursor::fresh());
    let c = json!({"v": 1, "calendars": {"/c/": {"sync_token": "t", "ctag": "x"}, "/d/": {}}});
    assert_eq!(Cursor::load(Some(c.clone())).to_value(), c);
}

#[test]
fn request_bodies_escape_untrusted_values() {
    let b = sync_collection_body("a<b>&c");
    assert!(b.contains("<D:sync-token>a&lt;b&gt;&amp;c</D:sync-token>"));
    xml::parse(b.as_bytes()).unwrap();
    let m = multiget_body(&["/c/a&b.ics".into(), "/c/<x>.ics".into()]);
    assert!(m.contains("<D:href>/c/a&amp;b.ics</D:href>"));
    let parsed = xml::parse(m.as_bytes()).unwrap();
    assert_eq!(parsed.children(DAV, "href").count(), 2);
}
