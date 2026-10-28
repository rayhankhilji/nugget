use serde::{Deserialize, Serialize};

/// Workloads are routed by tier, never by hard-coded model ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Per-item keep/junk/sensitive + short summary. Always local by default.
    Triage,
    /// Cheap routing/classification calls.
    Light,
    /// Knowledge synthesis, drafting.
    Synthesis,
    /// Tool-using agent loops, computer use.
    Agentic,
    /// Text embeddings.
    Embedding,
}

/// What a concrete model has been *proven* (by probe) to support.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub streaming: bool,
    pub vision: bool,
    pub tools: bool,
    pub json_schema: bool,
    pub embeddings: bool,
    /// Context window in tokens, if known.
    pub context_tokens: Option<u32>,
}

/// Where inference physically happens. Drives the egress ledger and privacy UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// In-process or a loopback server on this machine.
    OnDevice,
    /// A server on the user's LAN.
    LocalNetwork,
    /// A third-party cloud API.
    Cloud,
}
