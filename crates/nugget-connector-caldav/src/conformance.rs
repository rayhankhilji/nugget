//! Conformance: the shared robustness properties (`nugget_connector::testkit`) against a fake
//! RFC 6578 CalDAV server: sync-token paging, expired tokens, throttling with `Retry-After`,
//! 401, deletes, moves and edits, huge objects, unicode, recurrence and time zones, malformed
//! members, and resumption after a crash at any send or checkpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nugget_connector::testkit::{
    MemSecrets, Replica, ReplicaSink, UNICODE_SAMPLES, VecEgress, assert_crash_resumable, context,
    run_pass, server_error, too_many_requests,
};
use nugget_connector::{Connector, KnownItems, SecretStore};
use nugget_core::Error;
use nugget_core::paths::Paths;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

const SOURCE: &str = "caldav:conformance";
const CAL: &str = "/dav/cal/work/";

// ---------------------------------------------------------------------------------------------
// A fake RFC 6578 server
// ---------------------------------------------------------------------------------------------

/// Server state. Tokens are `v<base>-o<offset>`: "changes after version `base`, from the
/// `offset`th"; the empty token is version 0 (everything).
struct Model {
    events: BTreeMap<String, (String, String)>,
    version: u64,
    changed: Vec<(u64, String)>,
    /// Tokens of versions below this are expired (403 valid-sync-token).
    expired_below: u64,
    expired_status: u16,
    page: usize,
    /// How multiget responses spell hrefs (to mimic servers that encode differently).
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
        self.events.insert(
            href.to_string(),
            (format!("e{}", self.version), ics.to_string()),
        );
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
}

type Shared = Arc<Mutex<Model>>;

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
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">{}{token}</D:multistatus>",
            responses.concat()
        ))
}

fn parse_token(t: &str) -> Option<(u64, usize)> {
    if t.is_empty() {
        return Some((0, 0));
    }
    let (b, o) = t.strip_prefix('v')?.split_once("-o")?;
    Some((b.parse().ok()?, o.parse().ok()?))
}

fn sync_response(m: &Model, body: &str) -> ResponseTemplate {
    let token = body
        .split("<D:sync-token>")
        .nth(1)
        .and_then(|t| t.split("</D:sync-token>").next())
        .unwrap_or("");
    let Some((base, offset)) = parse_token(token).filter(|(b, _)| *b == 0 || *b >= m.expired_below)
    else {
        return ResponseTemplate::new(m.expired_status).set_body_string(
            "<?xml version=\"1.0\"?><D:error xmlns:D=\"DAV:\"><D:valid-sync-token/></D:error>",
        );
    };
    let hrefs: Vec<String> = if base == 0 {
        m.events.keys().cloned().collect()
    } else {
        m.changed
            .iter()
            .filter(|(v, _)| *v > base)
            .map(|(_, h)| h.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let end = (offset + m.page).min(hrefs.len());
    let mut out: Vec<String> = hrefs[offset.min(end)..end]
        .iter()
        .map(|h| match m.events.get(h) {
            Some((etag, data)) if m.report_length => resp(
                h,
                &format!(
                    "<D:getetag>\"{etag}\"</D:getetag><D:getcontentlength>{}</D:getcontentlength>",
                    data.len()
                ),
            ),
            Some((etag, _)) => resp(h, &format!("<D:getetag>\"{etag}\"</D:getetag>")),
            None => status_resp(h, 404),
        })
        .collect();
    let next = if end < hrefs.len() {
        out.push(status_resp(CAL, 507));
        format!("v{base}-o{end}")
    } else {
        m.token()
    };
    ms(&out, Some(&next))
}

fn multiget_response(m: &Model, body: &str) -> ResponseTemplate {
    let out: Vec<String> = body
        .split("<D:href>")
        .skip(1)
        .filter_map(|h| h.split("</D:href>").next())
        .map(|href| match m.events.get(href) {
            Some((etag, data)) => resp(
                &(m.spell)(href),
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

struct Env {
    server: MockServer,
    model: Shared,
    egress: Arc<VecEgress>,
    secrets: Arc<MemSecrets>,
}

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        attempts: 5,
        base: Duration::from_millis(5),
        max: Duration::from_secs(5),
    }
}

impl Env {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let model: Shared = Arc::default();
        Mock::given(method("PROPFIND"))
            .and(path(CAL))
            .and(header("depth", "0"))
            .respond_with(ms(
                &[resp(
                    CAL,
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
                     <D:displayname>Work</D:displayname><D:sync-token>any</D:sync-token>",
                )],
                None,
            ))
            .mount(&server)
            .await;
        let m = model.clone();
        Mock::given(method("REPORT"))
            .and(path(CAL))
            .and(body_string_contains("sync-collection"))
            .respond_with(move |r: &Request| {
                sync_response(&m.lock().unwrap(), &String::from_utf8_lossy(&r.body))
            })
            .mount(&server)
            .await;
        let m = model.clone();
        Mock::given(method("REPORT"))
            .and(path(CAL))
            .and(body_string_contains("calendar-multiget"))
            .respond_with(move |r: &Request| {
                multiget_response(&m.lock().unwrap(), &String::from_utf8_lossy(&r.body))
            })
            .mount(&server)
            .await;
        let m = model.clone();
        Mock::given(method("PROPFIND"))
            .and(path(CAL))
            .and(header("depth", "1"))
            .respond_with(move |_: &Request| {
                let m = m.lock().unwrap();
                let mut out = vec![resp(
                    CAL,
                    "<D:resourcetype><D:collection/></D:resourcetype>",
                )];
                for (h, (etag, data)) in &m.events {
                    let len = if m.report_length {
                        format!("<D:getcontentlength>{}</D:getcontentlength>", data.len())
                    } else {
                        String::new()
                    };
                    out.push(resp(
                        h,
                        &format!("<D:resourcetype/><D:getetag>\"{etag}\"</D:getetag>{len}"),
                    ));
                }
                ms(&out, None)
            })
            .mount(&server)
            .await;
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

    fn connector(&self, replica: Arc<Replica>) -> CalDavConnector {
        let ctx = context(
            SOURCE,
            Paths::rooted(std::env::temp_dir().join("nugget-caldav-conformance")),
            self.egress.clone(),
            self.secrets.clone(),
            replica as Arc<dyn KnownItems>,
        );
        let settings = json!({"url": format!("{}{CAL}", self.server.uri()), "username": "me"});
        CalDavConnector::new(CalDavSettings::parse(&settings).unwrap(), ctx)
            .unwrap()
            .with_retry(fast_retry())
    }

    async fn pass(&self, replica: &Arc<Replica>, cursor: Option<Value>) -> (Value, ReplicaSink) {
        let conn = self.connector(replica.clone());
        let sink = ReplicaSink::new(replica.clone());
        let c = run_pass(&conn, cursor, &sink).await.expect("sync pass");
        (c, sink)
    }

    /// The replica must hold exactly the server's events, with their etags as versions.
    fn assert_mirrors(&self, replica: &Replica) {
        let m = self.model.lock().unwrap();
        let want: Vec<String> = m.events.keys().cloned().collect();
        assert_eq!(replica.keys(), want);
        for (href, (etag, _)) in &m.events {
            let got = replica.get(href).unwrap();
            assert_eq!(got.version.as_deref(), Some(format!("\"{etag}\"").as_str()));
        }
    }

    async fn requests(&self, needle: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains(needle))
            .count()
    }
}

fn href(i: usize) -> String {
    format!("{CAL}ev-{i:05}.ics")
}

// ---------------------------------------------------------------------------------------------
// Paging, tokens, deltas
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn sync_token_paging_across_many_pages_then_deltas() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        m.page = 97;
        for i in 0..1234 {
            m.put(&href(i), &ics(&format!("u{i}"), &format!("Event {i}")));
        }
    }
    let replica = Replica::new();
    let (cursor, sink) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica);
    assert_eq!(
        env.requests("sync-collection").await,
        1234_usize.div_ceil(97)
    );
    assert_eq!(
        env.requests("calendar-multiget").await,
        1234_usize.div_ceil(97) * 2
    );
    assert_eq!(sink.sends(), 1234);
    assert_eq!(cursor["calendars"][CAL]["sync_token"], "v1234-o0");

    // Deltas spanning several pages: edits (new etags), deletes, additions.
    {
        let mut m = env.model.lock().unwrap();
        for i in (0..1234).step_by(5) {
            m.put(&href(i), &ics(&format!("u{i}"), &format!("Edited {i}")));
        }
        for i in (1..1234).step_by(41) {
            m.delete(&href(i));
        }
        for i in 2000..2050 {
            m.put(&href(i), &ics(&format!("u{i}"), "New"));
        }
    }
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    env.assert_mirrors(&replica);
    let deletes = sink.summary().iter().filter(|s| s.starts_with('-')).count();
    assert_eq!(deletes, (1..1234).step_by(41).count());
    // Incremental pages checkpoint as they go (resumable), plus the final commit.
    assert!(sink.checkpoint_calls() > 3, "{}", sink.checkpoint_calls());
    assert!(
        replica.get(&href(5)).unwrap().content.contains("Edited 5"),
        "edit applied"
    );
    // Every request is in the egress ledger under the sync feature.
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
            for i in 0..30 {
                m.put(&href(i), &ics(&format!("u{i}"), "x"));
            }
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
async fn deletions_moves_renames_and_edited_etags() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        for i in 0..5 {
            m.put(&href(i), &ics(&format!("u{i}"), &format!("E{i}")));
        }
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    {
        let mut m = env.model.lock().unwrap();
        m.delete(&href(0));
        // Moved to another resource name (or "renamed" by a client): same object, new href.
        let moved = m.events[&href(1)].1.clone();
        m.delete(&href(1));
        m.put(&format!("{CAL}renamed-1.ics"), &moved);
        // Edited: the etag changes; a summary change ("rename" of the event title).
        m.put(&href(2), &ics("u2", "Renamed title"));
    }
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    env.assert_mirrors(&replica);
    let mut s = sink.summary();
    s.sort();
    assert_eq!(
        s,
        vec![
            format!("+{CAL}ev-00002.ics"),
            format!("+{CAL}renamed-1.ics"),
            format!("-{CAL}ev-00000.ics"),
            format!("-{CAL}ev-00001.ics"),
        ]
    );
    assert!(
        replica
            .get(&href(2))
            .unwrap()
            .content
            .contains("Renamed title")
    );
}

// ---------------------------------------------------------------------------------------------
// Throttling and auth
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn throttling_429_and_503_with_retry_after_are_retried_with_backoff() {
    let env = Env::new().await;
    env.model.lock().unwrap().put(&href(0), &ics("u0", "x"));
    // The first three sync REPORTs fail: 429 (delta seconds), 503 (HTTP-date measured against
    // the server's own Date header, so a skewed local clock does not matter), 502 (no hint).
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(too_many_requests(1))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&env.server)
        .await;
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(
            server_error(503, None)
                .insert_header("Date", "Wed, 21 Oct 2026 07:28:00 GMT")
                .insert_header("Retry-After", "Wed, 21 Oct 2026 07:28:01 GMT"),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&env.server)
        .await;
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(server_error(502, None))
        .up_to_n_times(1)
        .with_priority(3)
        .mount(&env.server)
        .await;
    let replica = Replica::new();
    let started = Instant::now();
    env.pass(&replica, None).await;
    // Both hints were honoured as floors (1 s each).
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    env.assert_mirrors(&replica);
    assert_eq!(env.requests("sync-collection").await, 4);
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
    let sink = ReplicaSink::new(replica.clone());
    // A long hint is not waited out inline: ingest gets it and schedules the next pass.
    let err = run_pass(&conn, None, &sink).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(3_600_000)
            }
        ),
        "{err:?}"
    );
    assert_eq!(env.requests("sync-collection").await, 1);

    // A server that keeps failing: bounded attempts, then a transient error (never
    // "reconnect"), and nothing was deleted.
    Mock::given(method("REPORT"))
        .and(body_string_contains("sync-collection"))
        .respond_with(server_error(503, None))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let err = run_pass(&conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err:?}");
    assert_eq!(
        env.requests("sync-collection").await,
        1 + fast_retry().attempts as usize
    );
}

#[tokio::test]
async fn unauthorized_401_at_discovery_or_mid_sync() {
    let env = Env::new().await;
    for i in 0..3 {
        env.model.lock().unwrap().put(&href(i), &ics("u", "x"));
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    env.model.lock().unwrap().put(&href(9), &ics("u9", "x"));
    // Password changed: the multiget is refused mid-sync.
    Mock::given(method("REPORT"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(ResponseTemplate::new(401).insert_header("www-authenticate", "Basic"))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let conn = env.connector(replica.clone());
    let err = run_pass(
        &conn,
        Some(cursor.clone()),
        &ReplicaSink::new(replica.clone()),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(replica.len(), 3, "nothing deleted on an auth failure");
    // And at discovery.
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let err = run_pass(&conn, Some(cursor), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unauthorized(_)), "{err:?}");
    assert_eq!(replica.len(), 3);
}

// ---------------------------------------------------------------------------------------------
// Content: size, unicode, recurrence, time zones, malformed members
// ---------------------------------------------------------------------------------------------

fn fold(line: &str) -> String {
    let b = line.as_bytes();
    b.chunks(74)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("\r\n ")
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
    let huge_text = ics("txt", "Huge description").replace(
        "END:VEVENT",
        &format!(
            "{}\r\nEND:VEVENT",
            fold(&format!("DESCRIPTION:{}", "d".repeat(30_000)))
        ),
    );
    let too_big_to_download = ics("dl", &"z".repeat(200_000));
    {
        let mut m = env.model.lock().unwrap();
        for i in 0..8 {
            m.put(&href(i), &ics(&format!("u{i}"), "small"));
        }
        m.put(&href(3), &with_attachment);
        m.put(&href(5), &huge_text);
        m.put(&href(6), &too_big_to_download);
    }
    let replica = Replica::new();
    let conn = env
        .connector(replica.clone())
        .with_limits(Limits {
            max_body: 100 * 1024,
            max_event: 16 * 1024,
        })
        .unwrap();
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&conn, None, &sink).await.expect("sync completes");
    let keys = replica.keys();
    assert_eq!(keys.len(), 6, "{keys:?}");
    assert!(!keys.contains(&href(5)) && !keys.contains(&href(6)));
    // The inline attachment was dropped; the link attachment and the text stayed.
    let att = replica.get(&href(3)).unwrap().content;
    assert!(att.len() < 2_000, "{}", att.len());
    assert!(att.contains("Has a big inline attachment"));
    assert!(att.contains("ATTACH:https://example.com/link.pdf"));
    assert!(!att.contains("QUJD"));
}

#[tokio::test]
async fn unicode_summaries_and_hrefs() {
    let env = Env::new().await;
    let names = [
        "caf%C3%A9-%F0%9F%8E%89.ics",
        "id%40google.com.ics",
        "with%20space.ics",
        "plain.ics",
    ];
    {
        let mut m = env.model.lock().unwrap();
        for (i, n) in names.iter().enumerate() {
            let s = UNICODE_SAMPLES[i % UNICODE_SAMPLES.len()];
            m.put(&format!("{CAL}{n}"), &ics(&format!("uid-{i}"), s));
        }
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    env.assert_mirrors(&replica);
    for (i, n) in names.iter().enumerate() {
        let got = replica.get(&format!("{CAL}{n}")).unwrap().content;
        assert!(
            got.contains(UNICODE_SAMPLES[i % UNICODE_SAMPLES.len()]),
            "{got}"
        );
    }
    let (_, quiet) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(quiet.sends(), 0);
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
async fn recurrence_and_time_zones_pass_through_intact() {
    let env = Env::new().await;
    let tz = "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\n\
              TZOFFSETTO:+0200\r\nTZNAME:CEST\r\nDTSTART:19700329T020000\r\n\
              RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\n\
              TZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nTZNAME:CET\r\nDTSTART:19701025T030000\r\n\
              RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
    let cases = [
        // Weekly series across the DST switch, one occurrence excluded, one moved.
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{tz}BEGIN:VEVENT\r\nUID:series\r\n\
             DTSTART;TZID=Europe/Berlin:20261019T090000\r\nDTEND;TZID=Europe/Berlin:20261019T093000\r\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=6\r\nEXDATE;TZID=Europe/Berlin:20261026T090000\r\n\
             SUMMARY:Standup\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series\r\n\
             RECURRENCE-ID;TZID=Europe/Berlin:20261102T090000\r\n\
             DTSTART;TZID=Europe/Berlin:20261102T110000\r\nSUMMARY:Standup (moved)\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        ),
        // All-day and floating time.
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:allday\r\n\
         DTSTART;VALUE=DATE:20261231\r\nDTEND;VALUE=DATE:20270101\r\nSUMMARY:NYE\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nUID:floating\r\nDTSTART:20261231T230000\r\nSUMMARY:Floating\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
            .to_string(),
        // A long folded line with multi-byte characters.
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:folded\r\n{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            fold(&format!("DESCRIPTION:{}", "naïve café ".repeat(20)))
        ),
    ];
    {
        let mut m = env.model.lock().unwrap();
        for (i, c) in cases.iter().enumerate() {
            m.put(&href(i), c);
        }
    }
    let replica = Replica::new();
    env.pass(&replica, None).await;
    for (i, c) in cases.iter().enumerate() {
        let got = replica.get(&href(i)).unwrap();
        assert_eq!(
            got.content, *c,
            "object {i} must reach extraction byte for byte"
        );
        assert_eq!(got.mime, "text/calendar");
    }
}

#[tokio::test]
async fn malformed_members_never_stop_the_sync() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        m.put(&href(0), &ics("ok0", "fine"));
        m.put(&href(1), "BEGIN:VEVENT\r\nthis is not iCalendar\r\n");
        m.put(&href(2), &ics("ok2", "fine"));
        m.put(&href(3), "   ");
    }
    let replica = Replica::new();
    env.pass(&replica, None).await;
    // The connector does not parse objects: a malformed one is passed on for extraction to
    // judge (where it fails alone); an empty one is skipped.
    assert_eq!(replica.keys(), vec![href(0), href(1), href(2)]);
}

#[tokio::test]
async fn regression_bad_member_href_skips_the_item_not_the_sync() {
    let env = Env::new().await;
    env.model.lock().unwrap().put(&href(0), &ics("ok", "fine"));
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
    // No request ever went to the other host (it is not even resolvable here).
    for r in env.server.received_requests().await.unwrap() {
        assert!(!String::from_utf8_lossy(&r.body).contains("evil.example.com"));
    }
}

/// The account root `/dav/cal/` holding the model-backed `work` calendar (sync-collection) and
/// an ETag-only `zz` calendar with one event.
async fn mount_home_with_two_calendars(env: &Env) {
    Mock::given(method("PROPFIND"))
        .and(path("/dav/cal/"))
        .and(header("depth", "0"))
        .respond_with(ms(
            &[resp(
                "/dav/cal/",
                "<D:resourcetype><D:collection/></D:resourcetype>",
            )],
            None,
        ))
        .mount(&env.server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/dav/cal/"))
        .and(header("depth", "1"))
        .respond_with(ms(
            &[
                resp(
                    "/dav/cal/",
                    "<D:resourcetype><D:collection/></D:resourcetype>",
                ),
                resp(
                    CAL,
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
                     <D:sync-token>any</D:sync-token>",
                ),
                resp(
                    "/dav/cal/zz/",
                    "<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>",
                ),
            ],
            None,
        ))
        .mount(&env.server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/dav/cal/zz/"))
        .and(header("depth", "1"))
        .respond_with(ms(
            &[
                resp(
                    "/dav/cal/zz/",
                    "<D:resourcetype><D:collection/></D:resourcetype>",
                ),
                resp(
                    "/dav/cal/zz/a.ics",
                    "<D:resourcetype/><D:getetag>\"z1\"</D:getetag>",
                ),
            ],
            None,
        ))
        .mount(&env.server)
        .await;
    Mock::given(method("REPORT"))
        .and(path("/dav/cal/zz/"))
        .and(body_string_contains("calendar-multiget"))
        .respond_with(ms(
            &[resp(
                "/dav/cal/zz/a.ics",
                &format!(
                    "<D:getetag>\"z1\"</D:getetag><C:calendar-data>{}</C:calendar-data>",
                    xml::escape(&ics("z", "other calendar"))
                ),
            )],
            None,
        ))
        .mount(&env.server)
        .await;
}

impl Env {
    fn home_connector(&self, replica: Arc<Replica>) -> CalDavConnector {
        let ctx = context(
            SOURCE,
            Paths::rooted(std::env::temp_dir().join("nugget-caldav-conformance")),
            self.egress.clone(),
            self.secrets.clone(),
            replica as Arc<dyn KnownItems>,
        );
        let settings = json!({"url": format!("{}/dav/cal/", self.server.uri()), "username": "me"});
        CalDavConnector::new(CalDavSettings::parse(&settings).unwrap(), ctx)
            .unwrap()
            .with_retry(fast_retry())
    }
}

#[tokio::test]
async fn regression_truncated_paging_without_a_new_token_falls_back_to_etags() {
    let env = Env::new().await;
    mount_home_with_two_calendars(&env).await;
    for i in 0..3 {
        env.model
            .lock()
            .unwrap()
            .put(&href(i), &ics(&format!("u{i}"), "x"));
    }
    let replica = Replica::new();
    let conn = env.home_connector(replica.clone());
    let sink = ReplicaSink::new(replica.clone());
    let cursor = run_pass(&conn, None, &sink).await.unwrap();
    assert_eq!(cursor["calendars"][CAL]["sync_token"], "v3-o0");
    assert_eq!(replica.len(), 4);
    // From now on the server truncates with the same token forever.
    Mock::given(method("REPORT"))
        .and(body_string_contains("<D:sync-token>v3-o0</D:sync-token>"))
        .respond_with(ms(&[status_resp(CAL, 507)], Some("v3-o0")))
        .with_priority(1)
        .mount(&env.server)
        .await;
    env.model.lock().unwrap().put(&href(9), &ics("u9", "new"));
    env.model.lock().unwrap().delete(&href(0));
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&conn, Some(cursor), &sink)
        .await
        .expect("the calendar is listed by ETag; the pass completes");
    let mut keys = replica.keys();
    keys.sort();
    assert_eq!(
        keys,
        vec![href(1), href(2), href(9), "/dav/cal/zz/a.ics".to_string()]
    );
    assert_eq!(env.requests("<D:sync-token>v3-o0").await, 1);
}

// ---------------------------------------------------------------------------------------------
// Crash resumption
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn crash_at_every_send_and_checkpoint_resumes_to_the_same_index() {
    let env = Arc::new(Env::new().await);
    {
        let mut m = env.model.lock().unwrap();
        m.page = 4;
        for i in 0..10 {
            m.put(&href(i), &ics(&format!("u{i}"), &format!("E{i}")));
        }
    }
    let build = {
        let env = env.clone();
        move |r: Arc<Replica>| {
            let c: Arc<dyn Connector> = Arc::new(env.connector(r));
            async move { c }
        }
    };
    // Initial full sync (paged).
    let report = assert_crash_resumable(&build, None, BTreeMap::new()).await;
    assert_eq!(report.sends, 10);

    // Incremental: edits, deletes and additions over several pages (checkpointed per page).
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    {
        let mut m = env.model.lock().unwrap();
        for i in [0, 3, 7] {
            m.put(&href(i), &ics(&format!("u{i}"), "edited"));
        }
        for i in [1, 8] {
            m.delete(&href(i));
        }
        for i in 20..24 {
            m.put(&href(i), &ics(&format!("u{i}"), "new"));
        }
    }
    let report = assert_crash_resumable(&build, Some(cursor), replica.snapshot()).await;
    assert_eq!(report.sends, 9);
    assert!(report.checkpoints >= 3, "{report:?}");
}

// ---------------------------------------------------------------------------------------------
// Review regressions
// ---------------------------------------------------------------------------------------------

async fn multigets_naming(env: &Env, h: &str) -> usize {
    env.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            let b = String::from_utf8_lossy(&r.body);
            b.contains("calendar-multiget") && b.contains(h)
        })
        .count()
}

fn small_limits() -> Limits {
    Limits {
        max_body: 100 * 1024,
        max_event: 64 * 1024,
    }
}

#[tokio::test]
async fn regression_oversized_object_is_not_downloaded_again_until_it_changes() {
    let env = Env::new().await;
    {
        let mut m = env.model.lock().unwrap();
        m.page = 3;
        for i in 0..6 {
            m.put(&href(i), &ics(&format!("u{i}"), "small"));
        }
        m.put(&href(4), &ics("big", &"z".repeat(200_000)));
    }
    let replica = Replica::new();
    let conn = env
        .connector(replica.clone())
        .with_limits(small_limits())
        .unwrap();
    let cursor = run_pass(&conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(replica.len(), 5);
    let first = multigets_naming(&env, &href(4)).await;
    assert!(first >= 1);
    assert!(
        cursor["calendars"][CAL]["skipped"][href(4)].is_string(),
        "{cursor}"
    );
    // The token expires: the full relisting does not download it again (same ETag).
    {
        let mut m = env.model.lock().unwrap();
        m.expired_below = m.version + 1;
    }
    let cursor = run_pass(&conn, Some(cursor), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(multigets_naming(&env, &href(4)).await, first);
    // Once it changes (and fits), it is fetched and the skip is forgotten.
    env.model
        .lock()
        .unwrap()
        .put(&href(4), &ics("big", "now small"));
    let cursor = run_pass(&conn, Some(cursor), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert!(replica.get(&href(4)).unwrap().content.contains("now small"));
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
    let conn = env
        .connector(replica.clone())
        .with_limits(small_limits())
        .unwrap();
    let before = env.egress.events().len();
    run_pass(&conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(replica.keys(), vec![href(0)]);
    assert_eq!(multigets_naming(&env, &href(1)).await, 0);
    let received: u64 = env.egress.events()[before..]
        .iter()
        .map(|e| e.bytes_received)
        .sum();
    assert!(received < 50_000, "{received}");
}

#[tokio::test]
async fn regression_unresolvable_href_keeps_the_known_item() {
    let env = Env::new().await;
    for i in 0..3 {
        env.model
            .lock()
            .unwrap()
            .put(&href(i), &ics(&format!("u{i}"), "x"));
    }
    let replica = Replica::new();
    let (cursor, _) = env.pass(&replica, None).await;
    // The token expires and the full listing names item 1 by an href we cannot use.
    {
        let mut m = env.model.lock().unwrap();
        m.expired_below = m.version + 1;
    }
    let etag = |i: usize| format!("<D:getetag>\"e{}\"</D:getetag>", i + 1);
    Mock::given(method("REPORT"))
        .and(body_string_contains("<D:sync-token></D:sync-token>"))
        .respond_with(ms(
            &[
                resp(&href(0), &etag(0)),
                resp(
                    &format!("https://elsewhere.example.com{}", href(1)),
                    &etag(1),
                ),
                resp(&href(2), &etag(2)),
            ],
            Some("v3-o0"),
        ))
        .with_priority(1)
        .mount(&env.server)
        .await;
    let (_, sink) = env.pass(&replica, Some(cursor)).await;
    assert_eq!(replica.keys(), vec![href(0), href(1), href(2)]);
    assert!(
        sink.summary().iter().all(|s| !s.starts_with('-')),
        "{:?}",
        sink.summary()
    );
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
