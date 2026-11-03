//! The desktop shell's cloud half, without the webview, for `apps/desktop/e2e` (scenario
//! `cloud`): one request per line on stdin, one answer per line on stdout. It runs the real
//! client (`Session`, `Api`) and the shell's own command code ([`nugget_cloud_client::shell`]),
//! with the session kept in memory instead of the keychain.
//!
//! Requests: `{"id": 1, "cmd": "cloud_me", "args": {...}}` for any `cloud_*` command (args as
//! the page sends them), `{"id": 2, "cmd": "deeplink", "args": {"url": "nugget://auth/callback?code=…"}}`
//! to finish a sign-in, `{"id": 3, "cmd": "handoff"}` for nuggetd's token (or null signed out).
//! Answers: `{"id", "ok": value, "effects": [...]}` or `{"id", "err": {kind, message, code, cloud}, "effects": [...]}`.
//! Configuration: `NUGGET_SUPABASE_URL`, `NUGGET_SUPABASE_ANON_KEY`, `NUGGET_CLOUD_URL`.

use std::sync::Arc;

use nugget_cloud_client::shell::{Effect, dispatch};
use nugget_cloud_client::{
    Api, CloudConfig, CloudError, MemoryStore, Session, SystemClock, parse_callback,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

fn effects_json(fx: &[Effect]) -> Value {
    Value::Array(
        fx.iter()
            .map(|e| match e {
                Effect::OpenWeb(url) => json!({"open_web": url}),
                Effect::Session { signed_in, error } => {
                    json!({"session": {"signed_in": signed_in, "error": error}})
                }
                Effect::Handoff => json!({"handoff": true}),
            })
            .collect(),
    )
}

fn error_json(e: &CloudError) -> Value {
    // The shell's CloudCmdError shape (src-tauri/src/cloud.rs).
    json!({"kind": "rpc", "message": e.to_string(), "code": null, "cloud": e.code()})
}

async fn handle(api: &Api, cmd: &str, args: &Value) -> Value {
    match cmd {
        "deeplink" => {
            let url = args["url"].as_str().unwrap_or_default();
            let r = match parse_callback(url) {
                Ok(cb) => api.session().complete(cb).await,
                Err(e) => Err(e),
            };
            match r {
                Ok(s) => json!({"ok": {"signed_in": s.signed_in, "email": s.email},
                                "effects": [{"session": {"signed_in": s.signed_in, "error": null}}, {"handoff": true}]}),
                Err(e) => json!({"err": error_json(&e),
                                 "effects": [{"session": {"signed_in": false, "error": e.to_string()}}]}),
            }
        }
        "handoff" => match api.session().handoff().await {
            Ok(h) => {
                json!({"ok": {"api_base": h.api_base, "access_token": h.access_token, "expires_at": h.expires_at}, "effects": []})
            }
            Err(CloudError::NotSignedIn) => json!({"ok": null, "effects": []}),
            Err(e) => json!({"err": error_json(&e), "effects": []}),
        },
        _ => {
            let (r, fx) = dispatch(api, cmd, args).await;
            match r {
                Ok(v) => json!({"ok": v, "effects": effects_json(&fx)}),
                Err(e) => json!({"err": error_json(&e), "effects": effects_json(&fx)}),
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cfg = CloudConfig::from_env().expect("cloud configuration in the environment");
    let http = nugget_cloud_client::http_client().expect("http client");
    let session = Session::new(
        cfg,
        http,
        Arc::new(MemoryStore::default()),
        Arc::new(SystemClock),
    );
    let api = Arc::new(Api::new(Arc::new(session)));
    let out = Arc::new(Mutex::new(tokio::io::stdout()));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (api, out) = (api.clone(), out.clone());
        // Concurrent, like the shell: the page makes several calls at once.
        tokio::spawn(async move {
            let cmd = req["cmd"].as_str().unwrap_or_default().to_owned();
            let mut answer = handle(&api, &cmd, &req["args"]).await;
            answer["id"] = req["id"].clone();
            let mut o = out.lock().await;
            let _ = o.write_all(format!("{answer}\n").as_bytes()).await;
            let _ = o.flush().await;
        });
    }
}
