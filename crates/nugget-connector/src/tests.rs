use std::sync::{Arc, Mutex};

use nugget_core::egress::{EgressEvent, EgressSink};
use reqwest::Method;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::http::MeteredHttp;

#[derive(Default)]
struct VecSink(Mutex<Vec<EgressEvent>>);
impl EgressSink for VecSink {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

#[tokio::test]
async fn metered_http_records_egress_and_does_not_follow_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "http://127.0.0.2:1/x"))
        .mount(&server)
        .await;
    let sink = Arc::new(VecSink::default());
    let http = MeteredHttp::new(sink.clone(), "sync:test").unwrap();
    let resp = http
        .send(http.request(Method::GET, &server.uri()), &[])
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
    assert!(resp.error_for_status().is_err());
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].feature, "sync:test");
    assert!(events[0].bytes_sent > 0);
}

#[tokio::test]
async fn status_codes_map_to_typed_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3"))
        .mount(&server)
        .await;
    let http = MeteredHttp::new(Arc::new(VecSink::default()), "sync:test").unwrap();
    let err = http
        .send(http.request(Method::GET, &server.uri()), &[])
        .await
        .unwrap()
        .error_for_status()
        .unwrap_err();
    assert!(matches!(
        err,
        nugget_core::Error::RateLimited {
            retry_after_ms: Some(3000)
        }
    ));
}

#[tokio::test]
async fn oversized_bodies_are_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 2048]))
        .mount(&server)
        .await;
    let http = MeteredHttp::new(Arc::new(VecSink::default()), "sync:test")
        .unwrap()
        .with_max_body(1024);
    assert!(
        http.send(http.request(Method::GET, &server.uri()), &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_exchange_still_records_egress() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(30)))
        .mount(&server)
        .await;
    let sink = Arc::new(VecSink::default());
    let http = MeteredHttp::new(sink.clone(), "sync:test").unwrap();
    let req = http
        .request(Method::POST, &server.uri())
        .body(vec![b'a'; 1000]);
    // Drop the request future while the server is still "thinking".
    let res =
        tokio::time::timeout(std::time::Duration::from_millis(300), http.send(req, &[])).await;
    assert!(res.is_err(), "the request should have been cancelled");
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1, "a cancelled exchange still writes one row");
    assert_eq!(events[0].feature, "sync:test");
    assert!(events[0].bytes_sent >= 1000);
    assert_eq!(events[0].bytes_received, 0);
}

#[tokio::test]
async fn completed_exchange_records_exactly_one_row_with_received_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 4096]))
        .mount(&server)
        .await;
    let sink = Arc::new(VecSink::default());
    let http = MeteredHttp::new(sink.clone(), "sync:test").unwrap();
    http.send(http.request(Method::GET, &server.uri()), &[])
        .await
        .unwrap();
    let events = sink.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].bytes_received, 4096);
}
