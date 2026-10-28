//! `draft` with a real provider, mocked: an OpenAI-compatible streaming endpoint on loopback.

use nugget_bench::suites::draft::DraftOptions;
use nugget_bench::{RunConfig, Scale, Status, find, run_benchmark};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse(chunks: &[&str]) -> String {
    let mut body = String::new();
    for c in chunks {
        let ev = serde_json::json!({
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": { "content": c }, "finish_reason": null }],
        });
        body.push_str(&format!("data: {ev}\n\n"));
    }
    body.push_str(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    );
    body.push_str("data: [DONE]\n\n");
    body
}

#[test]
fn real_provider_path_streams_and_gets_the_local_ttft_target() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    rt.block_on(
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(&[
                        "Hi Priya,\n\n",
                        "The price is $48,000.",
                        "\n\nBest,\nMaya",
                    ])),
            )
            .mount(&server),
    );
    let draft = find("draft").unwrap();
    let report = run_benchmark(
        draft.as_ref(),
        &RunConfig {
            scale: Scale::Smoke,
            seed: 3,
            draft: DraftOptions {
                provider_url: Some(format!("{}/v1", server.uri())),
                provider_model: Some("mock-model".into()),
                ..DraftOptions::default()
            },
            ..RunConfig::default()
        },
    );
    assert_eq!(report.status, Status::Ok, "{:?}", report.message);
    let p = &report.config.params;
    assert_eq!(p["tasks"], 6, "smoke limits real-provider runs");
    assert!(p["model"].as_str().unwrap().contains("openai_compatible"));
    assert_eq!(p["model.locality"], "OnDevice");
    // Both arms went to the provider: 6 tasks x 2 arms.
    let requests = rt.block_on(server.received_requests()).unwrap();
    assert_eq!(requests.len(), 12);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "mock-model");
    // A loopback provider is local: the 3 s target applies, the cloud leak gates do not.
    let ttft = report.metric("draft.nugget.ttft.p50_s").unwrap();
    assert_eq!(ttft.target, Some(3.0));
    assert_eq!(ttft.target_met, Some(true));
    assert!(ttft.value > 0.0);
    assert!(
        report
            .metric("draft.nugget.sensitive_leak_rate")
            .unwrap()
            .target
            .is_none()
    );
    // The canned reply carries the first task's gold fact.
    assert!(report.metric("draft.nugget.fact_recall").unwrap().value > 0.0);
}
