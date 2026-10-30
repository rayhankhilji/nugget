//! ks-probe: kill switch vs. computer-use actions already handed to the OS.
//!
//! A. `type_text` (Edit, allowed without asking) whose backend call takes 1500 ms (as typing a long
//!    text does): engage the kill switch 300 ms into it. When does the run end, and when does the
//!    "OS" finish typing?
//! B. `click` on an element never observed (Send, asks): engage the kill switch, then approve the
//!    pending confirmation 0 ms later through `Agent::confirmations()` (what `agent.confirm` calls).
//!    Is the approval accepted, and does the click reach the backend?
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use nugget_a11y::{
    ActionOutcome, AppEntry, ComputerUse, ElementId, KeyInput, TreeLimits, UiAction, UiTree,
    WindowInfo, WindowSel,
};
use nugget_agent::{Agent, AgentConfig, AgentEvent, MemoryLog, Model, Task, ToolRegistry};
use nugget_core::capability::Locality;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use nugget_infer::{ChatEvent, ChatRequest, ChatStream, Provider, ProviderKind, ProviderMeta, StopReason};
use nugget_policy::{KillSwitch, PolicyEngine};
use serde_json::{Value, json};

struct SlowCu {
    t0: Instant,
    calls: Mutex<Vec<String>>,
    delay: Duration,
}

impl SlowCu {
    fn note(&self, s: String) {
        self.calls.lock().unwrap().push(format!("{:>6} ms {s}", self.t0.elapsed().as_millis()));
    }
}

impl ComputerUse for SlowCu {
    fn permission(&self) -> nugget_a11y::Permission {
        nugget_a11y::Permission::NotRequired
    }
    fn apps(&self) -> Result<Vec<AppEntry>> {
        Ok(Vec::new())
    }
    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
    fn snapshot(&self, _w: &WindowSel, _l: &TreeLimits) -> Result<UiTree> {
        Err(Error::NotFound("no window (fake)".into()))
    }
    fn act(&self, id: &ElementId, a: &UiAction) -> Result<ActionOutcome> {
        self.note(format!("OS act START {id:?} {a:?}"));
        std::thread::sleep(self.delay);
        self.note("OS act END (the click happened)".into());
        Ok(ActionOutcome { verified: true, detail: "clicked".into(), before: None, after: None })
    }
    fn key_input(&self, _t: Option<&ElementId>, i: &KeyInput) -> Result<ActionOutcome> {
        let n = match i {
            KeyInput::Text(s) => s.chars().count(),
            KeyInput::Keys(k) => k.len(),
        };
        self.note(format!("OS key_input START ({n} chars)"));
        std::thread::sleep(self.delay);
        self.note("OS key_input END (all text typed)".into());
        Ok(ActionOutcome { verified: true, detail: "typed".into(), before: None, after: None })
    }
    fn open_app(&self, _n: &str) -> Result<ActionOutcome> {
        Err(Error::Unsupported("fake".into()))
    }
}

struct Scripted {
    id: ProviderId,
    turns: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Provider for Scripted {
    fn id(&self) -> &ProviderId {
        &self.id
    }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta { kind: ProviderKind::OpenaiCompatible, model: "scripted".into(), locality: Locality::OnDevice, context_tokens: None }
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
        let mut t = self.turns.lock().unwrap();
        if t.is_empty() {
            t.push(("finish".into(), json!({"summary": "end", "outcome": "cannot"})));
        }
        let (name, args) = t.remove(0);
        let n = t.len();
        Ok(Box::pin(stream::iter(vec![
            Ok(ChatEvent::ToolCall { id: format!("c{n}"), name, arguments: args }),
            Ok(ChatEvent::Done { stop_reason: StopReason::ToolUse, input_tokens: Some(10), output_tokens: Some(5) }),
        ])))
    }
    async fn embed(&self, _t: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported("embed".into()))
    }
}

fn agent(turns: Vec<(&str, Value)>, delay: Duration) -> (Agent, Arc<SlowCu>, KillSwitch, Arc<MemoryLog>, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    let cu = Arc::new(SlowCu { t0: Instant::now(), calls: Mutex::new(Vec::new()), delay });
    let kill = KillSwitch::new();
    let engine = Arc::new(PolicyEngine::builder().kill_switch(kill.clone()).build());
    let mut reg = ToolRegistry::new();
    for t in nugget_computer::tools_with(cu.clone(), nugget_computer::ComputerConfig::default()) {
        reg.register(t);
    }
    let model = Model::Provider(Arc::new(Scripted {
        id: ProviderId::new("scripted"),
        turns: Mutex::new(turns.into_iter().map(|(n, a)| (n.to_string(), a)).collect()),
    }));
    let log = Arc::new(MemoryLog::new());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let a = Agent::new(model, reg, engine).with_log(log.clone()).with_events(Arc::new(tx));
    (a, cu, kill, log, rx)
}

fn records(log: &MemoryLog) -> Vec<String> {
    log.entries()
        .iter()
        .map(|e| {
            let v = serde_json::to_value(&e.record).unwrap();
            let mut s = v["kind"].as_str().unwrap_or("?").to_string();
            if let Some(t) = v["tool"].as_str() {
                s += &format!("({t})");
            }
            if let Some(d) = v.get("decision") {
                s += &format!(":{}", d.get("decision").or(Some(d)).unwrap());
            }
            if v["interrupted"] == json!(true) {
                s += &format!(":INTERRUPTED[{}]", v["result"].as_str().unwrap_or(""));
            }
            if let Some(o) = v.get("outcome") {
                s += &format!(":{o}");
            }
            s
        })
        .collect()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let reps: usize = std::env::var("KS_REPEAT").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    for rep in 1..=reps {
        // ---------------- A ----------------
        let text = "Synthetic QA text. ".repeat(100);
        let (a, cu, kill, log, _rx) = agent(vec![("type_text", json!({"text": text}))], Duration::from_millis(1500));
        let a = Arc::new(a);
        let a2 = a.clone();
        let run = tokio::spawn(async move { a2.run(Task::new(format!("A{rep}"), "type the synthetic text"), AgentConfig::default()).await });
        // wait for the key_input to start
        while !cu.calls.lock().unwrap().iter().any(|c| c.contains("key_input START")) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        cu.note("KILL SWITCH ENGAGED".into());
        kill.halt();
        let r = run.await.unwrap();
        cu.note(format!("run returned: {:?}", r.outcome));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        println!("A#{rep} timeline:");
        for c in cu.calls.lock().unwrap().iter() {
            println!("    {c}");
        }
        println!("A#{rep} log: {:?}", records(&log));

        // ---------------- B ----------------
        let mut accepted_after_kill = 0;
        let mut acts_after_kill = 0;
        let mut logs = Vec::new();
        for _ in 0..20 {
            let (a, cu, kill, log, mut rx) = agent(vec![("click", json!({"element": "e7"}))], Duration::from_millis(10));
            let a = Arc::new(a);
            let a2 = a.clone();
            let run = tokio::spawn(async move { a2.run(Task::new("B", "click send"), AgentConfig::default()).await });
            let req = loop {
                match rx.recv().await {
                    Some(AgentEvent::ConfirmationNeeded { request, .. }) => break request,
                    Some(_) => continue,
                    None => panic!("no confirmation"),
                }
            };
            kill.halt();
            cu.note("KILL".into());
            let ok = a.confirmations().answer(&req.request_id, true);
            let r = run.await.unwrap();
            let calls = cu.calls.lock().unwrap().clone();
            let kill_idx = calls.iter().position(|c| c.contains("KILL")).unwrap();
            if ok {
                accepted_after_kill += 1;
            }
            acts_after_kill += calls[kill_idx..].iter().filter(|c| c.contains("OS act START")).count();
            logs.push(format!("accepted={ok} outcome={:?} log={:?}", r.outcome, records(&log).into_iter().rev().take(3).rev().collect::<Vec<_>>()));
        }
        println!("B#{rep}: approvals accepted after kill: {accepted_after_kill}/20; OS clicks after kill: {acts_after_kill}");
        for l in logs.iter().take(3) {
            println!("    {l}");
        }
        if let Some(l) = logs.iter().find(|l| l.starts_with("accepted=true")) {
            println!("    e.g. accepted: {l}");
        }
    }
}
