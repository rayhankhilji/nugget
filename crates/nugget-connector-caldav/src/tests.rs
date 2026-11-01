//! Against a fake RFC 6578 server: discovery, sync-token paging and expiry, the ETag fallback,
//! throttling, auth, size caps, crash resumption, and the URL / XML safety rules.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, VecEgress, assert_crash_resumable, context, run_pass,
    server_error, too_many_requests,
};
use nugget_connector::{AuthKind, Connector, ConnectorFactory, KnownItems, SecretStore};
use nugget_core::paths::Paths;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;

const SOURCE: &str = "caldav:test";
const CAL: &str = "/dav/cal/work/";
const ZZ: &str = "/dav/cal/zz/";

/// Server state. Tokens are `v<base>-o<offset>`: "changes after version `base`, from the
/// `offset`th"; the empty token is version 0 (everything).
struct Model {
    events: BTreeMap<String, (String, String)>,
    version: u64,
    changed: Vec<(u64, String)>,
    /// Tokens of versions below this are expired (`expired_status` valid-sync-token).
    expired_below: u64,
    expired_status: u16,
    page: usize,
    /// How multiget responses spell hrefs (servers encode differently).
    spell: fn(&str) -> String,
    /// List `getcontentlength` with each member.
    report_length: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            events: BTreeMap::new(),
            version: 0,
            changed: Vec::new(),
            expired_below: 0,
            expired_status: 403,
            page: 100,
            spell: |h| h.to_string(),
            report_length: false,
        }
    }
}

impl Model {
    fn put(&mut self, href: &str, ics: &str) {
        self.version += 1;
        let etag = format!("e{}", self.version);
        self.events
            .insert(href.to_string(), (etag, ics.to_string()));
        self.changed.push((self.version, href.to_string()));
    }

    fn delete(&mut self, href: &str) {
        self.version += 1;
        self.events.remove(href);
        self.changed.push((self.version, href.to_string()));
    }

    fn token(&self) -> String {
        format!("v{}-o0", self.version)
    }

    fn member(&self, h: &str, extra: &str) -> String {
        match self.events.get(h) {
            Some((etag, data)) => {
                let len = if self.report_length {
                    format!("<D:getcontentlength>{}</D:getcontentlength>", data.len())
                } else {
                    String::new()
                };
                resp(h, &format!("{extra}<D:getetag>\"{etag}\"</D:getetag>{len}"))
            }
            None => status_resp(h, 404),
        }
    }

    fn sync_response(&self, body: &str) -> ResponseTemplate {
        let token = body
            .split("<D:sync-token>")
            .nth(1)
            .and_then(|t| t.split("</D:sync-token>").next())
            .unwrap_or("");
        let Some((base, offset)) =
            parse_token(token).filter(|(b, _)| *b == 0 || *b >= self.expired_below)
        else {
            return ResponseTemplate::new(self.expired_status).set_body_string(
                "<?xml version=\"1.0\"?><D:error xmlns:D=\"DAV:\"><D:valid-sync-token/></D:error>",
            );
        };
        let hrefs: Vec<String> = if base == 0 {
            self.events.keys().cloned().collect()
        } else {
            let set: BTreeSet<_> = self
                .changed
                .iter()
                .filter(|(v, _)| *v > base)
                .map(|(_, h)| h.clone())
                .collect();
            set.into_iter().collect()
        };
        let end = (offset + self.page).min(hrefs.len());
        let mut out: Vec<String> = hrefs[offset.min(end)..end]
            .iter()
            .map(|h| self.member(h, ""))
            .collect();
        let next = if end < hrefs.len() {
            out.push(status_resp(CAL, 507));
            format!("v{base}-o{end}")
        } else {
            self.token()
        };
        ms(&out, Some(&next))
    }

    fn multiget_response(&self, body: &str) -> ResponseTemplate {
        let out: Vec<String> = body
            .split("<D:href>")
            .skip(1)
            .filter_map(|h| h.split("</D:href>").next())
            .map(|href| match self.events.get(href) {
                Some((etag, data)) => resp(
                    &(self.spell)(href),
                    &format!(
                        "<D:getetag>\"{etag}\"</D:getetag><C:calendar-data>{}</C:calendar-data>",
                        xml::escape(data)
                    ),
                ),
                None => status_resp(href, 404),
            })
            .collect();
        ms(&out, None)
    }
}

fn parse_token(t: &str) -> Option<(u64, usize)> {
    if t.is_empty() {
        return Some((0, 0));
    }
    let (b, o) = t.strip_prefix('v')?.split_once("-o")?;
    Some((b.parse().ok()?, o.parse().ok()?))
}

fn ics(uid: &str, summary: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         DTSTAMP:20260101T000000Z\r\nDTSTART:20260102T100000Z\r\nSUMMARY:{summary}\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
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

fn ms(responses: &[String], token: Option<&str>) -> ResponseTemplate {
    let token = token
        .map(|t| format!("<D:sync-token>{t}</D:sync-token>"))
        .unwrap_or_default();
    ResponseTemplate::new(207)
        .insert_header("content-type", "application/xml; charset=utf-8")
        .set_body_string(format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\" \
             xmlns:C=\"urn:ietf:params:xml:ns:caldav\" xmlns:CS=\"http://calendarserver.org/ns/\">\
             {}{token}</D:multistatus>",
            responses.concat()
        ))
}

const COLLECTION: &str = "<D:resourcetype><D:collection/></D:resourcetype>";
const CALENDAR: &str = "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>";

async fn propfind(server: &MockServer, at: &str, depth: &str, out: impl Respond + 'static) {
    Mock::given(method("PROPFIND"))
        .and(path(at))
        .and(header("depth", depth))
        .respond_with(out)
        .mount(server)
        .await;
}

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 4,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_secs(5),
    }
}

struct Env {
    server: MockServer,
    model: Arc<Mutex<Model>>,
    egress: Arc<VecEgress>,
    secrets: Arc<MemSecrets>,
}

impl Env {
    /// The model-backed calendar at [`CAL`].
    async fn new() -> Self {
        let server = MockServer::start().await;
        let model: Arc<Mutex<Model>> = Arc::default();
        let props = format!(
            "{CALENDAR}<D:displayname>Work</D:displayname><D:sync-token>any</D:sync-token>"
        );
        propfind(&server, CAL, "0", ms(&[resp(CAL, &props)], None)).await;
        for (kind, sync) in [("sync-collection", true), ("calendar-multiget", false)] {
            let m = model.clone();
            Mock::given(method("REPORT"))
                .and(path(CAL))
                .and(body_string_contains(kind))
                .respond_with(move |r: &Request| {
                    let m = m.lock().unwrap();
                    let body = String::from_utf8_lossy(&r.body);
                    if sync {
                        m.sync_response(&body)
                    } else {
                        m.multiget_response(&body)
                    }
                })
                .mount(&server)
                .await;
        }
        let m = model.clone();
        let listing = move |_: &Request| {
            let m = m.lock().unwrap();
            let mut out = vec![resp(CAL, COLLECTION)];
            out.extend(m.events.keys().map(|h| m.member(h, "<D:resourcetype/>")));
            ms(&out, None)
        };
        propfind(&server, CAL, "1", listing).await;
        let secrets = Arc::new(MemSecrets::default());
        secrets
            .set(&password_account(SOURCE), "app-password")
            .unwrap();
        Self {
            server,
            model,
            egress: Arc::new(VecEgress::default()),
            secrets,
        }
    }

    /// Discovery from `/`: principal → home `/dav/cal/` with [`CAL`], an ETag-only calendar
    /// [`ZZ`] ("Tasks & Chores", one event) and collections that are not synced.
    async fn mount_home(&self) {
        let s = &self.server;
        let principal =
            "<D:current-user-principal><D:href>/principals/me/</D:href></D:current-user-principal>";
        propfind(
            s,
            "/",
            "0",
            ms(&[resp("/", &format!("{COLLECTION}{principal}"))], None),
        )
        .await;
        let home = "<C:calendar-home-set><D:href>/dav/cal/</D:href></C:calendar-home-set>";
        propfind(
            s,
            "/principals/me/",
            "0",
            ms(&[resp("/principals/me/", home)], None),
        )
        .await;
        propfind(
            s,
            "/dav/cal/",
            "0",
            ms(&[resp("/dav/cal/", COLLECTION)], None),
        )
        .await;
        let comps = |c: &str| {
            format!(
                "<C:supported-calendar-component-set><C:comp name=\"{c}\"/></C:supported-calendar-component-set>"
            )
        };
        let listing = [
            resp("/dav/cal/", COLLECTION),
            resp(
                CAL,
                &format!(
                    "{CALENDAR}<D:displayname>Work</D:displayname><D:sync-token>any</D:sync-token>{}",
                    comps("VEVENT")
                ),
            ),
            resp(
                ZZ,
                &format!(
                    "{CALENDAR}<D:displayname>Tasks &amp; Chores</D:displayname><CS:getctag>z1</CS:getctag>{}",
                    comps("VTODO")
                ),
            ),
            resp(
                "/dav/cal/freebusy/",
                &format!("{CALENDAR}{}", comps("VFREEBUSY")),
            ),
            resp(
                "/dav/cal/contacts/",
                "<D:resourcetype><D:collection/><A:addressbook xmlns:A=\"urn:ietf:params:xml:ns:carddav\"/></D:resourcetype>",
            ),
            resp(
                "/dav/cal/inbox/",
                "<D:resourcetype><D:collection/><C:schedule-inbox/></D:resourcetype>",
            ),
        ];
        propfind(s, "/dav/cal/", "1", ms(&listing, None)).await;
        let a = format!("{ZZ}a.ics");
        let member = resp(&a, "<D:resourcetype/><D:getetag>\"z1\"</D:getetag>");
        propfind(s, ZZ, "1", ms(&[resp(ZZ, COLLECTION), member], None)).await;
        let data = format!(
            "<D:getetag>\"z1\"</D:getetag><C:calendar-data>{}</C:calendar-data>",
            xml::escape(&ics("z", "other calendar"))
        );
        Mock::given(method("REPORT"))
            .and(path(ZZ))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(ms(&[resp(&a, &data)], None))
            .mount(s)
            .await;
    }

    fn connector_with(&self, replica: Arc<Replica>, settings: Value) -> CalDavConnector {
        let ctx = context(
            SOURCE,
            Paths::rooted(std::env::temp_dir().join("nugget-caldav-tests")),
            self.egress.clone(),
            self.secrets.clone(),
            replica as Arc<dyn KnownItems>,
        );
        let mut c = CalDavConnector::new(CalDavSettings::parse(&settings).unwrap(), ctx).unwrap();
        c.retry = fast_retry();
        c
    }

    fn connector_at(&self, at: &str, replica: Arc<Replica>) -> CalDavConnector {
        let url = format!("{}{at}", self.server.uri());
        self.connector_with(replica, json!({"url": url, "username": "me"}))
    }

    fn connector(&self, replica: Arc<Replica>) -> CalDavConnector {
        self.connector_at(CAL, replica)
    }

    async fn pass(&self, replica: &Arc<Replica>, cursor: Option<Value>) -> (Value, ReplicaSink) {
        let conn = self.connector(replica.clone());
        let sink = ReplicaSink::new(replica.clone());
        let c = run_pass(&conn, cursor, &sink).await.expect("sync pass");
        (c, sink)
    }

    fn put(&self, i: usize, summary: &str) {
        self.model
            .lock()
            .unwrap()
            .put(&href(i), &ics(&format!("u{i}"), summary));
    }

    /// The replica must hold exactly the server's events, with their etags as versions.
    fn assert_mirrors(&self, replica: &Replica) {
        let m = self.model.lock().unwrap();
        assert_eq!(replica.keys(), m.events.keys().cloned().collect::<Vec<_>>());
        for (href, (etag, _)) in &m.events {
            let got = replica.get(href).unwrap();
            assert_eq!(got.version.as_deref(), Some(format!("\"{etag}\"").as_str()));
        }
    }

    /// Requests whose body contains all of `needles`.
    async fn requests(&self, needles: &[&str]) -> usize {
        let reqs = self.server.received_requests().await.unwrap();
        reqs.iter()
            .filter(|r| {
                let b = String::from_utf8_lossy(&r.body);
                needles.iter().all(|n| b.contains(n))
            })
            .count()
    }
}

fn href(i: usize) -> String {
    format!("{CAL}ev-{i:05}.ics")
}

async fn pass_ok(conn: &CalDavConnector, replica: &Arc<Replica>, cursor: Option<Value>) -> Value {
    run_pass(conn, cursor, &ReplicaSink::new(replica.clone()))
        .await
        .expect("sync pass")
}

async fn sync_err(conn: &CalDavConnector, replica: &Arc<Replica>, cursor: Option<Value>) -> Error {
    run_pass(conn, cursor, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err()
}

#[tokio::test]
async fn discovery_filters_collections_and_reads_both_sync_modes() {
    let env = Env::new().await;
    env.mount_home().await;
    env.put(0, "Standup");
    let replica = Replica::new();
    let conn = env.connector_at("/", replica.clone());
    let sink = ReplicaSink::new(replica.clone());
    let cursor = run_pass(&conn, None, &sink).await.unwrap();
    assert_eq!(replica.keys(), vec![href(0), format!("{ZZ}a.ics")]);
    let doc = sink
        .upserts()
        .into_iter()
        .find(|d| d.key.0 == href(0))
        .unwrap();
    assert_eq!(doc.mime, "text/calendar");
    let Content::Bytes(b) = &doc.content else {
        panic!()
    };
    assert!(String::from_utf8_lossy(b).contains("BEGIN:VEVENT\r\nUID:u0\r\n"));
    assert_eq!(doc.metadata["calendar"], "Work");
    assert_eq!(doc.metadata["calendar_href"], CAL);
    assert_eq!(doc.url, Some(format!("{}{}", env.server.uri(), href(0))));
    assert_eq!(cursor["calendars"][CAL]["sync_token"], "v1-o0");
    assert_eq!(cursor["calendars"][ZZ], json!({"ctag": "z1"}));
    assert_eq!(cursor["calendars"].as_object().unwrap().len(), 2);
    for r in env.server.received_requests().await.unwrap() {
        let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("Basic "));
    }
    // Unchanged ctag: the ETag-only calendar is not listed again.
    let listings = || async {
        let reqs = env.server.received_requests().await.unwrap();
        reqs.iter().filter(|r| r.url.path() == ZZ).count()
    };
    let before = listings().await;
    run_pass(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(listings().await, before);

    // Filtering to "tasks & chores" by display name: Work's items become orphans and go.
    let conn = env.connector_with(
        replica.clone(),
        json!({"url": format!("{}/", env.server.uri()), "username": "me", "calendars": ["tasks & chores"]}),
    );
    let cursor = pass_ok(&conn, &replica, None).await;
    assert_eq!(replica.keys(), vec![format!("{ZZ}a.ics")]);
    let cals: Vec<_> = cursor["calendars"].as_object().unwrap().keys().collect();
    assert_eq!(cals, vec![ZZ]);
}

#[tokio::test]
async fn sync_token_paging_across_many_pages_then_deltas() {
    let env = Env::new().await;
    env.model.lock().unwrap().page = 97;
    for i in 0..1234 {
        env.put(i, &format!("Event {i}"));
    }
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica);
    assert_eq!(
        env.requests(&["sync-collection"]).await,
        1234_usize.div_ceil(97)
    );
    assert_eq!(
        env.requests(&["calendar-multiget"]).await,
        1234_usize.div_ceil(97) * 2
    );
    assert_eq!(sink.sends(), 1234);
    assert_eq!(cursor["calendars"][CAL]["sync_token"], "v1234-o0");

    // Deltas over several pages: edits (new etags), deletes, additions.
    for i in (0..1234).step_by(5) {
        env.put(i, &format!("Edited {i}"));
    }
    for i in (1..1234).step_by(41) {
        env.model.lock().unwrap().delete(&href(i));
    }
    for i in 2000..2050 {
        env.put(i, "New");
    }
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    env.assert_mirrors(&replica);
    let deletes = sink.summary().iter().filter(|s| s.starts_with('-')).count();
    assert_eq!(deletes, (1..1234).step_by(41).count());
    // Incremental pages checkpoint as they go (resumable), plus the final commit.
    assert!(sink.checkpoint_calls() > 3, "{}", sink.checkpoint_calls());
    assert!(replica.get(&href(5)).unwrap().content.contains("Edited 5"));
    let n = env.server.received_requests().await.unwrap().len();
    env.egress.assert_one_row_per_request(n, FEATURE);
}

#[tokio::test]
async fn expired_sync_token_403_or_409_triggers_a_reconciling_full_resync() {
    for status in [403, 409] {
        let env = Env::new().await;
        {
            let mut m = env.model.lock().unwrap();
            m.page = 7;
            m.expired_status = status;
        }
        for i in 0..30 {
            env.put(i, "x");
        }
        let replica = Replica::new();
        let (cursor, _) = env.pass(&replica, None).await;
        {
            let mut m = env.model.lock().unwrap();
            // Deletions the server no longer reports once the token expired.
            for i in 0..10 {
                m.delete(&href(i));
            }
            m.put(&href(99), &ics("u99", "new"));
            m.expired_below = m.version + 1;
        }
        let (cursor, sink) = env.pass(&replica, Some(cursor)).await;
        env.assert_mirrors(&replica);
        let deletes = sink.summary().iter().filter(|s| s.starts_with('-')).count();
        assert_eq!(deletes, 10, "status {status}");
        // Unchanged events are not re-downloaded by the full listing.
        assert_eq!(sink.upserts().len(), 1, "status {status}");
        let tok = cursor["calendars"][CAL]["sync_token"].as_str().unwrap();
        assert_eq!(tok, env.model.lock().unwrap().token());
    }
}

#[tokio::test]
async fn throttling_429_and_503_with_retry_after_are_retried_with_backoff() {
    let env = Env::new().await;
    env.put(0, "x");
    // 429 (delta seconds), 503 (HTTP-date against the server's own Date, so a skewed local
    // clock does not matter), 502 (no hint).
    let failures = [
        too_many_requests(1),
        server_error(503, None)
            .insert_header("Date", "Wed, 21 Oct 2026 07:28:00 GMT")
            .insert_header("Retry-After", "Wed, 21 Oct 2026 07:28:01 GMT"),
        server_error(502, None),
    ];
    for (i, f) in failures.into_iter().enumerate() {
        Mock::given(method("REPORT"))
            .and(body_string_contains("sync-collection"))
            .respond_with(f)
            .up_to_n_times(1)
            .with_priority(i as u8 + 1)
            .mount(&env.server)
            .await;
    }
    let replica = Replica::new();
    let started = Instant::now();
    env.pass(&replica, None).await;
    // Both hints were honoured as floors (1 s each).
    assert!(started.elapsed() >= Duration::from_secs(2));
    env.assert_mirrors(&replica);
    assert_eq!(env.requests(&["sync-collection"]).await, 4);
}

#[tokio::test]
async fn long_retry_after_and_persistent_outages_surface_as_transient_errors() {
    let env = Env::new().await;
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(too_many_requests(3600))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&env.server)
        .await;
    let replica = Replica::new();
    let conn = env.connector(replica.clone());
    // A long hint is not waited out inline: ingest gets it and schedules the next pass.
    let err = sync_err(&conn, &replica, None).await;
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(3_600_000)
            }
        ),
        "{err:?}"
    );
    // A server that keeps failing: bounded attempts, then a transient error (never
    // "reconnect").
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(server_error(503, None))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let err = sync_err(&conn, &replica, None).await;
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    let attempts = 1 + fast_retry().max_retries as usize;
    assert_eq!(env.requests(&["sync-collection"]).await, 1 + attempts);
}

#[tokio::test]
async fn missing_password_or_401_is_unauthorized_and_deletes_nothing() {
    let env = Env::new().await;
    for i in 0..3 {
        env.put(i, "x");
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    let conn = env.connector(replica.clone());
    env.secrets.delete(&password_account(SOURCE)).unwrap();
    let n = env.server.received_requests().await.unwrap().len();
    let err = sync_err(&conn, &replica, Some(cursor.clone())).await;
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(env.server.received_requests().await.unwrap().len(), n);
    env.secrets
        .set(&password_account(SOURCE), "changed")
        .unwrap();

    // The multiget is refused mid-sync, then discovery is.
    env.put(9, "x");
    Mock::given(method("REPORT"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(ResponseTemplate::new(401).insert_header("www-authenticate", "Basic"))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let err = sync_err(&conn, &replica, Some(cursor.clone())).await;
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let err = sync_err(&conn, &replica, Some(cursor)).await;
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(replica.len(), 3);
}

#[tokio::test]
async fn bad_server_answers_are_errors_never_mass_deletes() {
    let env = Env::new().await;
    let replica = Replica::new();
    let mut seed = Document::new(href(0), "text/calendar", Content::Text("x".into()));
    seed.version = Some("\"1\"".into());
    replica.apply(&Change::Upsert(seed));

    // An account with no calendars.
    propfind(&env.server, "/", "0", ms(&[resp("/", COLLECTION)], None)).await;
    propfind(&env.server, "/", "1", ms(&[resp("/", COLLECTION)], None)).await;
    let err = sync_err(&env.connector_at("/", replica.clone()), &replica, None).await;
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");

    // Malformed XML.
    Mock::given(method("REPORT"))
        .respond_with(
            ms(&[], None).set_body_string("<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>/x"),
        )
        .with_priority(1)
        .mount(&env.server)
        .await;
    let conn = env.connector(replica.clone());
    let err = sync_err(&conn, &replica, None).await;
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert_eq!(replica.len(), 1);

    let cancel = CancellationToken::new();
    cancel.cancel();
    let sink = ReplicaSink::new(replica.clone());
    let err = conn.sync(None, &sink, &cancel).await.unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test]
async fn redirects_and_cross_host_hrefs_are_refused() {
    let env = Env::new().await;
    let other = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .respond_with(ms(&[], None))
        .mount(&other)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/moved/"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/dav/", other.uri())),
        )
        .mount(&env.server)
        .await;
    let replica = Replica::new();
    let err = sync_err(
        &env.connector_at("/moved/", replica.clone()),
        &replica,
        None,
    )
    .await;
    assert!(
        matches!(&err, Error::Network(m) if m.contains("redirect")),
        "{err:?}"
    );
    assert!(other.received_requests().await.unwrap().is_empty());

    let evil =
        "<C:calendar-home-set><D:href>https://evil.example.com/cal/</D:href></C:calendar-home-set>";
    propfind(
        &env.server,
        "/",
        "0",
        ms(&[resp("/", &format!("{COLLECTION}{evil}"))], None),
    )
    .await;
    let before = env.server.received_requests().await.unwrap().len();
    let err = sync_err(&env.connector_at("/", replica.clone()), &replica, None).await;
    assert!(
        matches!(&err, Error::Invalid(m) if m.contains("evil.example.com")),
        "{err:?}"
    );
    assert_eq!(
        env.server.received_requests().await.unwrap().len(),
        before + 1
    );
}

fn fold(line: &str) -> String {
    let chunks: Vec<_> = line
        .as_bytes()
        .chunks(74)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    chunks.join("\r\n ")
}

#[tokio::test]
async fn huge_calendar_objects_are_capped_without_stopping_the_sync() {
    let env = Env::new().await;
    let attach = fold(&format!(
        "ATTACH;FMTTYPE=image/png;ENCODING=BASE64;VALUE=BINARY:{}",
        "QUJD".repeat(10_000)
    ));
    let with_attachment = ics("att", "Has a big inline attachment").replace(
        "END:VEVENT",
        &format!("{attach}\r\nATTACH:https://example.com/link.pdf\r\nEND:VEVENT"),
    );
    let huge_text = ics("txt", "Huge").replace(
        "END:VEVENT",
        &format!(
            "{}\r\nEND:VEVENT",
            fold(&format!("DESCRIPTION:{}", "d".repeat(30_000)))
        ),
    );
    for i in 0..8 {
        env.put(i, "small");
    }
    {
        let mut m = env.model.lock().unwrap();
        m.put(&href(3), &with_attachment);
        m.put(&href(5), &huge_text);
        m.put(&href(6), &ics("dl", &"z".repeat(200_000)));
        m.put(&href(7), "   ");
    }
    let replica = Replica::new();
    let mut conn = env.connector(replica.clone());
    conn.limits = Limits {
        max_body: 100 * 1024,
        max_event: 16 * 1024,
    };
    conn.http = MeteredHttp::new(env.egress.clone(), FEATURE)
        .unwrap()
        .with_max_body(100 * 1024);
    pass_ok(&conn, &replica, None).await;
    // Too large (after stripping) or empty objects are skipped.
    assert_eq!(replica.keys(), [0, 1, 2, 3, 4].map(href));
    // The inline attachment was dropped; the link attachment and the text stayed.
    let att = replica.get(&href(3)).unwrap().content;
    assert!(att.len() < 2_000, "{}", att.len());
    assert!(att.contains("Has a big inline attachment"));
    assert!(att.contains("ATTACH:https://example.com/link.pdf"));
    assert!(!att.contains("QUJD"));
}

#[tokio::test]
async fn regression_multiget_href_spelling_differs_from_sync_collection() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        m.put(&format!("{CAL}abc%40google.com.ics"), &ics("a", "at sign"));
        m.put(&format!("{CAL}caf%C3%A9.ics"), &ics("b", "Café"));
        // The multiget answers with `@` unescaped and lowercase escapes.
        m.spell = |h| h.replace("%40", "@").replace("%C3%A9", "%c3%a9");
    }
    let replica = Replica::new();
    env.pass(&replica, None).await;
    env.assert_mirrors(&replica);
}

#[tokio::test]
async fn regression_bad_member_href_skips_the_item_not_the_sync() {
    let env = Env::new().await;
    env.put(0, "fine");
    // The sync listing names a member on another host and one with an unparseable href.
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(ms(
            &[
                resp(
                    "https://evil.example.com/dav/cal/work/x.ics",
                    "<D:getetag>\"1\"</D:getetag>",
                ),
                resp("http://[::1", "<D:getetag>\"2\"</D:getetag>"),
                resp(&href(0), "<D:getetag>\"e1\"</D:getetag>"),
            ],
            Some("v1-o0"),
        ))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let replica = Replica::new();
    env.pass(&replica, None).await;
    assert_eq!(replica.keys(), vec![href(0)]);
    assert_eq!(env.requests(&["evil.example.com"]).await, 0);
}

#[tokio::test]
async fn regression_truncated_paging_without_a_new_token_falls_back_to_etags() {
    let env = Env::new().await;
    env.mount_home().await;
    for i in 0..3 {
        env.put(i, "x");
    }
    let replica = Replica::new();
    let conn = env.connector_at("/dav/cal/", replica.clone());
    let cursor = pass_ok(&conn, &replica, None).await;
    assert_eq!(cursor["calendars"][CAL]["sync_token"], "v3-o0");
    assert_eq!(replica.len(), 4);
    // From now on the server truncates with the same token forever.
    Mock::given(method("REPORT"))
        .and(body_string_contains("<D:sync-token>v3-o0</D:sync-token>"))
        .respond_with(ms(&[status_resp(CAL, 507)], Some("v3-o0")))
        .with_priority(1)
        .mount(&env.server)
        .await;
    env.put(9, "new");
    env.model.lock().unwrap().delete(&href(0));
    pass_ok(&conn, &replica, Some(cursor)).await;
    let want = vec![href(1), href(2), href(9), format!("{ZZ}a.ics")];
    assert_eq!(replica.keys(), want);
    assert_eq!(env.requests(&["<D:sync-token>v3-o0"]).await, 1);
}

#[tokio::test]
async fn crash_at_every_send_and_checkpoint_resumes_to_the_same_index() {
    let env = Arc::new(Env::new().await);
    env.model.lock().unwrap().page = 4;
    for i in 0..10 {
        env.put(i, &format!("E{i}"));
    }
    let build = {
        let env = env.clone();
        move |r: Arc<Replica>| {
            let c: Arc<dyn Connector> = Arc::new(env.connector(r));
            async move { c }
        }
    };
    let report = assert_crash_resumable(&build, None, BTreeMap::new()).await;
    assert_eq!(report.sends, 10);

    // Incremental: edits, deletes and additions over several pages (checkpointed per page).
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    for i in [0, 3, 7] {
        env.put(i, "edited");
    }
    for i in [1, 8] {
        env.model.lock().unwrap().delete(&href(i));
    }
    for i in 20..24 {
        env.put(i, "new");
    }
    let report = assert_crash_resumable(&build, Some(cursor), replica.snapshot()).await;
    assert_eq!(report.sends, 9);
    assert!(report.checkpoints >= 3, "{report:?}");
}

fn small(env: &Env, replica: &Arc<Replica>) -> CalDavConnector {
    let mut conn = env.connector(replica.clone());
    conn.limits = Limits {
        max_body: 100 * 1024,
        max_event: 64 * 1024,
    };
    conn.http = MeteredHttp::new(env.egress.clone(), FEATURE)
        .unwrap()
        .with_max_body(100 * 1024);
    conn
}

#[tokio::test]
async fn regression_oversized_object_is_not_downloaded_again_until_it_changes() {
    let env = Env::new().await;
    env.model.lock().unwrap().page = 3;
    for i in 0..6 {
        env.put(i, "small");
    }
    env.model
        .lock()
        .unwrap()
        .put(&href(4), &ics("big", &"z".repeat(200_000)));
    let replica = Replica::new();
    let conn = small(&env, &replica);
    let cursor = pass_ok(&conn, &replica, None).await;
    assert_eq!(replica.len(), 5);
    let h4 = href(4);
    let first = env.requests(&["calendar-multiget", &h4]).await;
    assert!(first >= 1);
    assert!(
        cursor["calendars"][CAL]["skipped"][&h4].is_string(),
        "{cursor}"
    );
    // The token expires: the full relisting does not download it again (same ETag).
    {
        let mut m = env.model.lock().unwrap();
        m.expired_below = m.version + 1;
    }
    let cursor = pass_ok(&conn, &replica, Some(cursor)).await;
    assert_eq!(env.requests(&["calendar-multiget", &h4]).await, first);
    // Once it changes (and fits), it is fetched and the skip is forgotten.
    env.put(4, "now small");
    let cursor = pass_ok(&conn, &replica, Some(cursor)).await;
    assert!(replica.get(&h4).unwrap().content.contains("now small"));
    assert!(
        cursor["calendars"][CAL].get("skipped").is_none(),
        "{cursor}"
    );
}

#[tokio::test]
async fn regression_listed_content_length_avoids_the_oversized_download() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        m.report_length = true;
        m.put(&href(0), &ics("a", "small"));
        m.put(&href(1), &ics("big", &"z".repeat(200_000)));
    }
    let replica = Replica::new();
    pass_ok(&small(&env, &replica), &replica, None).await;
    assert_eq!(replica.keys(), vec![href(0)]);
    assert_eq!(env.requests(&["calendar-multiget", &href(1)]).await, 0);
    let received: u64 = env.egress.events().iter().map(|e| e.bytes_received).sum();
    assert!(received < 50_000, "{received}");
}

#[tokio::test]
async fn regression_unresolvable_href_keeps_the_known_item() {
    let env = Env::new().await;
    for i in 0..3 {
        env.put(i, "x");
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    // The token expires and the full listing names item 1 by an href we cannot use.
    {
        let mut m = env.model.lock().unwrap();
        m.expired_below = m.version + 1;
    }
    let etag = |i: usize| format!("<D:getetag>\"e{}\"</D:getetag>", i + 1);
    let elsewhere = format!("https://elsewhere.example.com{}", href(1));
    Mock::given(method("REPORT"))
        .and(body_string_contains("<D:sync-token></D:sync-token>"))
        .respond_with(ms(
            &[
                resp(&href(0), &etag(0)),
                resp(&elsewhere, &etag(1)),
                resp(&href(2), &etag(2)),
            ],
            Some("v3-o0"),
        ))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(replica.keys(), vec![href(0), href(1), href(2)]);
    assert!(sink.summary().iter().all(|s| !s.starts_with('-')));
}

#[test]
fn regression_href_comparison_keeps_escaped_slash_and_percent() {
    assert_eq!(href_bytes("/c/a%40b.ics"), href_bytes("/c/a@b.ics"));
    assert_eq!(
        href_bytes("/c/caf%c3%a9.ics"),
        href_bytes("/c/caf%C3%A9.ics")
    );
    assert_ne!(href_bytes("/c/a%2Fb.ics"), href_bytes("/c/a/b.ics"));
    assert_eq!(href_bytes("/c/a%2fb.ics"), href_bytes("/c/a%2Fb.ics"));
    assert_ne!(href_bytes("/c/a%2540.ics"), href_bytes("/c/a%40.ics"));
    assert_eq!(
        raw_href_path("https://h.example.com/c/x.ics?q#f"),
        href_bytes("/c/x.ics")
    );
}

#[test]
fn factory_and_settings_validation() {
    let f = CalDavFactory::new();
    assert_eq!(f.info().auth, AuthKind::Password);
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
async fn credentials_go_only_to_the_same_host_or_a_known_partitioned_provider() {
    let env = Env::new().await;
    let check = |base: &str, cases: &[(&str, bool)]| {
        let c = env.connector_with(Replica::new(), json!({"url": base, "username": "u"}));
        for (u, ok) in cases {
            assert_eq!(
                c.check_url(&Url::parse(u).unwrap()).is_ok(),
                *ok,
                "{base} → {u}"
            );
        }
    };
    check(
        "https://caldav.icloud.com/",
        &[
            ("https://caldav.icloud.com/123/calendars/", true),
            ("https://p42-caldav.icloud.com/123/calendars/", true),
            ("http://p42-caldav.icloud.com/", false),
            ("https://p42-caldav.icloud.com:8443/", false),
            ("https://evil.com/", false),
            ("https://icloud.com.evil.com/", false),
            ("https://xicloud.com/", false),
            ("https://user:pw@caldav.icloud.com/", false),
        ],
    );
    check(
        "https://fastmail.com/",
        &[
            ("https://fastmail.com/a", true),
            ("https://x.fastmail.com/", false),
        ],
    );
    // A dynamic-DNS name: every other `*.duckdns.org` belongs to someone else.
    check(
        "https://mycal.duckdns.org/dav/",
        &[
            ("https://mycal.duckdns.org/dav/cal/", true),
            ("https://attacker.duckdns.org/dav/", false),
        ],
    );
    check(
        "https://dav.example.co.uk/",
        &[("https://evil.example.co.uk/", false)],
    );
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
}

#[test]
fn hostile_xml_is_rejected_without_panicking() {
    let deep = format!("<a>{}{}</a>", "<b>".repeat(100), "</b>".repeat(100));
    let attrs: String = (0..100_001).map(|i| format!(" a{i}=\"\"")).collect();
    let cases: Vec<Vec<u8>> = vec![
        // Entity expansion ("billion laughs") and external entities need a DTD: refused.
        br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]><D:multistatus xmlns:D="DAV:">&lol2;</D:multistatus>"#.to_vec(),
        br#"<!DOCTYPE x [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><x>&xxe;</x>"#.to_vec(),
        b"<x>&undefined;</x>".to_vec(),
        b"<x>&#0;</x>".to_vec(),
        b"<x>&#xFFFFFFFF;</x>".to_vec(),
        deep.into_bytes(),
        format!("<x{attrs}/>").into_bytes(),
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
        assert!(
            xml::parse(&c).is_err(),
            "{:?}",
            String::from_utf8_lossy(&c[..c.len().min(80)])
        );
    }
    assert!(xml::multistatus(b"<x xmlns=\"DAV:\"/>").is_err());
    // Responses without an href are skipped, not fatal.
    let ms =
        xml::multistatus(b"<D:multistatus xmlns:D=\"DAV:\"><D:response/></D:multistatus>").unwrap();
    assert!(ms.responses.is_empty());
}

#[test]
fn regression_a_long_namespace_is_not_copied_into_every_element() {
    // One 1 MiB namespace URI used by 100k empty elements once cost ~100 GiB.
    let long_ns = format!("urn:x:{}", "a".repeat(1024 * 1024));
    let doc = format!(
        "<D:multistatus xmlns:D=\"DAV:\" xmlns:p=\"{long_ns}\">{}</D:multistatus>",
        "<p:a/>".repeat(100_000)
    );
    let root = xml::parse(doc.as_bytes()).unwrap();
    assert_eq!(root.children.len(), 100_000);
    assert!(root.children.iter().all(|c| c.ns.len() <= 256));
    assert!(root.is(DAV, "multistatus"));
}

#[test]
fn cursor_garbage_resyncs_and_request_bodies_escape_untrusted_values() {
    assert_eq!(Cursor::load(Some(json!([1, 2]))), Cursor::fresh());
    assert_eq!(Cursor::load(Some(json!({"v": 9}))), Cursor::fresh());
    let b = sync_collection_body("a<b>&c");
    assert!(b.contains("<D:sync-token>a&lt;b&gt;&amp;c</D:sync-token>"));
    xml::parse(b.as_bytes()).unwrap();
    let m = multiget_body(["/c/a&b.ics", "/c/<x>.ics"].into_iter());
    assert!(m.contains("<D:href>/c/a&amp;b.ics</D:href>"));
    assert_eq!(
        xml::parse(m.as_bytes())
            .unwrap()
            .children(DAV, "href")
            .count(),
        2
    );
}

#[tokio::test]
async fn put_event_creates_once_in_the_first_event_calendar() {
    let env = Env::new().await;
    env.mount_home().await;
    let created = format!("{CAL}abc-1.ics");
    Mock::given(method("PUT"))
        .and(path(created.as_str()))
        .and(header("if-none-match", "*"))
        .and(header("content-type", "text/calendar; charset=utf-8"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&env.server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(412))
        .mount(&env.server)
        .await;
    let conn = env.connector_at("/", Replica::new());
    let cancel = CancellationToken::new();
    let body = "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";
    let url = conn.put_event("abc-1.ics", body, &cancel).await.unwrap();
    assert!(url.ends_with(&created), "{url}");
    let puts: Vec<_> = env
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].body, body.as_bytes());
    assert!(
        env.egress
            .events()
            .iter()
            .any(|e| e.feature == ACTION_FEATURE)
    );
    let err = conn.put_event("dup.ics", "x", &cancel).await.unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    // Bad resource names never reach the network.
    for bad in ["", "../x.ics", "a/b.ics", ".hidden"] {
        let err = conn.put_event(bad, body, &cancel).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{bad}: {err}");
    }
}
