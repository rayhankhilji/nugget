//! The [`Tool`] trait, its result and verification types, the [`ToolRegistry`], and the built-in
//! tools every run has: [`FinishTool`], [`AskUserTool`] and [`NoteTool`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use nugget_infer::ToolSpec;
use nugget_policy::RiskClass;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Observed state relevant to one action, captured by [`Tool::observe`] before and after it runs.
/// `None` when the tool cannot observe.
pub type Snapshot = Option<Value>;

/// What a call is handed besides its arguments.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub task_id: String,
    /// The idempotent action id (stable across replays of the same model call). Tools that talk
    /// to an API with idempotency keys should pass it through.
    pub action_id: String,
    /// Model turn number, from 1.
    pub step: u32,
    /// Fires when the run stops (kill switch, cancel, deadline). Long-running tools should check it.
    pub cancel: CancellationToken,
}

/// Control flow a tool can ask of the loop. Only the built-ins use it today, but any tool may.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "control", rename_all = "snake_case")]
pub enum Control {
    /// End the run with the model's claim (which the loop checks against verification).
    Finish { summary: String, claim: Claim },
    /// Suspend the run until the user answers.
    AskUser { question: String },
}

/// What the model says happened when it calls `finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Claim {
    Success,
    Partial,
    Failure,
}

/// The outcome of one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Text returned to the model.
    pub content: String,
    /// Structured result for the log and the UI (never sent to the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
    /// The content came from outside (screen text, web pages, email bodies, files) and may carry
    /// injected instructions. The loop wraps it as data and scans it.
    #[serde(default)]
    pub untrusted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: false,
            untrusted: false,
            control: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::ok(content)
        }
    }

    /// Content observed from the outside world.
    pub fn untrusted(content: impl Into<String>) -> Self {
        Self {
            untrusted: true,
            ..Self::ok(content)
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn with_control(mut self, control: Control) -> Self {
        self.control = Some(control);
        self
    }
}

/// Did a mutating action have its intended effect, judged from observed state?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum Verification {
    /// The observed after-state shows the intended effect.
    Passed(String),
    /// The observed state contradicts the intended effect.
    Failed(String),
    /// The tool cannot check this action. A run whose mutations are unverifiable never reports
    /// plain success.
    Unverifiable,
}

impl Verification {
    pub fn label(&self) -> &'static str {
        match self {
            Verification::Passed(_) => "passed",
            Verification::Failed(_) => "failed",
            Verification::Unverifiable => "unverifiable",
        }
    }
}

/// What a proposed call targets, for the policy engine and the user. The loop adds the risk and
/// the task id to make an [`nugget_policy::ActionDescriptor`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionTarget {
    pub target_app: String,
    pub verb: String,
    pub target_label: Option<String>,
    pub text_preview: Option<String>,
}

/// A capability the model can invoke. Computer-use and browser tools live in other crates and
/// implement this.
///
/// Call order for one action: [`risk`](Tool::risk) and [`describe`](Tool::describe) for the
/// policy gate; for mutating actions (`Edit` and above) [`observe`](Tool::observe) before,
/// [`call`](Tool::call), [`observe`](Tool::observe) after, then [`verify`](Tool::verify).
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique name the model calls (`[a-z0-9_.-]`, as providers require).
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema of the arguments object. Calls whose arguments fail it are not executed.
    fn schema(&self) -> Value;
    /// The worst consequence any call to this tool can have.
    fn declared_risk(&self) -> RiskClass;

    /// Risk of this particular call. May refine [`declared_risk`](Tool::declared_risk) upward
    /// (or downward for clearly read-only argument sets the tool itself recognizes).
    fn risk(&self, _args: &Value) -> RiskClass {
        self.declared_risk()
    }

    /// Target and preview for the policy gate and the confirmation prompt.
    fn describe(&self, _args: &Value) -> ActionTarget {
        ActionTarget {
            target_app: "nugget".into(),
            verb: self.name().into(),
            ..ActionTarget::default()
        }
    }

    /// What the tool can see around this call (the target's role, its containers, the form and
    /// dialog it sits in), for [`nugget_policy::PolicyGate::evaluate_with_context`]. Default:
    /// nothing.
    fn context(&self, _args: &Value) -> nugget_policy::ActionContext {
        nugget_policy::ActionContext::default()
    }

    /// Arguments as they may be written to the action log. The default masks values of keys that
    /// look secret and truncates long strings; tools handling sensitive text override it.
    fn redact(&self, args: &Value) -> Value {
        redact_default(args)
    }

    /// Capture the state this call is about to change. Used around mutating actions.
    async fn observe(&self, _args: &Value, _ctx: &ToolCtx) -> Snapshot {
        None
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolResult;

    /// Compare observed state before and after a mutating call. Default: cannot verify.
    fn verify(
        &self,
        _args: &Value,
        _before: &Snapshot,
        _after: &Snapshot,
        _result: &ToolResult,
    ) -> Verification {
        Verification::Unverifiable
    }
}

/// Tools available to a run, by name. Always contains the built-ins.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// A registry with `finish`, `ask_user` and `note`.
    pub fn new() -> Self {
        let mut r = Self {
            tools: BTreeMap::new(),
        };
        r.register(Arc::new(FinishTool));
        r.register(Arc::new(AskUserTool));
        r.register(Arc::new(NoteTool));
        r
    }

    /// Add or replace a tool. Returns the tool it replaced.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        self.tools.insert(tool.name().to_string(), tool)
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Specs for the model, sorted by name (stable prompts cache better).
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.schema(),
            })
            .collect()
    }
}

/// Name of the built-in tool that ends a run.
pub const FINISH: &str = "finish";
/// Name of the built-in tool that suspends a run for the user.
pub const ASK_USER: &str = "ask_user";
/// Name of the built-in scratchpad tool.
pub const NOTE: &str = "note";

/// `finish(summary, outcome)`: end the task. The claim is checked against verification.
pub struct FinishTool;

#[async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        FINISH
    }
    fn description(&self) -> &str {
        "End the task. `outcome` is success, partial or failure; `summary` says what was done \
         and what was observed. Success is only reported if every change was verified."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "outcome": {"type": "string", "enum": ["success", "partial", "failure"]}
            },
            "required": ["summary", "outcome"],
            "additionalProperties": false
        })
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        let summary = args["summary"].as_str().unwrap_or_default().to_string();
        let claim = match args["outcome"].as_str() {
            Some("success") => Claim::Success,
            Some("partial") => Claim::Partial,
            _ => Claim::Failure,
        };
        ToolResult::ok("finished").with_control(Control::Finish { summary, claim })
    }
}

/// `ask_user(question)`: suspend the run until the user answers.
pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        ASK_USER
    }
    fn description(&self) -> &str {
        "Ask the user a question when the task is ambiguous or needs information only they have. \
         The task pauses until they answer."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"question": {"type": "string"}},
            "required": ["question"],
            "additionalProperties": false
        })
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, args: Value, _ctx: &ToolCtx) -> ToolResult {
        let question = args["question"].as_str().unwrap_or_default().to_string();
        ToolResult::ok("waiting for the user").with_control(Control::AskUser { question })
    }
}

/// `note(text)`: record a thought or plan in the action log. No effect on the world.
pub struct NoteTool;

#[async_trait]
impl Tool for NoteTool {
    fn name(&self) -> &str {
        NOTE
    }
    fn description(&self) -> &str {
        "Write down a plan, a finding or a reminder for later steps. Has no effect outside the task."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
            "additionalProperties": false
        })
    }
    fn declared_risk(&self) -> RiskClass {
        RiskClass::Observe
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolResult {
        ToolResult::ok("noted")
    }
}

/// Longest string kept in logged arguments.
pub const LOG_STRING_MAX: usize = 200;

const SECRET_KEY_HINTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "otp",
    "pin",
    "cvv",
    "cvc",
    "card_number",
    "ssn",
    "credential",
    "private_key",
];

/// The default argument redaction: values under secret-looking keys become `"[redacted]"`,
/// strings are cut to [`LOG_STRING_MAX`] characters.
pub fn redact_default(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let lower = k.to_ascii_lowercase();
                    let secret = SECRET_KEY_HINTS.iter().any(|h| lower.contains(h));
                    let v = if secret {
                        Value::String("[redacted]".into())
                    } else {
                        redact_default(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_default).collect()),
        Value::String(s) => Value::String(truncate(s, LOG_STRING_MAX)),
        other => other.clone(),
    }
}

/// `s` cut to at most `max` characters, with an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_masks_secret_keys_recursively_and_truncates() {
        let long = "x".repeat(500);
        let v = json!({
            "user": "me",
            "Password": "hunter2",
            "nested": {"api_key": "k", "list": [{"otp_code": "123"}]},
            "body": long,
        });
        let r = redact_default(&v);
        assert_eq!(r["user"], "me");
        assert_eq!(r["Password"], "[redacted]");
        assert_eq!(r["nested"]["api_key"], "[redacted]");
        assert_eq!(r["nested"]["list"][0]["otp_code"], "[redacted]");
        assert_eq!(
            r["body"].as_str().unwrap().chars().count(),
            LOG_STRING_MAX + 1
        );
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "hé…");
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn registry_has_builtins_and_sorted_specs() {
        let r = ToolRegistry::new();
        let names: Vec<_> = r.specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["ask_user", "finish", "note"]);
    }
}
