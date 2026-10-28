//! Where the policy configuration lives.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use nugget_core::Result;

use crate::rules::PolicyConfig;

/// Loads and saves the user-editable [`PolicyConfig`].
pub trait PolicyStore: Send + Sync {
    /// `None` when nothing was saved yet.
    fn load(&self) -> Result<Option<PolicyConfig>>;
    fn save(&self, config: &PolicyConfig) -> Result<()>;
}

/// Keeps the configuration in memory (tests, ephemeral sessions).
#[derive(Debug, Default)]
pub struct MemoryPolicyStore {
    config: Mutex<Option<PolicyConfig>>,
}

impl MemoryPolicyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PolicyStore for MemoryPolicyStore {
    fn load(&self) -> Result<Option<PolicyConfig>> {
        Ok(self
            .config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone())
    }

    fn save(&self, config: &PolicyConfig) -> Result<()> {
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = Some(config.clone());
        Ok(())
    }
}

/// A pretty-printed JSON file, replaced atomically (write to a sibling temp file, then rename).
#[derive(Debug, Clone)]
pub struct JsonFilePolicyStore {
    path: PathBuf,
}

impl JsonFilePolicyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl PolicyStore for JsonFilePolicyStore {
    fn load(&self) -> Result<Option<PolicyConfig>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, config: &PolicyConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut bytes = serde_json::to_vec_pretty(config)?;
        bytes.push(b'\n');
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Effect, Rule};
    use crate::{ActionDescriptor, Decision, KillSwitch, PolicyEngine, PolicyGate, RiskClass};
    use std::sync::Arc;

    fn sample() -> PolicyConfig {
        let mut c = PolicyConfig::default();
        c.rules.push(
            Rule::new("slack-send", Effect::Allow)
                .targets(["*slack*"])
                .risks([RiskClass::Send]),
        );
        c.rate_limits.insert(RiskClass::Send, 7);
        c.confirm_timeout_secs = 30;
        c
    }

    #[test]
    fn memory_round_trip() {
        let s = MemoryPolicyStore::new();
        assert_eq!(s.load().unwrap(), None);
        s.save(&sample()).unwrap();
        assert_eq!(s.load().unwrap(), Some(sample()));
    }

    #[test]
    fn json_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = JsonFilePolicyStore::new(dir.path().join("nested/policy.json"));
        assert_eq!(s.load().unwrap(), None);
        s.save(&sample()).unwrap();
        assert_eq!(s.load().unwrap(), Some(sample()));
        let mut changed = sample();
        changed.rules.clear();
        s.save(&changed).unwrap();
        assert_eq!(s.load().unwrap(), Some(changed));
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(JsonFilePolicyStore::new(&path).load().is_err());
    }

    #[test]
    fn engine_persists_through_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        let send = ActionDescriptor {
            target_app: "com.tinyspeck.slackmacgap".into(),
            verb: "click".into(),
            target_label: Some("Send".into()),
            text_preview: None,
            declared_risk: RiskClass::Send,
            task_id: "t".into(),
        };
        {
            let store = Arc::new(JsonFilePolicyStore::new(&path));
            let e = PolicyEngine::load(
                store,
                PolicyEngine::builder().kill_switch(KillSwitch::new()),
            )
            .unwrap();
            assert!(matches!(e.evaluate(&send), Decision::Confirm(_)));
            e.upsert_rule(
                Rule::new("slack-send", Effect::Allow)
                    .targets(["*slack*"])
                    .risks([RiskClass::Send]),
            )
            .unwrap();
        }
        let store = Arc::new(JsonFilePolicyStore::new(&path));
        let e = PolicyEngine::load(
            store,
            PolicyEngine::builder().kill_switch(KillSwitch::new()),
        )
        .unwrap();
        assert_eq!(e.config().rules.len(), 1);
        assert_eq!(e.evaluate(&send), Decision::Allow);
    }
}
