//! `[policy]`: the safety policy for actions (`nugget-policy`), shared by the agent and the
//! browser features. Serves `policy.*` and broadcasts `policy.kill.changed`.
//!
//! On disk, under the data dir:
//! - `policy/policy.json`: the user's `PolicyConfig` (rules, disabled defaults, rate limits,
//!   confirmation timeout), written by `policy.set`.
//! - `policy/actions.jsonl`: the hash-chained action log every decision, confirmation, halt and
//!   agent step goes to. It is verified when the daemon starts. A log that fails verification is
//!   moved aside to `policy/actions.broken-<unix ms>.jsonl` (kept for inspection, never
//!   deleted) and a fresh chain is started; while such a file exists, `policy.get` reports
//!   `chain_ok = false` with an error naming it.
//!
//! The kill switch is this daemon's own (tests run several daemons in one process). Every change
//! of it, whoever made it (`policy.kill`, the browser's Stop button, ...), is broadcast.
//!
//! ```toml
//! [policy]
//! enabled = true   # false: policy.* are not served (the policy still gates every action)
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nugget_core::ipc::{
    ActionLogStatus, DefaultRuleView, KillState, PolicyApprovals, PolicyLogExport, PolicySet,
    PolicyState, TaskApprovalView, agent_method as m, agent_notification,
};
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError, parse_params, to_result};
use nugget_policy::kill::HookGuard;
use nugget_policy::{
    ActionLog, JsonFilePolicyStore, KillSwitch, PolicyConfig, PolicyEngine, default_rules,
};
use serde::Deserialize;
use serde_json::Value;

use super::{Feature, FeatureContext, section};

/// What the policy, browser and agent features share. One per daemon (tests run several
/// daemons in one process, so nothing here is global).
pub(crate) struct Safety {
    pub engine: Arc<PolicyEngine>,
    /// The hash-chained action log under the data dir, shared by the engine and the agent.
    pub log: Arc<ActionLog>,
    /// This daemon's kill switch (the engine's).
    pub kill: KillSwitch,
    /// Open confirmation requests (`nugget_agent::ConfirmationRequest` as JSON) by request id,
    /// written by the agent feature, read by `policy.approvals`.
    pub pending: Mutex<BTreeMap<String, Value>>,
}

const POLICY_DIR: &str = "policy";
const CONFIG_FILE: &str = "policy.json";
const LOG_FILE: &str = "actions.jsonl";
const BROKEN_PREFIX: &str = "actions.broken-";

impl Safety {
    /// Load the policy config and open the action log under the data dir.
    pub(crate) fn open(cx: &FeatureContext<'_>) -> Result<Arc<Self>> {
        Self::open_in(&cx.state.paths.data.join(POLICY_DIR))
    }

    /// [`Safety::open`] with the policy directory given.
    pub(crate) fn open_in(dir: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        let log = Arc::new(open_log(&dir.join(LOG_FILE))?);
        let kill = KillSwitch::new();
        let store = Arc::new(JsonFilePolicyStore::new(dir.join(CONFIG_FILE)));
        let engine = PolicyEngine::load(
            store,
            PolicyEngine::builder()
                .kill_switch(kill.clone())
                .log(log.clone()),
        )?;
        Ok(Arc::new(Self {
            engine: Arc::new(engine),
            log,
            kill,
            pending: Mutex::new(BTreeMap::new()),
        }))
    }
}

/// Open (and verify) the log; move a log that fails verification aside and start a new one.
fn open_log(path: &Path) -> Result<ActionLog> {
    match ActionLog::open(path) {
        Ok(log) => Ok(log),
        Err(e) => {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let aside = path.with_file_name(format!("{BROKEN_PREFIX}{ms}.jsonl"));
            tracing::error!(
                error = %e,
                path = %path.display(),
                moved_to = %aside.display(),
                "the action log failed verification; it was moved aside and a new chain started"
            );
            std::fs::rename(path, &aside)?;
            ActionLog::open(path)
        }
    }
}

/// Logs that failed verification at some start and were moved aside.
fn broken_logs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(BROKEN_PREFIX))
        })
        .collect();
    out.sort();
    out
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct PolicySection {
    enabled: bool,
}

impl Default for PolicySection {
    fn default() -> Self {
        Self { enabled: true }
    }
}

const METHODS: &[&str] = &[
    m::POLICY_GET,
    m::POLICY_SET,
    m::POLICY_APPROVALS,
    m::POLICY_KILL,
    m::POLICY_RESUME,
    m::POLICY_LOG_EXPORT,
];

/// The `policy` feature.
pub(crate) fn start(cx: &FeatureContext<'_>, safety: &Arc<Safety>) -> Result<Arc<dyn Feature>> {
    let cfg: PolicySection = section("policy", &cx.config.policy)?;
    let hub = cx.state.hub.clone();
    let feature = PolicyFeature::new(
        safety.clone(),
        cx.state.paths.data.join(POLICY_DIR),
        cfg.enabled,
        Box::new(move |state| {
            hub.broadcast(
                agent_notification::POLICY_KILL_CHANGED,
                serde_json::to_value(state).unwrap_or(Value::Null),
            )
        }),
    );
    Ok(Arc::new(feature))
}

type Broadcast = Box<dyn Fn(KillState) + Send + Sync>;

struct PolicyFeature {
    safety: Arc<Safety>,
    dir: PathBuf,
    enabled: bool,
    /// Broadcasts every kill-switch change while the feature lives.
    _kill_hook: HookGuard,
}

impl PolicyFeature {
    fn new(safety: Arc<Safety>, dir: PathBuf, enabled: bool, broadcast: Broadcast) -> Self {
        let kill_hook = safety
            .kill
            .on_change(move |halted| broadcast(KillState { halted }));
        Self {
            safety,
            dir,
            enabled,
            _kill_hook: kill_hook,
        }
    }

    fn log_status(&self) -> ActionLogStatus {
        let log = &self.safety.log;
        let mut status = ActionLogStatus {
            entries: log.len() as u64,
            head: log.head(),
            chain_ok: true,
            error: None,
        };
        if let Err(e) = log.verify_chain() {
            status.chain_ok = false;
            status.error = Some(e.to_string());
        } else if let Some(broken) = broken_logs(&self.dir).last() {
            status.chain_ok = false;
            status.error = Some(format!(
                "an earlier action log failed verification and was moved to {}",
                broken.display()
            ));
        }
        status
    }

    fn state(&self) -> PolicyState {
        let config = self.safety.engine.config();
        let defaults = default_rules()
            .into_iter()
            .map(|rule| DefaultRuleView {
                enabled: !config.disabled_defaults.contains(&rule.id),
                rule: serde_json::to_value(rule).unwrap_or(Value::Null),
            })
            .collect();
        PolicyState {
            config: serde_json::to_value(&config).unwrap_or(Value::Null),
            defaults,
            halted: self.safety.kill.halted(),
            log: self.log_status(),
        }
    }

    fn set(&self, params: Value) -> std::result::Result<PolicyState, RpcError> {
        let req: PolicySet = parse_params(params)?;
        let next: PolicyConfig = serde_json::from_value(req.config)
            .map_err(|e| RpcError::invalid_params(format!("config: {e}")))?;
        let known: Vec<String> = default_rules().into_iter().map(|r| r.id).collect();
        if let Some(bad) = next.disabled_defaults.iter().find(|id| !known.contains(id)) {
            return Err(Error::Invalid(format!("no default rule {bad}")).into());
        }
        if next.confirm_timeout_secs == 0 {
            return Err(Error::Invalid("confirm_timeout_secs must be at least 1".into()).into());
        }
        let summary = format!(
            "policy replaced: {} rules, {} defaults off",
            next.rules.len(),
            next.disabled_defaults.len()
        );
        self.safety.engine.update_config(&summary, |cfg| {
            cfg.rules = next.rules;
            cfg.disabled_defaults = next.disabled_defaults;
            cfg.rate_limits = next.rate_limits;
            cfg.confirm_timeout_secs = next.confirm_timeout_secs;
        })?;
        Ok(self.state())
    }

    fn approvals(&self) -> PolicyApprovals {
        let pending = self
            .safety
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let task = self
            .safety
            .engine
            .task_approvals()
            .into_iter()
            .map(|(task_id, a)| TaskApprovalView {
                task_id,
                risk: a.risk.as_str().to_owned(),
                target: a.target,
            })
            .collect();
        PolicyApprovals { pending, task }
    }

    fn export(&self) -> std::result::Result<PolicyLogExport, RpcError> {
        let mut buf = Vec::new();
        self.safety.log.export_jsonl(&mut buf)?;
        Ok(PolicyLogExport {
            jsonl: String::from_utf8_lossy(&buf).into_owned(),
            status: self.log_status(),
        })
    }

    fn handle(&self, method: &str, params: Value) -> std::result::Result<Value, RpcError> {
        match method {
            m::POLICY_GET => to_result(self.state()),
            m::POLICY_SET => to_result(self.set(params)?),
            m::POLICY_APPROVALS => to_result(self.approvals()),
            m::POLICY_KILL => {
                self.safety.kill.halt();
                to_result(KillState {
                    halted: self.safety.kill.halted(),
                })
            }
            m::POLICY_RESUME => {
                self.safety.kill.resume();
                to_result(KillState {
                    halted: self.safety.kill.halted(),
                })
            }
            m::POLICY_LOG_EXPORT => to_result(self.export()?),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

#[async_trait]
impl Feature for PolicyFeature {
    fn name(&self) -> &'static str {
        "policy"
    }

    fn methods(&self) -> &'static [&'static str] {
        if self.enabled { METHODS } else { &[] }
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        self.handle(method, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_policy::{
        ActionDescriptor, Decision, Effect, PolicyGate, RiskClass, Rule, TaskApproval,
    };
    use serde_json::json;

    fn feature(dir: &Path) -> (PolicyFeature, Arc<Mutex<Vec<bool>>>) {
        let safety = Safety::open_in(dir).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let f = PolicyFeature::new(
            safety,
            dir.to_path_buf(),
            true,
            Box::new(move |k| s2.lock().unwrap().push(k.halted)),
        );
        (f, seen)
    }

    fn send(task: &str) -> ActionDescriptor {
        ActionDescriptor {
            target_app: "com.tinyspeck.slackmacgap".into(),
            verb: "click".into(),
            target_label: Some("Send".into()),
            text_preview: None,
            declared_risk: RiskClass::Send,
            task_id: task.into(),
        }
    }

    #[test]
    fn get_set_persist_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        {
            let (f, _) = feature(dir.path());
            let st: PolicyState =
                serde_json::from_value(f.handle(m::POLICY_GET, json!({})).unwrap()).unwrap();
            assert!(!st.halted);
            assert!(st.log.chain_ok);
            assert!(st.defaults.iter().all(|d| d.enabled));
            assert!(!st.defaults.is_empty());

            let mut cfg = PolicyConfig::default();
            cfg.rules.push(
                Rule::new("slack-send", Effect::Allow)
                    .targets(["*slack*"])
                    .risks([RiskClass::Send]),
            );
            cfg.disabled_defaults.push("default:deny-financial".into());
            let st: PolicyState =
                serde_json::from_value(f.handle(m::POLICY_SET, json!({ "config": cfg })).unwrap())
                    .unwrap();
            assert_eq!(st.config["rules"][0]["id"], "slack-send");
            assert!(
                st.defaults
                    .iter()
                    .any(|d| d.rule["id"] == "default:deny-financial" && !d.enabled)
            );
            assert_eq!(f.safety.engine.evaluate(&send("t")), Decision::Allow);

            // Reserved ids, unknown defaults, bad shapes and a zero timeout are refused.
            let mut bad = PolicyConfig::default();
            bad.rules.push(Rule::new("default:mine", Effect::Allow));
            assert!(f.handle(m::POLICY_SET, json!({ "config": bad })).is_err());
            let mut bad = PolicyConfig::default();
            bad.disabled_defaults.push("default:nope".into());
            assert!(f.handle(m::POLICY_SET, json!({ "config": bad })).is_err());
            let bad = PolicyConfig {
                confirm_timeout_secs: 0,
                ..PolicyConfig::default()
            };
            assert!(f.handle(m::POLICY_SET, json!({ "config": bad })).is_err());
            assert!(
                f.handle(m::POLICY_SET, json!({ "config": {"rules": 3} }))
                    .is_err()
            );
            assert_eq!(f.safety.engine.config().rules.len(), 1);
        }
        // A new daemon reads the saved policy back, and the log chain continues.
        let (f, _) = feature(dir.path());
        assert_eq!(f.safety.engine.config().rules.len(), 1);
        let ex: PolicyLogExport =
            serde_json::from_value(f.handle(m::POLICY_LOG_EXPORT, json!({})).unwrap()).unwrap();
        assert!(ex.status.chain_ok);
        assert!(ex.status.entries >= 2);
        assert_eq!(ex.jsonl.lines().count() as u64, ex.status.entries);
        assert!(ex.jsonl.contains("config_changed"));
    }

    #[test]
    fn kill_and_resume_broadcast_from_anywhere() {
        let dir = tempfile::tempdir().unwrap();
        let (f, seen) = feature(dir.path());
        let k: KillState =
            serde_json::from_value(f.handle(m::POLICY_KILL, json!({})).unwrap()).unwrap();
        assert!(k.halted);
        assert!(f.safety.engine.halted());
        assert!(matches!(
            f.safety.engine.evaluate(&send("t")),
            Decision::Deny(_)
        ));
        let k: KillState =
            serde_json::from_value(f.handle(m::POLICY_RESUME, json!({})).unwrap()).unwrap();
        assert!(!k.halted);
        // A halt from elsewhere (the browser's Stop) is broadcast too.
        f.safety.kill.trigger_hook()();
        assert_eq!(*seen.lock().unwrap(), vec![true, false, true]);
        // The switch is this daemon's, not the process-wide one.
        assert!(!KillSwitch::global().halted());
        drop(f);
        // The hook goes with the feature.
    }

    #[test]
    fn approvals_list_pending_and_task_approvals() {
        let dir = tempfile::tempdir().unwrap();
        let (f, _) = feature(dir.path());
        f.safety
            .pending
            .lock()
            .unwrap()
            .insert("r1".into(), json!({"request_id": "r1"}));
        f.safety
            .engine
            .approve_for_task(
                "t1",
                TaskApproval {
                    risk: RiskClass::Send,
                    target: Some("mail.google.com".into()),
                },
            )
            .unwrap();
        let a: PolicyApprovals =
            serde_json::from_value(f.handle(m::POLICY_APPROVALS, json!({})).unwrap()).unwrap();
        assert_eq!(a.pending, vec![json!({"request_id": "r1"})]);
        assert_eq!(
            a.task,
            vec![TaskApprovalView {
                task_id: "t1".into(),
                risk: "send".into(),
                target: Some("mail.google.com".into()),
            }]
        );
    }

    #[test]
    fn a_tampered_log_is_moved_aside_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        {
            let (f, _) = feature(dir.path());
            f.handle(m::POLICY_KILL, json!({})).unwrap();
            f.handle(m::POLICY_RESUME, json!({})).unwrap();
        }
        let path = dir.path().join(LOG_FILE);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replacen("halted", "resumed", 1)).unwrap();

        let (f, _) = feature(dir.path());
        let st: PolicyState =
            serde_json::from_value(f.handle(m::POLICY_GET, json!({})).unwrap()).unwrap();
        assert!(!st.log.chain_ok);
        assert!(
            st.log.error.as_deref().unwrap().contains(BROKEN_PREFIX),
            "{:?}",
            st.log.error
        );
        assert_eq!(broken_logs(dir.path()).len(), 1);
        // The daemon keeps running on a fresh chain.
        assert!(f.safety.log.verify_chain().is_ok());
    }

    #[test]
    fn disabled_serves_nothing_and_unknown_methods_fail() {
        let dir = tempfile::tempdir().unwrap();
        let safety = Safety::open_in(dir.path()).unwrap();
        let f = PolicyFeature::new(safety, dir.path().into(), false, Box::new(|_| {}));
        assert!(f.methods().is_empty());
        assert!(f.handle("policy.nope", json!({})).is_err());
        let t: toml::Table = toml::from_str("enabled = false").unwrap();
        assert!(!section::<PolicySection>("policy", &t).unwrap().enabled);
        assert!(
            section::<PolicySection>("policy", &toml::Table::new())
                .unwrap()
                .enabled
        );
        let t: toml::Table = toml::from_str("nope = 1").unwrap();
        assert!(section::<PolicySection>("policy", &t).is_err());
    }
}
