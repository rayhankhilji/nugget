//! [`Session`] implementations: in process, or over the apps' loopback socket.

use std::net::SocketAddr;
use std::sync::Arc;

use nugget_policy::ActionDescriptor;
use serde_json::Value;

use super::agent::{ActResult, EnvError, Session};
use super::server::{Stage, request_json};
use super::ui::{Action, Observation};

/// How a session reaches the world.
#[derive(Clone)]
pub enum Transport {
    /// Direct calls on the shared stage.
    Local(Arc<Stage>),
    /// HTTP to the apps server, exactly as an out-of-process agent would.
    Socket(SocketAddr),
}

/// One task's session: transport plus step budget and usage accounting.
pub struct EpisodeSession {
    transport: Transport,
    max_steps: u32,
    steps: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    display: Option<String>,
}

impl EpisodeSession {
    pub fn new(transport: Transport, max_steps: u32, display: Option<String>) -> Self {
        Self {
            transport,
            max_steps,
            steps: 0,
            input_tokens: 0,
            output_tokens: 0,
            display,
        }
    }
}

fn transport_err(e: String) -> EnvError {
    EnvError::Transport(e)
}

fn from_json<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, EnvError> {
    serde_json::from_value(v).map_err(|e| EnvError::Transport(e.to_string()))
}

impl Session for EpisodeSession {
    fn observe(&mut self) -> Result<Observation, EnvError> {
        match &self.transport {
            Transport::Local(s) => s.with(|w| w.observe()).map_err(transport_err),
            Transport::Socket(a) => {
                from_json(request_json(*a, "GET", "/__agent/observe", None).map_err(transport_err)?)
            }
        }
    }

    fn act(&mut self, action: &Action) -> Result<ActResult, EnvError> {
        if self.steps >= self.max_steps {
            return Err(EnvError::StepBudget(self.max_steps));
        }
        self.steps += 1;
        match &self.transport {
            Transport::Local(s) => s.act(action).map_err(transport_err),
            Transport::Socket(a) => {
                let body =
                    serde_json::to_value(action).map_err(|e| transport_err(e.to_string()))?;
                from_json(
                    request_json(*a, "POST", "/__agent/act", Some(&body)).map_err(transport_err)?,
                )
            }
        }
    }

    fn confirm(&mut self, action: &ActionDescriptor) -> Result<bool, EnvError> {
        match &self.transport {
            Transport::Local(s) => s.with(|w| w.confirm(action)).map_err(transport_err),
            Transport::Socket(a) => {
                let body =
                    serde_json::to_value(action).map_err(|e| transport_err(e.to_string()))?;
                let v = request_json(*a, "POST", "/__agent/confirm", Some(&body))
                    .map_err(transport_err)?;
                v.get("approved")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| transport_err("bad confirm reply".into()))
            }
        }
    }

    fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
    }

    fn steps_taken(&self) -> u32 {
        self.steps
    }

    fn display(&self) -> Option<&str> {
        self.display.as_deref()
    }
}
