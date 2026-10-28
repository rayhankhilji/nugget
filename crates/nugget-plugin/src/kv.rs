//! The plugin key-value store: one small JSON file per plugin namespace, size-capped by the
//! manifest's `kv_max_bytes`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use nugget_core::{Error, Result};

use crate::store::write_atomic;

/// Longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 256;

/// One namespace (a plugin's tools, or one source of a plugin connector).
#[derive(Debug)]
pub(crate) struct KvNamespace {
    path: PathBuf,
    max_bytes: u64,
    data: Mutex<Option<BTreeMap<String, Vec<u8>>>>,
}

impl KvNamespace {
    pub(crate) fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            data: Mutex::new(None),
        }
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.with(|m| Ok(m.get(key).cloned()))
    }

    pub(crate) fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        self.with(|m| {
            Ok(m.range(prefix.to_string()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, _)| k.clone())
                .collect())
        })
    }

    pub(crate) fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        check_key(key)?;
        let max = self.max_bytes;
        self.with_mut(|m| {
            let used: u64 = m
                .iter()
                .filter(|(k, _)| k.as_str() != key)
                .map(|(k, v)| (k.len() + v.len()) as u64)
                .sum();
            let after = used + (key.len() + value.len()) as u64;
            if after > max {
                return Err(Error::Invalid(format!(
                    "key-value store is full ({after} bytes > the declared {max})"
                )));
            }
            m.insert(key.to_string(), value.to_vec());
            Ok(())
        })
    }

    pub(crate) fn delete(&self, key: &str) -> Result<()> {
        self.with_mut(|m| {
            m.remove(key);
            Ok(())
        })
    }

    fn with<R>(&self, f: impl FnOnce(&BTreeMap<String, Vec<u8>>) -> Result<R>) -> Result<R> {
        let mut guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        f(self.loaded(&mut guard)?)
    }

    fn with_mut(&self, f: impl FnOnce(&mut BTreeMap<String, Vec<u8>>) -> Result<()>) -> Result<()> {
        let mut guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.loaded(&mut guard)?;
        let before = map.clone();
        f(map)?;
        let encoded: BTreeMap<&String, String> =
            map.iter().map(|(k, v)| (k, hex::encode(v))).collect();
        if let Err(e) = write_atomic(&self.path, &serde_json::to_vec(&encoded)?) {
            *map = before;
            return Err(e);
        }
        Ok(())
    }

    fn loaded<'a>(
        &self,
        slot: &'a mut Option<BTreeMap<String, Vec<u8>>>,
    ) -> Result<&'a mut BTreeMap<String, Vec<u8>>> {
        if slot.is_none() {
            let map = match std::fs::read(&self.path) {
                Ok(bytes) => {
                    let encoded: BTreeMap<String, String> = serde_json::from_slice(&bytes)
                        .map_err(|e| Error::Storage(format!("plugin kv is corrupt: {e}")))?;
                    encoded
                        .into_iter()
                        .map(|(k, v)| {
                            hex::decode(v)
                                .map(|v| (k, v))
                                .map_err(|e| Error::Storage(format!("plugin kv is corrupt: {e}")))
                        })
                        .collect::<Result<_>>()?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            };
            *slot = Some(map);
        }
        Ok(slot.as_mut().expect("loaded above"))
    }
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(Error::Invalid(format!(
            "kv keys must be 1-{MAX_KEY_BYTES} bytes"
        )));
    }
    Ok(())
}
