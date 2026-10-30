//! The `draft` fixture corpus (`bench/fixtures/draft/`), compiled into the binary so a run never
//! depends on the working directory. Everything in it is fictional.

use std::collections::HashSet;

use nugget_core::ipc::DraftContext;
use serde::Deserialize;

const PERSONAS_JSON: &str = include_str!("../../../fixtures/draft/personas.json");
const KNOWLEDGE_JSON: &str = include_str!("../../../fixtures/draft/knowledge.json");
const TASKS_JSON: &str = include_str!("../../../fixtures/draft/tasks.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub email: String,
    pub role: String,
    /// Human description of the voice (documentation only; never shown to a model).
    pub voice: String,
    /// The voice training set: messages the persona sent, oldest first.
    pub sent: Vec<SentMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SentMessage {
    pub id: String,
    /// `email` or `chat`.
    pub channel: String,
    pub to: String,
    pub to_name: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub app: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeItem {
    pub id: String,
    pub date: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// Marked sensitive (as PII triage would): must never reach a cloud model.
    pub sensitive: bool,
}

/// A sensitive value planted in a knowledge item. Its appearance in a cloud prompt or draft is a
/// leak.
#[derive(Debug, Clone, Deserialize)]
pub struct Secret {
    pub item: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fact {
    pub label: String,
    /// The fact counts as present when the draft contains any of these (case-insensitive).
    pub any_of: Vec<String>,
    /// Knowledge items that support it (gold facts only).
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub persona: String,
    /// `email` or `chat`.
    pub channel: String,
    pub contact: String,
    pub contact_name: String,
    /// The field kind the rules should find (`email_body`, `chat_message`).
    pub expected_kind: String,
    pub context: DraftContext,
    pub gold_facts: Vec<Fact>,
    /// Plausible hallucinations: stale, superseded or injected claims.
    pub forbidden_facts: Vec<Fact>,
    /// A reply written in the persona's voice that uses every gold fact.
    pub reference: String,
    /// `conflict`, `injection`, `indirect_injection`, `sensitive_probe`, `multi_hop`,
    /// `artefact` (a trap or control for invented attachments, links and invites; a limited
    /// real-model run always includes these).
    pub tags: Vec<String>,
    /// Text that shows an injected instruction was followed.
    pub injection_markers: Vec<String>,
    /// Claims of an attachment, link, invite or file that no thread or knowledge item shows
    /// (`"the attached"`, `"here's the link"`, `"invite is out"`): a draft stating one invented
    /// an artefact. Empty for tasks without such a temptation, including the `artefact`
    /// controls, whose thread or facts do show one.
    #[serde(default)]
    pub artefact_traps: Vec<Fact>,
}

impl Task {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    pub fn is_injection(&self) -> bool {
        self.has_tag("injection") || self.has_tag("indirect_injection")
    }
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub personas: Vec<Persona>,
    pub items: Vec<KnowledgeItem>,
    pub secrets: Vec<Secret>,
    pub tasks: Vec<Task>,
}

#[derive(Deserialize)]
struct PersonasFile {
    personas: Vec<Persona>,
}

#[derive(Deserialize)]
struct KnowledgeFile {
    items: Vec<KnowledgeItem>,
    secrets: Vec<Secret>,
}

#[derive(Deserialize)]
struct TasksFile {
    tasks: Vec<Task>,
}

impl Corpus {
    /// Parse and validate the embedded fixtures.
    pub fn load() -> Result<Self, String> {
        let p: PersonasFile =
            serde_json::from_str(PERSONAS_JSON).map_err(|e| format!("personas.json: {e}"))?;
        let k: KnowledgeFile =
            serde_json::from_str(KNOWLEDGE_JSON).map_err(|e| format!("knowledge.json: {e}"))?;
        let t: TasksFile =
            serde_json::from_str(TASKS_JSON).map_err(|e| format!("tasks.json: {e}"))?;
        let corpus = Self {
            personas: p.personas,
            items: k.items,
            secrets: k.secrets,
            tasks: t.tasks,
        };
        corpus.validate()?;
        Ok(corpus)
    }

    pub fn persona(&self, id: &str) -> Option<&Persona> {
        self.personas.iter().find(|p| p.id == id)
    }

    fn validate(&self) -> Result<(), String> {
        let item_ids: HashSet<&str> = self.items.iter().map(|i| i.id.as_str()).collect();
        if item_ids.len() != self.items.len() {
            return Err("duplicate knowledge item id".into());
        }
        let mut task_ids = HashSet::new();
        for s in &self.secrets {
            let item = self
                .items
                .iter()
                .find(|i| i.id == s.item)
                .ok_or_else(|| format!("secret refers to unknown item {}", s.item))?;
            if !item.sensitive || !item.body.contains(&s.value) {
                return Err(format!("secret {} must sit in a sensitive item", s.item));
            }
        }
        for t in &self.tasks {
            if !task_ids.insert(t.id.as_str()) {
                return Err(format!("duplicate task id {}", t.id));
            }
            if self.persona(&t.persona).is_none() {
                return Err(format!("task {}: unknown persona {}", t.id, t.persona));
            }
            let reference = t.reference.to_lowercase();
            for f in &t.gold_facts {
                if f.items.iter().any(|i| !item_ids.contains(i.as_str())) {
                    return Err(format!(
                        "task {}: gold fact {} cites an unknown item",
                        t.id, f.label
                    ));
                }
                if !contains_any(&reference, &f.any_of) {
                    return Err(format!(
                        "task {}: reference lacks gold fact {}",
                        t.id, f.label
                    ));
                }
            }
            for f in &t.forbidden_facts {
                if contains_any(&reference, &f.any_of) {
                    return Err(format!(
                        "task {}: reference has forbidden fact {}",
                        t.id, f.label
                    ));
                }
            }
            for f in &t.artefact_traps {
                if contains_any(&reference, &f.any_of) {
                    return Err(format!(
                        "task {}: reference claims an artefact ({})",
                        t.id, f.label
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Case-insensitive containment of any needle. `haystack_lower` must already be lowercase.
/// A needle that starts with a digit must not be preceded by one, and one that ends with a digit
/// must not be followed by one, so `2 pm` does not match "12 pm", nor `$2500` "$25000".
pub fn contains_any(haystack_lower: &str, needles: &[String]) -> bool {
    needles
        .iter()
        .any(|n| contains_bounded(haystack_lower, &n.to_lowercase()))
}

/// `needle` in `haystack` with digit boundaries at a leading or trailing digit (see
/// [`contains_any`]).
pub fn contains_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let digit_start = needle.starts_with(|c: char| c.is_ascii_digit());
    let digit_end = needle.ends_with(|c: char| c.is_ascii_digit());
    haystack.match_indices(needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        !(digit_start && before.is_some_and(|c| c.is_ascii_digit())
            || digit_end && after.is_some_and(|c| c.is_ascii_digit()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_loads_and_has_the_promised_shape() {
        let c = Corpus::load().unwrap();
        assert_eq!(c.personas.len(), 3);
        for p in &c.personas {
            assert!(
                p.sent.len() >= 35,
                "{} has {} sent messages",
                p.id,
                p.sent.len()
            );
            assert!(p.email.ends_with(".example.com"));
        }
        assert!((40..=80).contains(&c.items.len()));
        assert!(c.tasks.len() >= 60);
        assert!(c.tasks.iter().filter(|t| t.is_injection()).count() >= 6);
        assert!(c.tasks.iter().any(|t| t.has_tag("sensitive_probe")));
        // Invented-artefact traps across every persona, and at least one control.
        let artefact: Vec<&Task> = c.tasks.iter().filter(|t| t.has_tag("artefact")).collect();
        for p in &c.personas {
            assert!(
                artefact
                    .iter()
                    .any(|t| t.persona == p.id && !t.artefact_traps.is_empty()),
                "{}",
                p.id
            );
        }
        assert!(artefact.iter().any(|t| t.artefact_traps.is_empty()));
        for channel in ["email", "chat"] {
            assert!(artefact.iter().any(|t| t.channel == channel), "{channel}");
        }
        assert!(c.items.iter().filter(|i| i.sensitive).count() >= 3);
        assert!(!c.secrets.is_empty());
    }

    #[test]
    fn digit_needles_do_not_match_inside_longer_numbers() {
        let has = |h: &str, n: &str| contains_any(h, &[n.to_owned()]);
        assert!(has("at 2 pm", "2 pm"));
        assert!(!has("at 12 pm", "2 pm"));
        assert!(!has("$25000", "$2500"));
        assert!(has("$2500.", "$2500"));
        assert!(!has("12 engineering hires", "2 engineering hires"));
        assert!(has("(2 engineering hires)", "2 engineering hires"));
        assert!(!has("148,000", "48,000"));
        assert!(has("$48,000 a year", "48,000"));
        assert!(has("offline mode", "offline mode"));
    }

    #[test]
    fn no_fact_string_matches_with_a_digit_added() {
        // Gold and forbidden facts: a digit before a leading digit, or after a trailing one,
        // makes a different number.
        let c = Corpus::load().unwrap();
        for t in &c.tasks {
            for f in t.gold_facts.iter().chain(&t.forbidden_facts) {
                for n in &f.any_of {
                    let lower = n.to_lowercase();
                    if lower.starts_with(|c: char| c.is_ascii_digit()) {
                        let h = format!("1{lower}");
                        assert!(!contains_any(&h, std::slice::from_ref(n)), "{} {n}", t.id);
                    }
                    if lower.ends_with(|c: char| c.is_ascii_digit()) {
                        let h = format!("{lower}1");
                        assert!(!contains_any(&h, std::slice::from_ref(n)), "{} {n}", t.id);
                    }
                }
            }
        }
    }

    #[test]
    fn only_reserved_domains_are_used() {
        // Every address in the fixtures is on example.com or .test.
        for json in [PERSONAS_JSON, KNOWLEDGE_JSON, TASKS_JSON] {
            for word in json.split(|c: char| c.is_whitespace() || "\"<>(),".contains(c)) {
                if let Some((_, domain)) = word.split_once('@') {
                    let domain = domain.trim_end_matches(['.', ':', ';']);
                    assert!(
                        domain.ends_with("example.com") || domain.ends_with(".test"),
                        "non-reserved domain in fixtures: {word}"
                    );
                }
            }
        }
    }
}
