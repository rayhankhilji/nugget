//! [`PolicyEngine`]: the [`PolicyGate`] every executor consults.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::classify::RiskClassifier;
use crate::glob::glob_match;
use crate::kill::{HookGuard, KillSwitch};
use crate::log::{ActionLog, LogEvent};
use crate::redact::RedactedAction;
use crate::rules::{Effect, MatchInput, PolicyConfig, Rule, target_sets};
use crate::store::PolicyStore;
use crate::text::cap;
use crate::{ActionContext, ActionDescriptor, Decision, PolicyGate, RiskClass};

/// A standing approval inside one task ("allow sending in this task").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskApproval {
    /// The risk class approved. Only Send and Destroy can be approved for a whole task; every
    /// Spend and Security action is confirmed on its own.
    pub risk: RiskClass,
    /// Glob over target candidates; `None` approves the class on any (non-denied) target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// The full result of evaluating one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    pub decision: Decision,
    /// Effective risk after the classifier.
    pub risk: RiskClass,
    pub declared: RiskClass,
    /// Why the classifier raised the risk.
    pub reasons: Vec<String>,
    /// The rule (or approval, or limit) that decided.
    pub rule: Option<String>,
}

/// Builder for [`PolicyEngine`].
pub struct PolicyEngineBuilder {
    config: PolicyConfig,
    store: Option<Arc<dyn PolicyStore>>,
    kill: Option<KillSwitch>,
    log: Option<Arc<ActionLog>>,
}

impl PolicyEngineBuilder {
    pub fn config(mut self, config: PolicyConfig) -> Self {
        self.config = config;
        self
    }
    /// Persist configuration changes to `store`. Use [`PolicyEngine::load`] to also read from it.
    pub fn store(mut self, store: Arc<dyn PolicyStore>) -> Self {
        self.store = Some(store);
        self
    }
    /// Use this kill switch instead of [`KillSwitch::global`].
    pub fn kill_switch(mut self, kill: KillSwitch) -> Self {
        self.kill = Some(kill);
        self
    }
    /// Record every decision, approval and halt here.
    pub fn log(mut self, log: Arc<ActionLog>) -> Self {
        self.log = Some(log);
        self
    }

    pub fn build(self) -> PolicyEngine {
        let kill = self.kill.unwrap_or_else(|| KillSwitch::global().clone());
        let halt_hook = self.log.as_ref().map(|log| {
            let log = Arc::downgrade(log);
            kill.on_change(move |halted| {
                if let Some(log) = log.upgrade() {
                    let _ = log.append(if halted {
                        LogEvent::Halted
                    } else {
                        LogEvent::Resumed
                    });
                }
            })
        });
        PolicyEngine {
            config: RwLock::new(self.config),
            store: self.store,
            classifier: RiskClassifier::global(),
            kill,
            _halt_hook: halt_hook,
            log: self.log,
            tasks: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Default)]
struct TaskState {
    approvals: Vec<TaskApproval>,
    counts: HashMap<RiskClass, u32>,
}

/// The policy engine. Cheap to evaluate (no I/O except appending to the log), safe to share.
pub struct PolicyEngine {
    config: RwLock<PolicyConfig>,
    store: Option<Arc<dyn PolicyStore>>,
    classifier: &'static RiskClassifier,
    kill: KillSwitch,
    _halt_hook: Option<HookGuard>,
    log: Option<Arc<ActionLog>>,
    tasks: Mutex<HashMap<String, TaskState>>,
}

impl std::fmt::Debug for PolicyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEngine")
            .field("halted", &self.kill.halted())
            .finish_non_exhaustive()
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new(PolicyConfig::default())
    }
}

impl PolicyEngine {
    /// An engine with `config`, the global kill switch, no store and no log.
    pub fn new(config: PolicyConfig) -> Self {
        Self::builder().config(config).build()
    }

    pub fn builder() -> PolicyEngineBuilder {
        PolicyEngineBuilder {
            config: PolicyConfig::default(),
            store: None,
            kill: None,
            log: None,
        }
    }

    /// Read the configuration from `store` (defaults if it holds none) and persist changes there.
    pub fn load(store: Arc<dyn PolicyStore>, builder: PolicyEngineBuilder) -> Result<Self> {
        let config = store.load()?.unwrap_or_default();
        Ok(builder.config(config).store(store).build())
    }

    pub fn config(&self) -> PolicyConfig {
        self.read_config().clone()
    }

    /// Change the configuration, persist it, and log the change.
    pub fn update_config(&self, summary: &str, f: impl FnOnce(&mut PolicyConfig)) -> Result<()> {
        let snapshot = {
            let mut cfg = self.config.write().unwrap_or_else(|p| p.into_inner());
            let mut next = cfg.clone();
            f(&mut next);
            if let Some(dup) = duplicate_id(&next.rules) {
                return Err(Error::Invalid(format!("duplicate rule id {dup}")));
            }
            if next.rules.iter().any(|r| r.id.starts_with("default:")) {
                return Err(Error::Invalid(
                    "rule ids starting with default: are reserved".into(),
                ));
            }
            *cfg = next.clone();
            next
        };
        if let Some(store) = &self.store {
            store.save(&snapshot)?;
        }
        self.record(LogEvent::ConfigChanged {
            summary: cap(summary, 200),
        });
        Ok(())
    }

    /// Add or replace a user rule (by id).
    pub fn upsert_rule(&self, rule: Rule) -> Result<()> {
        let summary = format!("rule {} set to {:?}", rule.id, rule.effect);
        self.update_config(&summary, |cfg| {
            cfg.rules.retain(|r| r.id != rule.id);
            cfg.rules.push(rule);
        })
    }

    /// Remove a user rule. Returns whether it existed.
    pub fn remove_rule(&self, id: &str) -> Result<bool> {
        let existed = self.read_config().rules.iter().any(|r| r.id == id);
        if existed {
            self.update_config(&format!("rule {id} removed"), |cfg| {
                cfg.rules.retain(|r| r.id != id)
            })?;
        }
        Ok(existed)
    }

    pub fn kill_switch(&self) -> &KillSwitch {
        &self.kill
    }

    /// Engage the kill switch: every evaluation is denied and pending confirmations are denied.
    pub fn halt(&self) {
        self.kill.halt();
    }

    pub fn resume(&self) {
        self.kill.resume();
    }

    /// Whether the kill switch is engaged (one atomic load).
    #[inline]
    pub fn halted(&self) -> bool {
        self.kill.halted()
    }

    pub fn log(&self) -> Option<&Arc<ActionLog>> {
        self.log.as_ref()
    }

    /// Approve a risk class for the rest of a task. Spend and Security cannot be approved this
    /// way. Deny rules still apply.
    pub fn approve_for_task(&self, task_id: &str, approval: TaskApproval) -> Result<()> {
        if !matches!(approval.risk, RiskClass::Send | RiskClass::Destroy) {
            return Err(Error::Invalid(format!(
                "{} actions are confirmed one at a time",
                approval.risk
            )));
        }
        self.lock_tasks()
            .entry(task_id.to_string())
            .or_default()
            .approvals
            .push(approval.clone());
        self.record(LogEvent::TaskApproved {
            task_id: task_id.to_string(),
            risk: approval.risk,
            target: approval.target,
        });
        Ok(())
    }

    /// Forget a task's approvals and rate-limit counters.
    pub fn end_task(&self, task_id: &str) {
        if self.lock_tasks().remove(task_id).is_some() {
            self.record(LogEvent::TaskEnded {
                task_id: task_id.to_string(),
            });
        }
    }

    /// Every standing per-task approval, as `(task_id, approval)`, sorted by task id (approvals
    /// of one task in the order they were granted).
    pub fn task_approvals(&self) -> Vec<(String, TaskApproval)> {
        let tasks = self.lock_tasks();
        let mut ids: Vec<&String> = tasks.keys().collect();
        ids.sort();
        ids.into_iter()
            .flat_map(|id| {
                tasks[id]
                    .approvals
                    .iter()
                    .map(move |a| (id.clone(), a.clone()))
            })
            .collect()
    }

    /// How many actions of `risk` this task has used against its rate limit.
    pub fn used(&self, task_id: &str, risk: RiskClass) -> u32 {
        self.lock_tasks()
            .get(task_id)
            .and_then(|t| t.counts.get(&risk).copied())
            .unwrap_or(0)
    }

    /// Classify an action without deciding (no side effects).
    pub fn classify(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
    ) -> crate::Classification {
        self.classifier.classify_with_context(action, ctx)
    }

    /// Decide on an action, with context. An `Allow` consumes one unit of the task's rate limit
    /// for the risk class; a `Confirm` consumes one when the user approves it through the
    /// [`crate::ConfirmationBroker`] (or [`PolicyEngine::consume`]). The decision is logged.
    pub fn assess(&self, action: &ActionDescriptor, ctx: &ActionContext) -> Assessment {
        let assessment = self.decide(action, ctx, true);
        self.log_assessment(action, ctx, &assessment);
        assessment
    }

    /// Re-evaluate an action an outer gate already decided on and let through (an executor that
    /// runs under the agent loop checks again with what it sees itself). Same precedence as
    /// [`PolicyEngine::assess`] except that the rate limit is neither checked nor consumed: the
    /// outer evaluation did both, and counting twice would deny the last allowed action. The
    /// decision is logged.
    pub fn recheck(&self, action: &ActionDescriptor, ctx: &ActionContext) -> Assessment {
        let assessment = self.decide(action, ctx, false);
        self.log_assessment(action, ctx, &assessment);
        assessment
    }

    fn log_assessment(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        assessment: &Assessment,
    ) {
        self.record(LogEvent::Decision {
            action: RedactedAction::new(action, ctx),
            risk: assessment.risk,
            decision: assessment.decision.clone(),
            rule: assessment.rule.clone(),
            reasons: assessment.reasons.clone(),
        });
    }

    /// Count an approved action against the task's rate limit. Returns `false` (and counts
    /// nothing) if the limit is already used up.
    pub fn consume(&self, task_id: &str, risk: RiskClass) -> bool {
        let limit = self.read_config().rate_limits.get(&risk).copied();
        let mut tasks = self.lock_tasks();
        let used = tasks
            .entry(task_id.to_string())
            .or_default()
            .counts
            .entry(risk)
            .or_insert(0);
        if limit.is_some_and(|l| *used >= l) {
            return false;
        }
        *used += 1;
        true
    }

    fn decide(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        rate_limited: bool,
    ) -> Assessment {
        let classification = self.classifier.classify_with_context(action, ctx);
        let risk = classification.risk;
        let mut out = Assessment {
            decision: Decision::Allow,
            risk,
            declared: action.declared_risk,
            reasons: classification.reasons,
            rule: None,
        };
        if self.kill.halted() {
            out.decision = Decision::Deny("Nugget is halted (kill switch engaged)".into());
            out.rule = Some("kill-switch".into());
            return out;
        }

        let (primary, mentioned) = target_sets(action, ctx);
        let targets: Vec<String> = primary.iter().chain(mentioned.iter()).cloned().collect();
        let labels: Vec<String> = [action.target_label.as_deref(), ctx.window_title.as_deref()]
            .into_iter()
            .flatten()
            .map(str::to_lowercase)
            .collect();
        let verb = action.verb.to_lowercase();
        let input = MatchInput {
            targets: &targets,
            primary: &primary,
            verb: &verb,
            risk,
            labels: &labels,
        };
        let cfg = self.read_config();
        let defaults = cfg.active_defaults();

        // Deny wins, from any layer.
        if let Some(rule) = cfg
            .rules
            .iter()
            .chain(defaults.iter())
            .find(|r| r.effect == Effect::Deny && r.matches(&input).is_some())
        {
            let what = rule.description.as_deref().unwrap_or("a deny rule");
            let target = targets
                .iter()
                .find(|t| rule.targets.iter().any(|g| glob_match(g, t)))
                .map(String::as_str)
                .unwrap_or(&action.target_app);
            out.decision = Decision::Deny(format!(
                "Blocked: {what} ({target}) are off limits to Nugget [rule {}]",
                rule.id
            ));
            out.rule = Some(rule.id.clone());
            return out;
        }

        let limit = if rate_limited {
            cfg.rate_limits.get(&risk).copied()
        } else {
            None
        };
        drop(cfg);

        // Per-task approvals: only on what the action is performed on (never on a host a label
        // or the typed text merely mentions).
        let approved = self.lock_tasks().get(&action.task_id).is_some_and(|t| {
            t.approvals.iter().any(|a| {
                a.risk == risk
                    && a.target
                        .as_deref()
                        .is_none_or(|g| primary.iter().any(|t| glob_match(g, t)))
            })
        });
        let (effect, rule_id) = if approved {
            (Effect::Allow, format!("task-approval:{risk}"))
        } else {
            let cfg = self.read_config();
            best(&cfg.rules, &input)
                .or_else(|| best(&defaults, &input))
                .unwrap_or_else(|| {
                    if risk >= RiskClass::Send {
                        (Effect::Confirm, "fallback".into())
                    } else {
                        (Effect::Allow, "fallback".into())
                    }
                })
        };
        out.rule = Some(rule_id);

        // Rate limit.
        if let Some(limit) = limit {
            let mut tasks = self.lock_tasks();
            let used = tasks
                .entry(action.task_id.clone())
                .or_default()
                .counts
                .entry(risk)
                .or_insert(0);
            if *used >= limit {
                out.decision = Decision::Deny(format!(
                    "Rate limit: this task already used its {limit} {risk} actions"
                ));
                out.rule = Some(format!("rate-limit:{risk}"));
                return out;
            }
            if effect == Effect::Allow {
                *used += 1;
            }
        }

        out.decision = match effect {
            Effect::Allow => Decision::Allow,
            Effect::Confirm => Decision::Confirm(explain(action, ctx, &out)),
            Effect::Deny => unreachable!("deny rules were handled above"),
        };
        out
    }

    fn record(&self, event: LogEvent) {
        if let Some(log) = &self.log {
            // Logging must never block an evaluation; a failed write surfaces in verify/open.
            let _ = log.append(event);
        }
    }

    pub(crate) fn record_event(&self, event: LogEvent) {
        self.record(event);
    }

    fn read_config(&self) -> std::sync::RwLockReadGuard<'_, PolicyConfig> {
        self.config.read().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_tasks(&self) -> std::sync::MutexGuard<'_, HashMap<String, TaskState>> {
        self.tasks.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl PolicyGate for PolicyEngine {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision {
        self.assess(action, &ActionContext::default()).decision
    }

    fn halted(&self) -> bool {
        self.kill.halted()
    }

    fn evaluate_with_context(
        &self,
        action: &ActionDescriptor,
        context: &ActionContext,
    ) -> Decision {
        self.assess(action, context).decision
    }

    /// Counts the approval against the task's limit for the effective risk (the larger of
    /// `risk` and what the classifier makes of `action`), like [`PolicyEngine::consume`].
    fn record_approved(&self, action: &ActionDescriptor, risk: RiskClass) -> bool {
        let classified = self
            .classifier
            .classify_with_context(action, &ActionContext::default())
            .risk;
        self.consume(&action.task_id, risk.max(classified))
    }
}

/// A [`PolicyGate`] over [`PolicyEngine::recheck`]: for an executor whose actions were already
/// evaluated (and their rate limit counted) by an outer gate, such as the agent loop.
#[derive(Debug, Clone)]
pub struct RecheckGate(pub Arc<PolicyEngine>);

impl PolicyGate for RecheckGate {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision {
        self.0.recheck(action, &ActionContext::default()).decision
    }

    fn halted(&self) -> bool {
        self.0.halted()
    }

    fn evaluate_with_context(
        &self,
        action: &ActionDescriptor,
        context: &ActionContext,
    ) -> Decision {
        self.0.recheck(action, context).decision
    }

    /// The outer gate counted the approval already; counting again would halve the limit.
    fn record_approved(&self, _action: &ActionDescriptor, _risk: RiskClass) -> bool {
        true
    }
}

/// The most specific matching Allow/Confirm rule; ties go to Confirm, then to the earlier rule.
fn best(rules: &[Rule], input: &MatchInput<'_>) -> Option<(Effect, String)> {
    let mut best: Option<(usize, Effect, &str)> = None;
    for rule in rules.iter().filter(|r| r.effect != Effect::Deny) {
        if let Some(score) = rule.matches(input) {
            let better = match best {
                None => true,
                Some((s, e, _)) => score > s || (score == s && rule.effect > e),
            };
            if better {
                best = Some((score, rule.effect, &rule.id));
            }
        }
    }
    best.map(|(_, e, id)| (e, id.to_string()))
}

fn duplicate_id(rules: &[Rule]) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    rules
        .iter()
        .find(|r| !seen.insert(r.id.as_str()))
        .map(|r| r.id.clone())
}

/// A sentence the user can act on: what will happen, where, with what, and why we ask.
fn explain(action: &ActionDescriptor, ctx: &ActionContext, a: &Assessment) -> String {
    let r = RedactedAction::new(action, ctx);
    let what = match a.risk {
        RiskClass::Observe => "look at something",
        RiskClass::Edit => "make an edit",
        RiskClass::Send => "send something on your behalf",
        RiskClass::Destroy => "delete or overwrite data",
        RiskClass::Spend => "spend money or commit you to something",
        RiskClass::Security => "change security or system settings",
    };
    let mut s = format!("Nugget is about to {what} in {}: {}", r.target_app, r.verb);
    if let Some(label) = &r.target_label {
        s.push_str(&format!(" “{label}”"));
    }
    if let Some(preview) = &r.text_preview {
        s.push_str(&format!(" with “{preview}”"));
    }
    s.push('.');
    if a.risk > a.declared {
        let why = a
            .reasons
            .first()
            .map(String::as_str)
            .unwrap_or("heuristics");
        s.push_str(&format!(" Treated as {} ({why}).", a.risk));
    }
    s.push_str(&format!(" Task {}.", r.task_id));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rule;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn engine() -> PolicyEngine {
        PolicyEngine::builder()
            .kill_switch(KillSwitch::new())
            .build()
    }

    fn act(app: &str, verb: &str, label: Option<&str>, risk: RiskClass) -> ActionDescriptor {
        ActionDescriptor {
            target_app: app.into(),
            verb: verb.into(),
            target_label: label.map(Into::into),
            text_preview: None,
            declared_risk: risk,
            task_id: "task-1".into(),
        }
    }

    fn is_confirm(d: &Decision) -> bool {
        matches!(d, Decision::Confirm(_))
    }
    fn is_deny(d: &Decision) -> bool {
        matches!(d, Decision::Deny(_))
    }

    #[test]
    fn defaults() {
        let e = engine();
        assert_eq!(
            e.evaluate(&act("com.apple.mail", "scroll", None, RiskClass::Observe)),
            Decision::Allow
        );
        assert_eq!(
            e.evaluate(&act(
                "com.apple.mail",
                "type",
                Some("Body"),
                RiskClass::Edit
            )),
            Decision::Allow
        );
        for risk in [
            RiskClass::Send,
            RiskClass::Destroy,
            RiskClass::Spend,
            RiskClass::Security,
        ] {
            assert!(
                is_confirm(&e.evaluate(&act("com.apple.mail", "click", None, risk))),
                "{risk}"
            );
        }
        assert!(is_deny(&e.evaluate(&act(
            "com.1password.1password",
            "scroll",
            None,
            RiskClass::Observe
        ))));
        assert!(is_deny(&e.evaluate(&act(
            "secure.chase.com",
            "click",
            Some("Accounts"),
            RiskClass::Observe
        ))));
        assert!(is_deny(&e.evaluate(&act(
            "com.robinhood.release",
            "scroll",
            None,
            RiskClass::Observe
        ))));
        assert!(is_deny(&e.evaluate(&act(
            "ms-settings:windowsdefender",
            "open",
            None,
            RiskClass::Observe
        ))));
        assert!(is_deny(&e.evaluate(&act(
            "com.apple.systempreferences",
            "click",
            Some("Privacy & Security"),
            RiskClass::Observe
        ))));
        // Non-security settings panes are confirmed, not denied.
        assert!(is_confirm(&e.evaluate(&act(
            "com.apple.systempreferences",
            "click",
            Some("Displays"),
            RiskClass::Observe
        ))));
        // Unrelated hosts are not caught by brand globs.
        assert_eq!(
            e.evaluate(&act(
                "gemini.google.com",
                "scroll",
                None,
                RiskClass::Observe
            )),
            Decision::Allow
        );
        assert_eq!(
            e.evaluate(&act("chime.aws", "scroll", None, RiskClass::Observe)),
            Decision::Allow
        );
    }

    #[test]
    fn confirm_explains() {
        let e = engine();
        let mut a = act("com.apple.mail", "click", Some("Send"), RiskClass::Edit);
        a.text_preview = Some("Hi Bob, the invoice is attached".into());
        let Decision::Confirm(text) = e.evaluate(&a) else {
            panic!()
        };
        assert!(text.contains("send something"), "{text}");
        assert!(text.contains("com.apple.mail"), "{text}");
        assert!(text.contains("“Send”"), "{text}");
        assert!(text.contains("Hi Bob"), "{text}");
        assert!(text.contains("Treated as send"), "{text}");
    }

    #[test]
    fn precedence_deny_beats_allow_and_specific_beats_general() {
        let e = engine();
        // A user allow on a bank does not beat the default deny.
        e.upsert_rule(
            Rule::new("allow-bank", Effect::Allow)
                .targets(["chase.com"])
                .risks(RiskClass::ALL),
        )
        .unwrap();
        assert!(is_deny(&e.evaluate(&act(
            "chase.com",
            "scroll",
            None,
            RiskClass::Observe
        ))));

        // A user rule beats a default: allow sending in Slack.
        e.upsert_rule(
            Rule::new("slack-send", Effect::Allow)
                .targets(["*slack*"])
                .risks([RiskClass::Send]),
        )
        .unwrap();
        assert_eq!(
            e.evaluate(&act(
                "com.tinyspeck.slackmacgap",
                "click",
                Some("Send"),
                RiskClass::Send
            )),
            Decision::Allow
        );
        // ...but not spending there.
        assert!(is_confirm(&e.evaluate(&act(
            "com.tinyspeck.slackmacgap",
            "click",
            Some("Upgrade"),
            RiskClass::Spend
        ))));

        // A more specific Confirm beats a general Allow.
        e.upsert_rule(
            Rule::new("slack-general-send", Effect::Confirm)
                .targets(["com.tinyspeck.slackmacgap"])
                .labels(["*general*"])
                .risks([RiskClass::Send]),
        )
        .unwrap();
        assert!(is_confirm(&e.evaluate(&act(
            "com.tinyspeck.slackmacgap",
            "click",
            Some("Send to #general"),
            RiskClass::Send
        ))));

        // Same specificity: Confirm wins over Allow.
        e.upsert_rule(
            Rule::new("a", Effect::Allow)
                .targets(["x.example"])
                .risks([RiskClass::Edit]),
        )
        .unwrap();
        e.upsert_rule(
            Rule::new("b", Effect::Confirm)
                .targets(["x.example"])
                .risks([RiskClass::Edit]),
        )
        .unwrap();
        assert!(is_confirm(&e.evaluate(&act(
            "x.example",
            "type",
            None,
            RiskClass::Edit
        ))));

        // A user deny on a verb.
        e.upsert_rule(Rule::new("no-upload", Effect::Deny).verbs(["upload*"]))
            .unwrap();
        assert!(is_deny(&e.evaluate(&act(
            "drive.google.com",
            "upload_file",
            None,
            RiskClass::Send
        ))));
    }

    #[test]
    fn allow_rule_without_risks_does_not_allow_risky() {
        let e = engine();
        e.upsert_rule(Rule::new("mail", Effect::Allow).targets(["com.apple.mail"]))
            .unwrap();
        assert!(is_confirm(&e.evaluate(&act(
            "com.apple.mail",
            "click",
            Some("Send"),
            RiskClass::Observe
        ))));
    }

    #[test]
    fn disabled_defaults_and_reserved_ids() {
        let e = engine();
        e.update_config("off", |c| {
            c.disabled_defaults.push("default:deny-financial".into())
        })
        .unwrap();
        assert_eq!(
            e.evaluate(&act("chase.com", "scroll", None, RiskClass::Observe)),
            Decision::Allow
        );
        assert!(
            e.upsert_rule(Rule::new("default:mine", Effect::Allow))
                .is_err()
        );
        assert!(
            e.update_config("dup", |c| {
                c.rules.push(Rule::new("x", Effect::Allow));
                c.rules.push(Rule::new("x", Effect::Deny));
            })
            .is_err()
        );
        assert!(e.remove_rule("nope").is_ok_and(|existed| !existed));
    }

    #[test]
    fn per_task_approvals() {
        let e = engine();
        let send = act("com.apple.mail", "click", Some("Send"), RiskClass::Send);
        assert!(is_confirm(&e.evaluate(&send)));
        e.approve_for_task(
            "task-1",
            TaskApproval {
                risk: RiskClass::Send,
                target: Some("com.apple.mail".into()),
            },
        )
        .unwrap();
        assert_eq!(e.evaluate(&send), Decision::Allow);
        // Other tasks, other targets and other classes are unaffected.
        let mut other_task = send.clone();
        other_task.task_id = "task-2".into();
        assert!(is_confirm(&e.evaluate(&other_task)));
        assert!(is_confirm(&e.evaluate(&act(
            "com.tinyspeck.slackmacgap",
            "click",
            Some("Send"),
            RiskClass::Send
        ))));
        assert!(is_confirm(&e.evaluate(&act(
            "com.apple.mail",
            "click",
            Some("Delete"),
            RiskClass::Destroy
        ))));
        // Spend cannot be approved per task; deny rules still win.
        assert!(
            e.approve_for_task(
                "task-1",
                TaskApproval {
                    risk: RiskClass::Spend,
                    target: None
                }
            )
            .is_err()
        );
        e.approve_for_task(
            "task-1",
            TaskApproval {
                risk: RiskClass::Send,
                target: None,
            },
        )
        .unwrap();
        assert!(is_deny(&e.evaluate(&act(
            "paypal.com",
            "click",
            Some("Send"),
            RiskClass::Send
        ))));
        e.end_task("task-1");
        assert!(is_confirm(&e.evaluate(&send)));
    }

    #[test]
    fn rate_limits() {
        let e = engine();
        e.update_config("limit", |c| {
            c.rate_limits.insert(RiskClass::Send, 2);
            c.rate_limits.insert(RiskClass::Edit, 3);
        })
        .unwrap();
        let typing = act("com.apple.notes", "type", Some("Body"), RiskClass::Edit);
        for _ in 0..3 {
            assert_eq!(e.evaluate(&typing), Decision::Allow);
        }
        assert!(is_deny(&e.evaluate(&typing)));

        // Confirms count when approved (consume), not when asked.
        let send = act("com.apple.mail", "click", Some("Send"), RiskClass::Send);
        assert!(is_confirm(&e.evaluate(&send)));
        assert!(is_confirm(&e.evaluate(&send)));
        assert!(e.consume("task-1", RiskClass::Send));
        assert!(e.consume("task-1", RiskClass::Send));
        assert!(!e.consume("task-1", RiskClass::Send));
        assert!(is_deny(&e.evaluate(&send)));
        assert_eq!(e.used("task-1", RiskClass::Send), 2);

        // Approved for the task: still limited.
        let mut t2 = send.clone();
        t2.task_id = "task-2".into();
        e.approve_for_task(
            "task-2",
            TaskApproval {
                risk: RiskClass::Send,
                target: None,
            },
        )
        .unwrap();
        assert_eq!(e.evaluate(&t2), Decision::Allow);
        assert_eq!(e.evaluate(&t2), Decision::Allow);
        assert!(is_deny(&e.evaluate(&t2)));
        e.end_task("task-2");
        assert!(is_confirm(&e.evaluate(&t2)));
    }

    #[test]
    fn kill_switch_denies_everything() {
        let e = engine();
        let a = act("com.apple.notes", "scroll", None, RiskClass::Observe);
        assert_eq!(e.evaluate(&a), Decision::Allow);
        e.halt();
        assert!(e.halted());
        assert!(is_deny(&e.evaluate(&a)));
        e.resume();
        assert_eq!(e.evaluate(&a), Decision::Allow);
    }

    #[test]
    fn kill_switch_under_concurrency() {
        let kill = KillSwitch::new();
        let e = Arc::new(PolicyEngine::builder().kill_switch(kill.clone()).build());
        let threads = 8;
        let barrier = Arc::new(Barrier::new(threads + 1));
        let halted_at = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let violations = Arc::new(AtomicUsize::new(0));
        let allowed = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for i in 0..threads {
            let (e, barrier, halted_at, violations, allowed) = (
                Arc::clone(&e),
                Arc::clone(&barrier),
                Arc::clone(&halted_at),
                Arc::clone(&violations),
                Arc::clone(&allowed),
            );
            handles.push(std::thread::spawn(move || {
                let mut a = act("com.apple.notes", "scroll", None, RiskClass::Observe);
                a.task_id = format!("t{i}");
                barrier.wait();
                for _ in 0..2000 {
                    // Read the flag first: if the halt had completed, nothing may be allowed.
                    let was_halted = halted_at.load(Ordering::SeqCst);
                    let d = e.evaluate(&a);
                    if d == Decision::Allow {
                        allowed.fetch_add(1, Ordering::SeqCst);
                        if was_halted {
                            violations.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            }));
        }
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(2));
        kill.halt();
        halted_at.store(true, Ordering::SeqCst);
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(violations.load(Ordering::SeqCst), 0);
        assert!(e.halted());
        // After the halt every evaluation from every thread is a deny.
        let a = act("com.apple.notes", "scroll", None, RiskClass::Observe);
        assert!(is_deny(&e.evaluate(&a)));
    }

    #[test]
    fn decisions_and_halts_are_logged() {
        let log = Arc::new(ActionLog::in_memory());
        let kill = KillSwitch::new();
        let e = PolicyEngine::builder()
            .kill_switch(kill.clone())
            .log(Arc::clone(&log))
            .build();
        e.evaluate(&act(
            "com.apple.mail",
            "click",
            Some("Send"),
            RiskClass::Edit,
        ));
        kill.trigger_hook()();
        e.evaluate(&act("com.apple.mail", "scroll", None, RiskClass::Observe));
        e.resume();
        let kinds: Vec<&'static str> = log
            .entries()
            .iter()
            .map(|en| match en.event {
                LogEvent::Decision { .. } => "decision",
                LogEvent::Halted => "halted",
                LogEvent::Resumed => "resumed",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["decision", "halted", "decision", "resumed"]);
        log.verify_chain().unwrap();
    }

    fn act2(
        app: &str,
        verb: &str,
        label: Option<&str>,
        preview: Option<&str>,
        risk: RiskClass,
    ) -> ActionDescriptor {
        ActionDescriptor {
            text_preview: preview.map(Into::into),
            task_id: "t1".into(),
            ..act(app, verb, label, risk)
        }
    }

    // Security review PA-1 (regression): approvals and Allow rules match only primary targets.
    #[test]
    fn pa1_task_approval_does_not_leak_to_other_site_via_label_url() {
        let e = engine();
        e.approve_for_task(
            "t1",
            TaskApproval {
                risk: RiskClass::Send,
                target: Some("mail.google.com".into()),
            },
        )
        .unwrap();
        let plain = act2(
            "evil.example",
            "click",
            Some("button \"Post comment\""),
            None,
            RiskClass::Send,
        );
        assert!(is_confirm(&e.evaluate(&plain)));
        let spoof = act2(
            "evil.example",
            "click",
            Some("button \"Post https://mail.google.com/x\""),
            None,
            RiskClass::Send,
        );
        assert!(is_confirm(&e.evaluate(&spoof)), "{:?}", e.evaluate(&spoof));
        // The approved site itself still is approved.
        let real = act2(
            "mail.google.com",
            "click",
            Some("Send"),
            None,
            RiskClass::Send,
        );
        assert_eq!(e.evaluate(&real), Decision::Allow);
    }

    #[test]
    fn pa1_user_allow_rule_does_not_leak_via_preview_url() {
        let e = engine();
        e.upsert_rule(
            Rule::new("u:work", Effect::Allow)
                .targets(["*.corp.example"])
                .risks([RiskClass::Send]),
        )
        .unwrap();
        let a = act2(
            "com.tinyspeck.slackmacgap",
            "send_message",
            Some("Message #general"),
            Some("see https://wiki.corp.example/page"),
            RiskClass::Send,
        );
        assert!(is_confirm(&e.evaluate(&a)), "{:?}", e.evaluate(&a));
        // Deny rules still see mentioned hosts.
        e.upsert_rule(Rule::new("u:no-corp", Effect::Deny).targets(["*.corp.example"]))
            .unwrap();
        assert!(is_deny(&e.evaluate(&a)));
        // Navigating to a deep link or domain makes it a primary target.
        e.remove_rule("u:no-corp").unwrap();
        let nav = act2(
            "com.google.chrome",
            "navigate",
            Some("https://wiki.corp.example/page"),
            None,
            RiskClass::Send,
        );
        assert_eq!(e.evaluate(&nav), Decision::Allow);
    }

    // Security review PA-6: approvals count against the rate limit through the gate.
    #[test]
    fn pa6_record_approved_counts_against_the_rate_limit() {
        let e = Arc::new(engine());
        e.update_config("limit", |c| {
            c.rate_limits.insert(RiskClass::Spend, 2);
        })
        .unwrap();
        let buy = act("shop.example", "click", Some("Buy now"), RiskClass::Spend);
        let gate: &dyn PolicyGate = e.as_ref();
        assert!(gate.record_approved(&buy, RiskClass::Spend));
        assert!(gate.record_approved(&buy, RiskClass::Spend));
        assert!(!gate.record_approved(&buy, RiskClass::Spend));
        assert_eq!(e.used("task-1", RiskClass::Spend), 2);
        assert!(is_deny(&e.evaluate(&buy)));
        // The classifier's risk counts even when the caller under-declares.
        let mut other = buy.clone();
        other.task_id = "task-2".into();
        assert!(gate.record_approved(&other, RiskClass::Edit));
        assert_eq!(e.used("task-2", RiskClass::Spend), 1);
        // A recheck gate never counts twice.
        let re = RecheckGate(Arc::clone(&e));
        assert!(re.record_approved(&buy, RiskClass::Spend));
        assert_eq!(e.used("task-1", RiskClass::Spend), 2);
    }

    #[test]
    fn task_approvals_are_listed() {
        let e = engine();
        assert!(e.task_approvals().is_empty());
        let send = TaskApproval {
            risk: RiskClass::Send,
            target: Some("com.apple.mail".into()),
        };
        let destroy = TaskApproval {
            risk: RiskClass::Destroy,
            target: None,
        };
        e.approve_for_task("t2", destroy.clone()).unwrap();
        e.approve_for_task("t1", send.clone()).unwrap();
        e.approve_for_task("t1", destroy.clone()).unwrap();
        // Rate-limit counters alone make no approval.
        e.consume("t3", RiskClass::Send);
        assert_eq!(
            e.task_approvals(),
            vec![
                ("t1".to_string(), send),
                ("t1".to_string(), destroy.clone()),
                ("t2".to_string(), destroy),
            ]
        );
        e.end_task("t1");
        assert_eq!(e.task_approvals().len(), 1);
    }

    #[test]
    fn recheck_skips_the_rate_limit_but_not_the_rules() {
        let e = Arc::new(engine());
        e.update_config("limit", |c| {
            c.rate_limits.insert(RiskClass::Edit, 1);
        })
        .unwrap();
        let typing = act("com.apple.notes", "type", Some("Body"), RiskClass::Edit);
        // The outer gate uses the only unit; the executor's recheck still allows it.
        assert_eq!(e.evaluate(&typing), Decision::Allow);
        let gate = RecheckGate(Arc::clone(&e));
        assert_eq!(gate.evaluate(&typing), Decision::Allow);
        assert_eq!(e.used("task-1", RiskClass::Edit), 1);
        assert!(is_deny(&e.evaluate(&typing)));
        // Deny rules, confirmations and the kill switch still apply.
        assert!(is_deny(&gate.evaluate(&act(
            "chase.com",
            "scroll",
            None,
            RiskClass::Observe
        ))));
        assert!(is_confirm(&gate.evaluate(&act(
            "com.apple.mail",
            "click",
            Some("Send"),
            RiskClass::Send
        ))));
        e.halt();
        assert!(gate.halted());
        assert!(is_deny(&gate.evaluate(&typing)));
    }

    #[test]
    fn evaluate_with_context_uses_page_url() {
        let e = engine();
        let a = act(
            "com.google.chrome",
            "click",
            Some("Statements"),
            RiskClass::Observe,
        );
        assert_eq!(e.evaluate(&a), Decision::Allow);
        let ctx = ActionContext::default().with_url("https://www.wellsfargo.com/accounts");
        assert!(is_deny(&e.evaluate_with_context(&a, &ctx)));
        let gate: &dyn PolicyGate = &e;
        assert!(is_deny(&gate.evaluate_with_context(&a, &ctx)));
    }
}
