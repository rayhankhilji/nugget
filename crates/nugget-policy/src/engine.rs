//! [`PolicyEngine`]: the [`PolicyGate`] every executor consults.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::classify::RiskClassifier;
use crate::glob::glob_match;
use crate::kill::{HookGuard, KillSwitch};
use crate::log::{ActionLog, LogEvent};
use crate::redact::RedactedAction;
use crate::rules::{Effect, MatchInput, PolicyConfig, Rule, target_sets};
use crate::store::PolicyStore;
use crate::text::{Keyword, cap, fold_confusables, normalize, trim_label};
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

    /// Classify an action without deciding (no side effects): the [`RiskClassifier`], plus the
    /// engine's remote-access rule (see [`remote_access`]).
    pub fn classify(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
    ) -> crate::Classification {
        let mut c = self.classifier.classify_with_context(action, ctx);
        if let Some(why) = remote_access(action, ctx) {
            if c.risk < RiskClass::Security {
                c.risk = RiskClass::Security;
                c.reasons.insert(0, why);
            } else {
                c.reasons.push(why);
            }
        }
        c
    }

    /// [`PolicyGate::record_approved_with_context`] on the engine: counts the approval against
    /// the task's limit for the effective risk (the larger of `risk` and what
    /// [`PolicyEngine::classify`] makes of `action` in `ctx`), like [`PolicyEngine::consume`].
    pub fn record_approved_in(
        &self,
        action: &ActionDescriptor,
        risk: RiskClass,
        ctx: &ActionContext,
    ) -> bool {
        let classified = self.classify(action, ctx).risk;
        self.consume(&action.task_id, risk.max(classified))
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
        let remote = remote_access(action, ctx).is_some();
        let classification = self.classify(action, ctx);
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
        // Enabling or changing remote access is confirmed every time: no user or default Allow
        // rule ("trusted app"), and no per-task approval, lets it through unasked.
        let (effect, rule_id) = if remote && effect == Effect::Allow {
            (Effect::Confirm, "remote-access".to_string())
        } else {
            (effect, rule_id)
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

    /// Like [`PolicyGate::record_approved_with_context`] with an empty context. Callers that
    /// know the context should use that one.
    fn record_approved(&self, action: &ActionDescriptor, risk: RiskClass) -> bool {
        self.record_approved_in(action, risk, &ActionContext::default())
    }

    /// Counts the approval against the task's limit for the effective risk (the larger of
    /// `risk` and what the classifier makes of `action` in `context`), like
    /// [`PolicyEngine::consume`].
    fn record_approved_with_context(
        &self,
        action: &ActionDescriptor,
        risk: RiskClass,
        context: &ActionContext,
    ) -> bool {
        self.record_approved_in(action, risk, context)
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

    fn record_approved_with_context(
        &self,
        _action: &ActionDescriptor,
        _risk: RiskClass,
        _context: &ActionContext,
    ) -> bool {
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

// ---------------------------------------------------------------------------------------------
// Remote access. Turning on Remote Login (SSH), Screen Sharing, Remote Management, Remote
// Desktop, VNC or a remote-support agent hands the machine to someone else. The settings-app rule
// only sees System Settings; this one applies wherever the control is (a settings app, a router's
// web page, a third-party app, a typed command), raises the action to Security and forces a
// confirmation that no Allow rule or task approval skips.
// ---------------------------------------------------------------------------------------------

/// Phrases that name remote access, in a label or around a toggle.
const REMOTE_ACCESS_PHRASES: &[&str] = &[
    // English
    "remote login",
    "remote desktop",
    "remote access",
    "remote management",
    "remote administration",
    "remote assistance",
    "remote support",
    "remote control",
    "remote apple events",
    "remote connection*",
    "remote shell",
    "remote session*",
    "allow remote",
    "screen sharing",
    "desktop sharing",
    "share this computer",
    "unattended access",
    "vnc server",
    "vnc access",
    "enable vnc",
    "enable rdp",
    "allow rdp",
    "ssh server",
    "ssh access",
    "ssh login",
    "enable ssh",
    "openssh server",
    "secure shell server",
    "secure shell access",
    "teamviewer",
    "anydesk",
    "rustdesk",
    "chrome remote desktop",
    "remotedesktop",
    // German / French / Spanish / Portuguese / Italian / Dutch
    "entfernte anmeldung",
    "entfernte verwaltung",
    "bildschirmfreigabe",
    "fernzugriff",
    "fernwartung",
    "remoteanmeldung",
    "session à distance",
    "accès à distance",
    "bureau à distance",
    "partage d’écran",
    "partage d'écran",
    "partage de l’écran",
    "acceso remoto",
    "escritorio remoto",
    "inicio de sesión remoto",
    "compartir pantalla",
    "acesso remoto",
    "área de trabalho remota",
    "compartilhamento de tela",
    "accesso remoto",
    "desktop remoto",
    "condivisione schermo",
    "extern bureaublad",
    "externe toegang",
    "schermdeling",
    // Russian / CJK
    "удаленный доступ",
    "удалённый доступ",
    "удаленный рабочий стол",
    "удалённый рабочий стол",
    "远程桌面",
    "远程登录",
    "远程访问",
    "远程管理",
    "屏幕共享",
    "遠端桌面",
    "遠端登入",
    "螢幕共享",
    "リモートデスクトップ",
    "リモートログイン",
    "リモートマネージメント",
    "画面共有",
    "원격 데스크톱",
    "원격 로그인",
    "원격 관리",
    "화면 공유",
];

/// Deep-link and URL fragments of remote-access settings pages, matched on the URL with every
/// separator removed.
const REMOTE_ACCESS_URLS: &[&str] = &[
    "remotelogin",
    "remotedesktop",
    "remotemanagement",
    "remoteaccess",
    "remoteassistance",
    "screensharing",
    "sharingservices",
    "preferencessharing",
    "gnomecontrolcentersharing",
];

/// Commands that turn remote access on, matched on typed or run text.
const REMOTE_ACCESS_COMMANDS: &[&str] = &[
    "setremotelogin",
    "kickstart -activate",
    "ardagent",
    "screensharing.plist",
    "ssh.plist",
    "systemctl enable ssh",
    "systemctl enable --now ssh",
    "systemctl start ssh",
    "systemctl enable sshd",
    "systemctl enable --now sshd",
    "systemctl start sshd",
    "service ssh start",
    "grdctl",
    "gnome-remote-desktop",
    "x11vnc",
    "vncserver",
    "fdenytsconnections",
    "enable-psremoting",
    "winrm quickconfig",
    "add-windowscapability -online -name openssh.server",
    "openssh.server",
    "start-service sshd",
];

/// Bare toggle labels that mean a remote-access service (Raspberry Pi, router and NAS panels).
const REMOTE_SERVICE_TOGGLES: &[&str] = &["ssh", "sshd", "vnc", "rdp", "telnet", "sharing"];

/// Labels that say nothing beyond "change this": what they change is in the context.
const GENERIC_TOGGLE_LABELS: &[&str] = &[
    "", "on", "off", "enable", "enabled", "disable", "disabled", "turn on", "turn off", "allow",
    "ok", "apply", "save", "yes", "start", "activate", "switch", "toggle", "done",
];

const TOGGLE_ROLE_KEYS: &[&str] = &[
    "checkbox",
    "switch",
    "togglebutton",
    "toggle",
    "radio",
    "radiobutton",
    "menuitemcheckbox",
    "menuitemradio",
    "checkboxmenuitem",
];

/// Verbs that only look or move around (navigating to a settings page is not changing it).
const PASSIVE_VERBS: &[&str] = &[
    "snapshot",
    "screenshot",
    "observe",
    "read",
    "get",
    "find",
    "list",
    "wait",
    "waitfor",
    "inspect",
    "scroll",
    "hover",
    "focus",
    "zoom",
    "move",
    "mousemove",
    "navigate",
    "open",
    "openurl",
    "openapp",
    "goto",
    "go",
    "visit",
    "load",
    "browse",
    "launch",
];

fn remote_phrases() -> &'static [Keyword] {
    static K: OnceLock<Vec<Keyword>> = OnceLock::new();
    K.get_or_init(|| {
        REMOTE_ACCESS_PHRASES
            .iter()
            .map(|p| Keyword::new(p))
            .collect()
    })
}

fn phrase_in(text: &str) -> Option<&'static str> {
    let n = normalize(text);
    let folded = fold_confusables(&n);
    // Separators as spaces too, so `Remote_Login` and `remote-desktop` read as words.
    let spaced = normalize(&n.replace(['_', '-', '.', '/', ':', '?', '&', '='], " "));
    remote_phrases()
        .iter()
        .find(|k| k.matches(&n) || k.matches(&folded) || k.matches(&spaced))
        .map(Keyword::as_str)
}

fn compact(s: &str) -> String {
    normalize(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// A label with a leading role word (`checkbox "Remote Login"`) and quotes removed.
fn bare_label(label: &str) -> String {
    let n = normalize(label);
    let mut words = n.split(' ').peekable();
    if words.peek().is_some_and(|w| {
        TOGGLE_ROLE_KEYS.contains(w) || matches!(*w, "button" | "link" | "menuitem" | "option")
    }) {
        words.next();
    }
    trim_label(&words.collect::<Vec<_>>().join(" ")).to_string()
}

/// Why `action` enables or changes remote access, if it does (see the section comment).
pub(crate) fn remote_access(action: &ActionDescriptor, ctx: &ActionContext) -> Option<String> {
    let verb: String = action
        .verb
        .to_lowercase()
        .replace(['_', '-', '.', ':', '/'], " ");
    let compact_verb: String = verb.chars().filter(|c| !c.is_whitespace()).collect();
    let first = verb.split_whitespace().next().unwrap_or("");
    if PASSIVE_VERBS.contains(&compact_verb.as_str()) || PASSIVE_VERBS.contains(&first) {
        return None;
    }
    // A command typed or run anywhere.
    if let Some(p) = action.text_preview.as_deref() {
        let p = normalize(p);
        if let Some(cmd) = REMOTE_ACCESS_COMMANDS.iter().find(|c| p.contains(*c)) {
            return Some(format!("turns on remote access (command “{cmd}”)"));
        }
    }
    // The target names remote access itself ("Remote Login", "Enable Remote Desktop").
    let label = action.target_label.as_deref().unwrap_or("");
    if let Some(word) = phrase_in(label) {
        return Some(format!("changes remote access (“{word}”)"));
    }
    // A toggle or a generic control whose surroundings are about remote access.
    let role = ctx
        .role
        .as_deref()
        .map(|r| {
            r.chars()
                .filter(|c| !matches!(c, '_' | '-' | ' '))
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .unwrap_or_default();
    let toggle = TOGGLE_ROLE_KEYS.contains(&role.as_str());
    let bare = bare_label(label);
    let generic = GENERIC_TOGGLE_LABELS.contains(&bare.as_str());
    if !(toggle || generic) {
        return None;
    }
    // A switch named after the service ("SSH", "VNC"; GNOME's Sharing panel master switch).
    if toggle && REMOTE_SERVICE_TOGGLES.contains(&bare.as_str()) {
        return Some(format!("changes remote access (the “{bare}” switch)"));
    }
    let around = [ctx.window_title.as_deref(), ctx.dialog_text.as_deref()]
        .into_iter()
        .flatten()
        .chain(ctx.ancestors.iter().filter_map(|a| a.name.as_deref()));
    for text in around {
        if let Some(word) = phrase_in(text) {
            return Some(format!("changes remote access (“{word}”)"));
        }
        if toggle && trim_label(&normalize(text)) == "sharing" {
            return Some("changes remote access (the Sharing panel)".into());
        }
    }
    if let Some(url) = ctx.url.as_deref() {
        let c = compact(url);
        if let Some(u) = REMOTE_ACCESS_URLS.iter().find(|u| c.contains(*u)) {
            return Some(format!("changes remote access (page “{u}”)"));
        }
    }
    None
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

    // ---- record_approved with context ----

    #[test]
    fn record_approved_with_context_counts_the_contextual_risk() {
        let e = Arc::new(engine());
        e.update_config("limit", |c| {
            c.rate_limits.insert(RiskClass::Spend, 1);
        })
        .unwrap();
        // A generic "Continue" in a card form is a payment only in context.
        let cont = act("shop.example", "click", Some("Continue"), RiskClass::Edit);
        let ctx = ActionContext::default()
            .with_role("button")
            .with_form_fields(["Card number", "CVC", "Name on card"]);
        assert_eq!(
            e.classify(&cont, &ActionContext::default()).risk,
            RiskClass::Edit
        );
        assert_eq!(e.classify(&cont, &ctx).risk, RiskClass::Spend);

        let gate: &dyn PolicyGate = e.as_ref();
        // Without the context the approval is counted as an Edit.
        assert!(gate.record_approved(&cont, RiskClass::Edit));
        assert_eq!(e.used("task-1", RiskClass::Spend), 0);
        assert_eq!(e.used("task-1", RiskClass::Edit), 1);
        // With it, against the Spend limit, which then runs out.
        assert!(gate.record_approved_with_context(&cont, RiskClass::Edit, &ctx));
        assert_eq!(e.used("task-1", RiskClass::Spend), 1);
        assert!(!gate.record_approved_with_context(&cont, RiskClass::Edit, &ctx));
        assert_eq!(e.used("task-1", RiskClass::Spend), 1);

        // A recheck gate never counts, with or without context.
        let re = RecheckGate(Arc::clone(&e));
        assert!(re.record_approved_with_context(&cont, RiskClass::Spend, &ctx));
        assert_eq!(e.used("task-1", RiskClass::Spend), 1);

        // A gate that only implements `record_approved` still gets the call.
        struct Old(AtomicUsize);
        impl PolicyGate for Old {
            fn evaluate(&self, _: &ActionDescriptor) -> Decision {
                Decision::Allow
            }
            fn halted(&self) -> bool {
                false
            }
            fn record_approved(&self, _: &ActionDescriptor, _: RiskClass) -> bool {
                self.0.fetch_add(1, Ordering::SeqCst);
                false
            }
        }
        let old = Old(AtomicUsize::new(0));
        assert!(!old.record_approved_with_context(&cont, RiskClass::Edit, &ctx));
        assert_eq!(old.0.load(Ordering::SeqCst), 1);
    }

    // ---- remote access ----

    /// An engine where the user trusts every app with everything (the worst case for the
    /// remote-access rule).
    fn trusting_engine() -> PolicyEngine {
        let e = engine();
        e.upsert_rule(
            Rule::new("trust-all", Effect::Allow)
                .targets(["*"])
                .risks(RiskClass::ALL),
        )
        .unwrap();
        e
    }

    fn toggle(app: &str, label: &str) -> ActionDescriptor {
        act(app, "toggle", Some(label), RiskClass::Edit)
    }

    fn assert_remote(e: &PolicyEngine, a: &ActionDescriptor, ctx: &ActionContext) {
        let r = e.assess(a, ctx);
        assert_eq!(r.risk, RiskClass::Security, "{a:?} {ctx:?}");
        assert!(is_confirm(&r.decision), "{a:?} {ctx:?}: {:?}", r.decision);
        assert!(
            r.reasons.iter().any(|w| w.contains("remote access")),
            "{:?}",
            r.reasons
        );
    }

    #[test]
    fn remote_access_toggles_in_settings_apps_always_confirm() {
        let e = trusting_engine();
        let switch = ActionContext::default().with_role("switch");
        // macOS System Settings > General > Sharing.
        for label in ["Remote Login", "Screen Sharing", "Remote Management"] {
            assert_remote(&e, &toggle("com.apple.systempreferences", label), &switch);
        }
        // Windows Settings > System > Remote Desktop.
        assert_remote(
            &e,
            &toggle("windows.immersivecontrolpanel", "Remote Desktop"),
            &ActionContext::default().with_role("ToggleSwitch"),
        );
        assert_remote(
            &e,
            &act(
                "systempropertiesremote.exe",
                "click",
                Some("Allow remote connections to this computer"),
                RiskClass::Edit,
            ),
            &ActionContext::default().with_role("radio_button"),
        );
        // GNOME Settings > System > Remote Desktop, and the Sharing panel's master switch.
        assert_remote(&e, &toggle("org.gnome.Settings", "Remote Desktop"), &switch);
        assert_remote(&e, &toggle("gnome-control-center", "Sharing"), &switch);
        // An unnamed switch on a remote-access pane, found by its window title or deep link.
        assert_remote(
            &e,
            &act("org.gnome.Settings", "click", None, RiskClass::Edit),
            &switch.clone().with_window_title("Remote Desktop"),
        );
        assert_remote(
            &e,
            &act(
                "ms-settings:remotedesktop",
                "click",
                Some("On"),
                RiskClass::Edit,
            ),
            &ActionContext::default()
                .with_role("button")
                .with_url("ms-settings:remotedesktop"),
        );
        // Per-task approvals do not skip it either (Security cannot be task-approved, and
        // the forced confirmation sits after them).
        let r = e.assess(
            &toggle("com.apple.systempreferences", "Remote Login"),
            &switch,
        );
        assert_eq!(r.rule.as_deref(), Some("remote-access"));
    }

    #[test]
    fn remote_access_outside_settings_apps() {
        let e = trusting_engine();
        // A router's admin page in the browser.
        assert_remote(
            &e,
            &act(
                "192.168.1.1",
                "click",
                Some("checkbox \"Enable Remote Management\""),
                RiskClass::Edit,
            ),
            &ActionContext::default()
                .with_role("checkbox")
                .with_url("http://192.168.1.1/admin/advanced.html"),
        );
        // A NAS panel with a bare "SSH" switch.
        assert_remote(
            &e,
            &act(
                "nas.local",
                "click",
                Some("switch \"SSH\""),
                RiskClass::Edit,
            ),
            &ActionContext::default().with_role("switch"),
        );
        // Third-party remote-support apps.
        assert_remote(
            &e,
            &toggle("com.teamviewer.TeamViewer", "Grant easy access"),
            &ActionContext::default()
                .with_role("checkbox")
                .with_window_title("TeamViewer Options"),
        );
        assert_remote(
            &e,
            &toggle("com.philandro.anydesk", "Enable unattended access"),
            &ActionContext::default().with_role("checkbox"),
        );
        // A generic "Turn on" in a dialog about remote desktop.
        assert_remote(
            &e,
            &act(
                "remotedesktop.google.com",
                "click",
                Some("Turn on"),
                RiskClass::Edit,
            ),
            &ActionContext::default()
                .with_role("button")
                .with_dialog_text("Set up Chrome Remote Desktop on this computer"),
        );
        // A command typed into a terminal (or run by any shell tool).
        let mut cmd = act("com.apple.terminal", "type", Some("Shell"), RiskClass::Edit);
        cmd.text_preview = Some("sudo systemsetup -setremotelogin on".into());
        assert_remote(&e, &cmd, &ActionContext::default());
        // Localized labels.
        assert_remote(
            &e,
            &toggle("com.apple.systempreferences", "Bildschirmfreigabe"),
            &ActionContext::default().with_role("switch"),
        );
        assert_remote(
            &e,
            &toggle("com.apple.systempreferences", "远程登录"),
            &ActionContext::default().with_role("switch"),
        );
    }

    #[test]
    fn unrelated_toggles_and_navigation_are_not_remote_access() {
        let e = trusting_engine();
        let switch = ActionContext::default().with_role("switch");
        // Still Security (a settings app), but the user's Allow rule applies.
        let dark = toggle("com.apple.systempreferences", "Dark Mode");
        let r = e.assess(&dark, &switch);
        assert_eq!(r.risk, RiskClass::Security);
        assert_eq!(r.decision, Decision::Allow);
        assert!(remote_access(&dark, &switch).is_none());
        // A toggle elsewhere, a generic button on an unrelated page.
        for (a, ctx) in [
            (
                toggle("com.apple.mail", "Show previews"),
                switch.clone().with_window_title("Mail Settings"),
            ),
            (
                act("shop.example", "click", Some("OK"), RiskClass::Edit),
                ActionContext::default()
                    .with_role("button")
                    .with_url("https://shop.example/account/sharing-preferences"),
            ),
            (
                act(
                    "docs.example",
                    "click",
                    Some("link \"SSH\""),
                    RiskClass::Edit,
                ),
                ActionContext::default().with_role("link"),
            ),
            (
                act("com.apple.notes", "type", Some("Body"), RiskClass::Edit),
                ActionContext::default(),
            ),
        ] {
            assert!(remote_access(&a, &ctx).is_none(), "{a:?} {ctx:?}");
            assert_eq!(e.assess(&a, &ctx).decision, Decision::Allow, "{a:?}");
        }
        // Looking at or opening the Sharing pane changes nothing.
        let open = act(
            "com.apple.systempreferences",
            "open_url",
            Some("x-apple.systempreferences:com.apple.preferences.sharing?Services_RemoteLogin"),
            RiskClass::Observe,
        );
        assert!(remote_access(&open, &ActionContext::default()).is_none());
        let read = act(
            "com.apple.systempreferences",
            "read",
            Some("Remote Login"),
            RiskClass::Observe,
        );
        assert!(remote_access(&read, &switch).is_none());
    }

    #[test]
    fn remote_access_deny_rules_still_win() {
        let e = trusting_engine();
        e.upsert_rule(Rule::new("no-router", Effect::Deny).targets(["192.168.1.1"]))
            .unwrap();
        let a = toggle("192.168.1.1", "Remote Management");
        assert!(is_deny(
            &e.assess(&a, &ActionContext::default().with_role("checkbox"))
                .decision
        ));
    }
}
