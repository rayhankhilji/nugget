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
    /// Cross-encoder reranking of retrieval candidates. Marks manifest rerankers (run by the
    /// local runtime's `rerank` role); it is never a router tier, and a model carrying it serves
    /// neither chat nor embeddings.
    Rerank,
}

impl Tier {
    /// Every tier, in declaration order.
    pub const ALL: [Tier; 6] = [
        Tier::Triage,
        Tier::Light,
        Tier::Synthesis,
        Tier::Agentic,
        Tier::Embedding,
        Tier::Rerank,
    ];

    /// Tiers the router routes requests for (all but [`Tier::Rerank`]).
    pub const ROUTED: [Tier; 5] = [
        Tier::Triage,
        Tier::Light,
        Tier::Synthesis,
        Tier::Agentic,
        Tier::Embedding,
    ];

    /// A chat (text generation) tier: neither embedding nor rerank.
    pub fn is_chat(self) -> bool {
        matches!(
            self,
            Tier::Triage | Tier::Light | Tier::Synthesis | Tier::Agentic
        )
    }

    /// The tier's serde name (`"triage"`, ..., `"rerank"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Triage => "triage",
            Tier::Light => "light",
            Tier::Synthesis => "synthesis",
            Tier::Agentic => "agentic",
            Tier::Embedding => "embedding",
            Tier::Rerank => "rerank",
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_names_match_serde_and_rerank_is_neither_chat_nor_routed() {
        for t in Tier::ALL {
            assert_eq!(
                serde_json::to_value(t).unwrap(),
                serde_json::Value::from(t.as_str())
            );
        }
        let r: Tier = serde_json::from_str("\"rerank\"").unwrap();
        assert_eq!(r, Tier::Rerank);
        assert!(!Tier::Rerank.is_chat() && !Tier::Embedding.is_chat());
        assert!(!Tier::ROUTED.contains(&Tier::Rerank));
        assert_eq!(Tier::ROUTED.len() + 1, Tier::ALL.len());
        assert!(
            Tier::ALL
                .iter()
                .filter(|t| t.is_chat())
                .all(|t| Tier::ROUTED.contains(t))
        );
    }
}
