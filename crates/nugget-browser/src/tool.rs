//! Browser actions as agent tools.
//!
//! `nugget-agent` is being built concurrently, so this module defines the minimal interface it
//! needs ([`AgentTool`]) instead of depending on it. Mapping, once the agent's registry lands:
//!
//! | here                         | `nugget-agent` / `nugget-infer`                          |
//! |------------------------------|----------------------------------------------------------|
//! | [`ToolSpec`] `name`, `description`, `input_schema` | `nugget_infer::ToolSpec` (same three fields) |
//! | [`AgentTool::call`]          | the registry's execute call, with the task id from its context |
//! | [`ToolError::is_mutation_refused`] | the loop's "do not retry; re-plan or ask the user" signal |
//! | [`ToolSpec::mutating`]       | whether the loop runs its verify step after the call     |
//!
//! A thin adapter (`impl agent::Tool for Adapter(Arc<dyn AgentTool>)`) is all the agent needs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::action::{BrowserAction, WaitCondition};
use crate::controller::{BrowserController, BrowserError};
use crate::protocol::SnapshotOptions;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the input object.
    pub input_schema: Value,
    /// Whether the tool can change the page (and so is followed by verification).
    pub mutating: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolContext {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error(transparent)]
    Browser(#[from] BrowserError),
}

impl ToolError {
    /// The action was refused (policy, user, kill switch) rather than failed: never retry it.
    pub fn is_mutation_refused(&self) -> bool {
        matches!(
            self,
            Self::Browser(
                BrowserError::Halted | BrowserError::Denied(_) | BrowserError::Declined(_)
            )
        )
    }
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<Value, ToolError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Snapshot,
    Click,
    Type,
    Select,
    Scroll,
    Navigate,
    Submit,
    WaitFor,
}

const ALL: [Kind; 8] = [
    Kind::Snapshot,
    Kind::Click,
    Kind::Type,
    Kind::Select,
    Kind::Scroll,
    Kind::Navigate,
    Kind::Submit,
    Kind::WaitFor,
];

struct BrowserTool {
    kind: Kind,
    controller: Arc<BrowserController>,
}

/// All browser tools, sharing one controller (and so one "latest snapshot").
pub fn browser_tools(controller: Arc<BrowserController>) -> Vec<Arc<dyn AgentTool>> {
    ALL.iter()
        .map(|&kind| {
            Arc::new(BrowserTool {
                kind,
                controller: controller.clone(),
            }) as Arc<dyn AgentTool>
        })
        .collect()
}

fn element_schema() -> Value {
    json!({ "type": "string", "description": "Element id from the latest browser_snapshot, e.g. \"n12\"" })
}

fn spec(kind: Kind) -> ToolSpec {
    let (name, description, input_schema, mutating) = match kind {
        Kind::Snapshot => (
            "browser_snapshot",
            "Read the current browser tab: URL, title, visible text and the interactive elements with ids. \
             Call this before any other browser tool and after the page changes. Password and payment field values are never included.",
            json!({ "type": "object", "properties": {
                "include_text": { "type": "boolean", "description": "Include page text (default true)" }
            }, "additionalProperties": false }),
            false,
        ),
        Kind::Click => (
            "browser_click",
            "Click an element from the latest snapshot.",
            json!({ "type": "object", "properties": { "element": element_schema() },
                "required": ["element"], "additionalProperties": false }),
            true,
        ),
        Kind::Type => (
            "browser_type",
            "Set the text of an editable field (replaces its value unless clear is false). Never works on password or payment fields.",
            json!({ "type": "object", "properties": {
                "element": element_schema(),
                "text": { "type": "string", "maxLength": crate::action::MAX_TYPE_CHARS },
                "clear": { "type": "boolean", "description": "Replace the current value (default true)" }
            }, "required": ["element", "text"], "additionalProperties": false }),
            true,
        ),
        Kind::Select => (
            "browser_select",
            "Choose an option of a select element by value or visible label.",
            json!({ "type": "object", "properties": {
                "element": element_schema(), "value": { "type": "string" }
            }, "required": ["element", "value"], "additionalProperties": false }),
            true,
        ),
        Kind::Scroll => (
            "browser_scroll",
            "Scroll the page (or bring an element into view). Without offsets scrolls down most of a screen.",
            json!({ "type": "object", "properties": {
                "element": element_schema(), "dx": { "type": "number" }, "dy": { "type": "number" }
            }, "additionalProperties": false }),
            false,
        ),
        Kind::Navigate => (
            "browser_navigate",
            "Open an http(s) URL in the current tab. Only sites the user allowed in the extension work.",
            json!({ "type": "object", "properties": { "url": { "type": "string" } },
                "required": ["url"], "additionalProperties": false }),
            true,
        ),
        Kind::Submit => (
            "browser_submit",
            "Submit the form an element belongs to.",
            json!({ "type": "object", "properties": { "element": element_schema() },
                "required": ["element"], "additionalProperties": false }),
            true,
        ),
        Kind::WaitFor => (
            "browser_wait_for",
            "Wait until text appears, an element appears (by role and/or name), the URL contains a string, or an element disappears.",
            json!({ "type": "object", "properties": {
                "text_present": { "type": "string" },
                "role": { "type": "string" },
                "name": { "type": "string" },
                "url_contains": { "type": "string" },
                "element_gone": element_schema(),
                "timeout_ms": { "type": "integer", "minimum": 0, "maximum": crate::action::MAX_WAIT_MS }
            }, "additionalProperties": false }),
            false,
        ),
    };
    ToolSpec {
        name,
        description,
        input_schema,
        mutating,
    }
}

fn parse<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ElementInput {
    element: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitInput {
    text_present: Option<String>,
    role: Option<String>,
    name: Option<String>,
    url_contains: Option<String>,
    element_gone: Option<String>,
    timeout_ms: Option<u64>,
}

fn to_action(kind: Kind, input: Value) -> Result<BrowserAction, ToolError> {
    let mut input = if input.is_null() { json!({}) } else { input };
    Ok(match kind {
        Kind::Snapshot => unreachable!("not an action"),
        Kind::Click => BrowserAction::Click {
            element: parse::<ElementInput>(input)?.element,
        },
        Kind::Submit => BrowserAction::Submit {
            element: parse::<ElementInput>(input)?.element,
        },
        Kind::Type | Kind::Select | Kind::Scroll | Kind::Navigate => {
            let tag = match kind {
                Kind::Type => "type",
                Kind::Select => "select",
                Kind::Scroll => "scroll",
                _ => "navigate",
            };
            if let Some(obj) = input.as_object_mut() {
                obj.insert("kind".into(), json!(tag));
            }
            parse(input)?
        }
        Kind::WaitFor => {
            let w: WaitInput = parse(input)?;
            let mut conds = Vec::new();
            if let Some(t) = w.text_present {
                conds.push(WaitCondition::TextPresent(t));
            }
            if w.role.is_some() || w.name.is_some() {
                conds.push(WaitCondition::ElementPresent {
                    role: w.role,
                    name: w.name,
                });
            }
            if let Some(u) = w.url_contains {
                conds.push(WaitCondition::UrlContains(u));
            }
            if let Some(e) = w.element_gone {
                conds.push(WaitCondition::ElementGone(e));
            }
            if conds.len() != 1 {
                return Err(ToolError::InvalidInput(
                    "give exactly one of text_present, role/name, url_contains, element_gone"
                        .into(),
                ));
            }
            BrowserAction::WaitFor {
                until: conds.remove(0),
                timeout_ms: w.timeout_ms,
            }
        }
    })
}

#[async_trait]
impl AgentTool for BrowserTool {
    fn spec(&self) -> ToolSpec {
        spec(self.kind)
    }

    async fn call(&self, ctx: &ToolContext, input: Value) -> Result<Value, ToolError> {
        if self.kind == Kind::Snapshot {
            #[derive(Deserialize, Default)]
            #[serde(deny_unknown_fields)]
            struct SnapInput {
                include_text: Option<bool>,
            }
            let i: SnapInput = if input.is_null() {
                SnapInput::default()
            } else {
                parse(input)?
            };
            let snap = self
                .controller
                .snapshot(
                    &ctx.task_id,
                    SnapshotOptions {
                        include_text: i.include_text,
                        include_hidden: None,
                    },
                )
                .await?;
            return Ok(serde_json::to_value(snap).unwrap_or(Value::Null));
        }
        let action = to_action(self.kind, input)?;
        let outcome = self.controller.act(&ctx.task_id, action).await?;
        Ok(json!({
            "verification": outcome.verification,
            "diff": outcome.diff,
            "snapshot": outcome.snapshot,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_inputs_map_to_actions() {
        assert_eq!(
            to_action(Kind::Type, json!({"element": "n1", "text": "hi"})).unwrap(),
            BrowserAction::Type {
                element: "n1".into(),
                text: "hi".into(),
                clear: None
            }
        );
        assert_eq!(
            to_action(Kind::Scroll, Value::Null).unwrap(),
            BrowserAction::Scroll {
                element: None,
                dx: None,
                dy: None
            }
        );
        assert_eq!(
            to_action(Kind::WaitFor, json!({"role": "button", "name": "OK"})).unwrap(),
            BrowserAction::WaitFor {
                until: WaitCondition::ElementPresent {
                    role: Some("button".into()),
                    name: Some("OK".into())
                },
                timeout_ms: None
            }
        );
        assert!(to_action(Kind::Click, json!({"element": "n1", "x": 1})).is_err());
        assert!(
            to_action(
                Kind::Type,
                json!({"element": "n1", "text": "a", "kind": "click"})
            )
            .is_ok_and(|a| matches!(a, BrowserAction::Type { .. }))
        );
        assert!(
            to_action(
                Kind::WaitFor,
                json!({"text_present": "a", "url_contains": "b"})
            )
            .is_err()
        );
        assert!(to_action(Kind::WaitFor, json!({})).is_err());
    }

    #[test]
    fn specs_are_unique_objects() {
        let names: std::collections::HashSet<_> = ALL.iter().map(|&k| spec(k).name).collect();
        assert_eq!(names.len(), ALL.len());
        for k in ALL {
            assert_eq!(spec(k).input_schema["type"], "object");
        }
    }
}
