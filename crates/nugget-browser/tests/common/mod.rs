#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;
use nugget_browser::bridge::{BridgeError, BrowserBridge};
use nugget_browser::controller::Confirmer;
use nugget_browser::protocol::HostRequest;
use nugget_browser::snapshot::PageSnapshot;
use nugget_policy::{ActionDescriptor, Decision, PolicyGate};
use serde_json::Value;

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

pub fn form() -> PageSnapshot {
    serde_json::from_str(&fixture("form-snapshot.json")).unwrap()
}

pub fn checkout() -> PageSnapshot {
    serde_json::from_str(&fixture("checkout-snapshot.json")).unwrap()
}

pub fn id(s: &PageSnapshot, name: &str) -> String {
    s.elements
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
        .id
        .clone()
}

/// A policy gate that records every descriptor it sees.
pub struct FakeGate {
    pub seen: Mutex<Vec<ActionDescriptor>>,
    pub denied_hosts: Vec<String>,
    pub confirm_verbs: Vec<String>,
    pub halted: std::sync::atomic::AtomicBool,
}

impl FakeGate {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            denied_hosts: vec!["bank.example".into()],
            confirm_verbs: Vec::new(),
            halted: false.into(),
        }
    }

    pub fn verbs(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.verb.clone())
            .collect()
    }
}

impl PolicyGate for FakeGate {
    fn evaluate(&self, a: &ActionDescriptor) -> Decision {
        self.seen.lock().unwrap().push(a.clone());
        if self.denied_hosts.contains(&a.target_app) {
            Decision::Deny(format!("{} is on the deny list", a.target_app))
        } else if self.confirm_verbs.contains(&a.verb) {
            Decision::Confirm(format!("{} needs approval", a.verb))
        } else {
            Decision::Allow
        }
    }

    fn halted(&self) -> bool {
        self.halted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A bridge answering from a script, recording every request.
pub struct FakeBridge {
    pub requests: Mutex<Vec<HostRequest>>,
    pub snapshot: Mutex<PageSnapshot>,
    pub act_reply: Mutex<Option<Result<Value, BridgeError>>>,
}

impl FakeBridge {
    pub fn new(snapshot: PageSnapshot) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            snapshot: Mutex::new(snapshot),
            act_reply: Mutex::new(None),
        }
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| match r {
                HostRequest::Hello { .. } => "hello",
                HostRequest::Snapshot { .. } => "snapshot",
                HostRequest::Act { .. } => "act",
                HostRequest::Overlay { active: true, .. } => "overlay_on",
                HostRequest::Overlay { .. } => "overlay_off",
                HostRequest::Halt => "halt",
            })
            .collect()
    }
}

#[async_trait]
impl BrowserBridge for FakeBridge {
    async fn request(&self, request: HostRequest) -> Result<Value, BridgeError> {
        self.requests.lock().unwrap().push(request.clone());
        match request {
            HostRequest::Snapshot { .. } => {
                Ok(serde_json::to_value(&*self.snapshot.lock().unwrap()).unwrap())
            }
            HostRequest::Act { .. } => {
                self.act_reply.lock().unwrap().clone().unwrap_or_else(|| {
                    Ok(serde_json::from_str(&fixture("form-act-type.json")).unwrap())
                })
            }
            _ => Ok(Value::Object(Default::default())),
        }
    }
}

pub struct ScriptedConfirmer(pub bool, pub Mutex<Vec<String>>);

#[async_trait]
impl Confirmer for ScriptedConfirmer {
    async fn confirm(&self, a: &ActionDescriptor, reason: &str) -> bool {
        self.1.lock().unwrap().push(format!("{}: {reason}", a.verb));
        self.0
    }
}
