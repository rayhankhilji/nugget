//! A deliberately misbehaving plugin. The host's security tests call its tools and its connector
//! to check that every capability is denied by default and every resource limit holds.

use nugget_plugin_sdk::{
    Connector, Document, Error, Result, Tool, Tools, Value, export_plugin, http, json, kv, secrets,
    sink,
};

struct Hostile;

impl Tools for Hostile {
    fn tools() -> Vec<Tool> {
        [
            "spin", "alloc", "fetch", "secret", "kv_set", "kv_get", "env", "fs", "clock", "sink",
            "echo", "panic",
        ]
        .into_iter()
        .map(|n| Tool::new(n, format!("hostile: {n}"), json!({ "type": "object" })))
        .collect()
    }

    fn call(name: &str, input: Value) -> Result<Value> {
        match name {
            // An infinite loop: stopped by fuel or the epoch deadline.
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
            // Allocate and touch `mib` MiB: stopped by the memory cap.
            "alloc" => {
                let mib = input["mib"].as_u64().unwrap_or(1024) as usize;
                let mut chunks: Vec<Vec<u8>> = Vec::new();
                for _ in 0..mib {
                    chunks.push(vec![1u8; 1024 * 1024]);
                }
                Ok(json!({ "allocated_mib": chunks.len() }))
            }
            "fetch" => {
                let url = input["url"].as_str().unwrap_or_default();
                let method = input["method"].as_str().unwrap_or("GET");
                let mut req = http::Request::new(method, url);
                if let Some(h) = input["header"].as_array() {
                    req = req.header(
                        h[0].as_str().unwrap_or_default(),
                        h[1].as_str().unwrap_or_default(),
                    );
                }
                let resp = req.send()?;
                Ok(json!({
                    "status": resp.status,
                    "len": resp.body.len(),
                    "location": resp.header("location"),
                }))
            }
            "secret" => Ok(json!(secrets::get(
                input["name"].as_str().unwrap_or_default()
            )?)),
            "kv_set" => {
                kv::set("k", b"v")?;
                Ok(Value::Null)
            }
            "kv_get" => Ok(json!(
                kv::get("k")?.map(|v| String::from_utf8_lossy(&v).into_owned())
            )),
            // WASI is not linked: each of these traps.
            "env" => Ok(json!(std::env::var("HOME").ok())),
            "fs" => Ok(json!(std::fs::read_to_string("/etc/passwd").ok())),
            "clock" => Ok(json!(format!("{:?}", std::time::SystemTime::now()))),
            "sink" => {
                sink::upsert(Document::text("x", "text/plain", "x"))?;
                Ok(Value::Null)
            }
            "echo" => Ok(input),
            "panic" => panic!("deliberate"),
            other => Err(Error::NotFound(other.to_string())),
        }
    }
}

impl Connector for Hostile {
    fn validate(settings: &Value) -> Result<()> {
        if settings["spin"].as_bool() == Some(true) {
            loop {
                std::hint::black_box(());
            }
        }
        if settings.get("ok").is_none() {
            return Err(Error::invalid("missing `ok`"));
        }
        Ok(())
    }

    /// Settings pick a misbehaviour: `{"mode": "huge_doc" | "bad_metadata" | "kv" | "secret" |
    /// "wait_cancel" | "count"}`.
    fn sync(settings: &Value, _cursor: Option<Value>) -> Result<Value> {
        match settings["mode"].as_str().unwrap_or("count") {
            "huge_doc" => {
                let mib = settings["mib"].as_u64().unwrap_or(4) as usize;
                sink::upsert(Document::bytes(
                    "big",
                    "application/octet-stream",
                    vec![0; mib << 20],
                ))?;
            }
            "bad_metadata" => {
                sink::upsert(Document::text("m", "text/plain", "x").with_metadata(json!([1, 2])))?;
            }
            "kv" => {
                let n = settings["bytes"].as_u64().unwrap_or(10) as usize;
                kv::set("blob", &vec![b'a'; n])?;
            }
            "secret" => {
                let v = secrets::get(settings["name"].as_str().unwrap_or("token"))?;
                sink::upsert(Document::text(
                    "secret",
                    "text/plain",
                    v.unwrap_or_default(),
                ))?;
            }
            "wait_cancel" => {
                // Keep emitting until the host cancels.
                let mut i = 0u64;
                loop {
                    if sink::cancelled() {
                        return Err(Error::Cancelled);
                    }
                    sink::upsert(Document::text(format!("w{i}"), "text/plain", "x"))?;
                    i += 1;
                }
            }
            _ => {
                let n = settings["count"].as_u64().unwrap_or(3);
                for i in 0..n {
                    sink::upsert(Document::text(
                        format!("doc-{i}"),
                        "text/plain",
                        format!("body {i}"),
                    ))?;
                }
            }
        }
        Ok(json!({ "done": true }))
    }
}

export_plugin!(connector: Hostile, tools: Hostile);
