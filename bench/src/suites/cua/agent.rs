//! The agent-under-test interface, and the built-in agents.
//!
//! Wave 2 plugs the real `nugget-agent` in by implementing [`CuaAgent`]: it reads the
//! instruction from [`TaskBrief`], perceives through [`Session::observe`] (or, in the `xvfb`
//! environment, through AT-SPI and screenshots on [`Session::display`]), acts with
//! [`Session::act`], asks the user through [`Session::confirm`] whenever its policy gate returns
//! `Confirm`, reports model usage with [`Session::record_usage`], and returns an [`Outcome`].

use std::collections::HashMap;

use nugget_policy::ActionDescriptor;
use serde::{Deserialize, Serialize};

use super::apps;
use super::task::{Finish, Step, Task};
use super::ui::{Action, Observation};

/// What the agent is told about a task. Checkers, risky lists and the reference are hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBrief {
    pub id: String,
    pub instruction: String,
    /// Apps whose windows are open.
    pub apps: Vec<String>,
    pub max_steps: u32,
}

impl From<&Task> for TaskBrief {
    fn from(t: &Task) -> Self {
        Self {
            id: t.id.clone(),
            instruction: t.instruction.clone(),
            apps: t.apps.clone(),
            max_steps: t.max_steps,
        }
    }
}

/// How the agent ended the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// It believes the task is complete (or stopped because the user declined).
    Done { summary: String },
    /// It needs the user to clarify before acting.
    AskUser { question: String },
    /// It declines to do the task.
    Refused { reason: String },
    /// It could not finish (error, budget, no idea).
    GaveUp { reason: String },
    /// It ran out of wall-clock time (the task timeout). A failure, listed separately.
    Timeout { reason: String },
}

impl Outcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Done { .. } => "done",
            Outcome::AskUser { .. } => "ask",
            Outcome::Refused { .. } => "refuse",
            Outcome::GaveUp { .. } => "gave_up",
            Outcome::Timeout { .. } => "timeout",
        }
    }
}

/// Result of one action. A rejected action (validation error, disabled control) still costs a
/// step, as it would on a real screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActResult {
    pub ok: bool,
    /// What the user would see on failure (`"Enter a valid email address"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error("step budget of {0} exhausted")]
    StepBudget(u32),
    #[error("environment: {0}")]
    Transport(String),
}

/// The agent's handle on the environment for one task.
pub trait Session {
    /// The accessibility tree of every open window. Free (not a step).
    fn observe(&mut self) -> Result<Observation, EnvError>;
    /// One input action. Counts against the step budget.
    fn act(&mut self, action: &Action) -> Result<ActResult, EnvError>;
    /// Ask the (simulated) user to approve a risky action. `true` means approved; the approval
    /// covers one matching action.
    fn confirm(&mut self, action: &ActionDescriptor) -> Result<bool, EnvError>;
    /// Ask the (simulated) user a clarifying question, with the candidates offered (empty: free
    /// text). Returns their reply, or `None` when they give none (the agent then stops with
    /// [`Outcome::AskUser`]). The harness records every question in order with the world's
    /// changes, so it can tell whether the agent asked before acting.
    fn ask(&mut self, question: &str, options: &[String]) -> Result<Option<String>, EnvError> {
        let _ = (question, options);
        Ok(None)
    }
    /// Model tokens spent, for the cost metrics.
    fn record_usage(&mut self, input_tokens: u64, output_tokens: u64);
    fn steps_taken(&self) -> u32;
    /// The X display the apps are shown on (`:91`), in the `xvfb` environment.
    fn display(&self) -> Option<&str> {
        None
    }
}

pub trait CuaAgent: Send {
    fn name(&self) -> &str;
    fn run(&mut self, task: &TaskBrief, session: &mut dyn Session) -> Outcome;
    /// Extra report parameters (the model under test, say).
    fn params(&self) -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }
    /// Extra metrics over the tasks run so far, reported without targets.
    fn metrics(&self) -> Vec<crate::report::Metric> {
        Vec::new()
    }
    /// Play a strict simulated user ([`super::world::World::set_strict_user`]): approve only
    /// the confirmations the task asked for. Off for every built-in agent but the compliant arm.
    fn strict_user(&self) -> bool {
        false
    }
}

/// Build the descriptor for an action key such as `shop.pay`.
pub fn descriptor(key: &str, task_id: &str) -> ActionDescriptor {
    let (app, kind) = key.split_once('.').unwrap_or((key, ""));
    ActionDescriptor {
        target_app: app.to_owned(),
        verb: kind.to_owned(),
        target_label: None,
        text_preview: None,
        declared_risk: apps::risk_of(app, kind),
        task_id: task_id.to_owned(),
    }
}

fn to_action(step: &Step) -> Option<Action> {
    Some(match step {
        Step::Click(t) => Action::Click { target: t.clone() },
        Step::Type(t, s) => Action::Type {
            target: t.clone(),
            text: s.clone(),
        },
        Step::Select(t, o) => Action::Select {
            target: t.clone(),
            option: o.clone(),
        },
        Step::Confirm(_) | Step::Ask(_) | Step::Finish(_) => return None,
    })
}

/// Play a script through the session. Every action's target must be visible in the latest
/// observation, so a passing script proves the task is solvable through the UI alone.
fn play(steps: &[Step], session: &mut dyn Session, task_id: &str, confirm: bool) -> Outcome {
    for (i, step) in steps.iter().enumerate() {
        match step {
            Step::Finish(f) => {
                return match f {
                    Finish::Done => Outcome::Done {
                        summary: "done".into(),
                    },
                    Finish::Ask => {
                        let question = "Which one do you mean?";
                        // Recorded; there is no answer to carry on with at the end.
                        let _ = session.ask(question, &[]);
                        Outcome::AskUser {
                            question: question.into(),
                        }
                    }
                    Finish::Refuse => Outcome::Refused {
                        reason: "This would be harmful or deceptive.".into(),
                    },
                };
            }
            Step::Ask(question) => match session.ask(question, &[]) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Outcome::AskUser {
                        question: question.clone(),
                    };
                }
                Err(e) => {
                    return Outcome::GaveUp {
                        reason: e.to_string(),
                    };
                }
            },
            Step::Confirm(key) => {
                if !confirm {
                    continue;
                }
                match session.confirm(&descriptor(key, task_id)) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Outcome::Done {
                            summary: format!("stopped: the user declined {key}"),
                        };
                    }
                    Err(e) => {
                        return Outcome::GaveUp {
                            reason: e.to_string(),
                        };
                    }
                }
            }
            _ => {
                let Some(action) = to_action(step) else {
                    continue;
                };
                match session.observe() {
                    Ok(obs) if obs.find(action.target()).is_some() => {}
                    Ok(_) => {
                        return Outcome::GaveUp {
                            reason: format!("step {i}: `{}` is not on screen", action.target()),
                        };
                    }
                    Err(e) => {
                        return Outcome::GaveUp {
                            reason: e.to_string(),
                        };
                    }
                }
                match session.act(&action) {
                    Ok(r) if r.ok => {}
                    Ok(r) => {
                        return Outcome::GaveUp {
                            reason: format!(
                                "step {i} {action:?}: {}",
                                r.message.unwrap_or_default()
                            ),
                        };
                    }
                    Err(e) => {
                        return Outcome::GaveUp {
                            reason: e.to_string(),
                        };
                    }
                }
            }
        }
    }
    Outcome::Done {
        summary: "script ended".into(),
    }
}

/// Plays each task's scripted reference solution. Validates the harness: on a correct harness
/// it scores 100% with no safety violations.
pub struct ReferenceAgent {
    scripts: HashMap<String, Vec<Step>>,
}

impl ReferenceAgent {
    pub fn new(tasks: &[Task]) -> Self {
        Self {
            scripts: tasks
                .iter()
                .map(|t| (t.id.clone(), t.reference.clone()))
                .collect(),
        }
    }
}

impl CuaAgent for ReferenceAgent {
    fn name(&self) -> &str {
        "reference"
    }
    fn run(&mut self, task: &TaskBrief, session: &mut dyn Session) -> Outcome {
        match self.scripts.get(&task.id) {
            Some(s) => play(s, session, &task.id, true),
            None => Outcome::GaveUp {
                reason: "no reference script".into(),
            },
        }
    }
}

/// Does nothing. The floor every real agent must beat, and a check that no task passes by
/// default.
pub struct NullAgent;

impl CuaAgent for NullAgent {
    fn name(&self) -> &str {
        "null"
    }
    fn run(&mut self, _task: &TaskBrief, _session: &mut dyn Session) -> Outcome {
        Outcome::GaveUp {
            reason: "null agent".into(),
        }
    }
}

/// Red-team control: follows every injected instruction, then the reference solution, and never
/// asks for confirmation. Proves the safety metrics catch violations (it must score non-zero
/// unconfirmed risky actions and injection compliance).
pub struct NaiveAgent {
    scripts: HashMap<String, Vec<Step>>,
}

impl NaiveAgent {
    pub fn new(tasks: &[Task]) -> Self {
        Self {
            scripts: tasks
                .iter()
                .map(|t| {
                    let mut s: Vec<Step> = t
                        .injection
                        .as_ref()
                        .map(|i| i.comply.clone())
                        .unwrap_or_default();
                    s.retain(|x| !matches!(x, Step::Finish(_)));
                    s.extend(t.reference.iter().cloned());
                    (t.id.clone(), s)
                })
                .collect(),
        }
    }
}

impl CuaAgent for NaiveAgent {
    fn name(&self) -> &str {
        "naive"
    }
    fn run(&mut self, task: &TaskBrief, session: &mut dyn Session) -> Outcome {
        let Some(steps) = self.scripts.get(&task.id) else {
            return Outcome::GaveUp {
                reason: "no script".into(),
            };
        };
        // Injected steps may fail (the injected path can be impossible); keep going regardless.
        for step in steps {
            match step {
                Step::Finish(_) => {
                    return play(std::slice::from_ref(step), session, &task.id, false);
                }
                Step::Confirm(_) | Step::Ask(_) => {}
                s => {
                    if let Some(a) = to_action(s)
                        && let Err(EnvError::StepBudget(_)) = session.act(&a)
                    {
                        return Outcome::GaveUp {
                            reason: "budget".into(),
                        };
                    }
                }
            }
        }
        Outcome::Done {
            summary: "done".into(),
        }
    }
}
