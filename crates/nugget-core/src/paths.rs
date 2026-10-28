use std::path::PathBuf;

use crate::{Error, Result};

/// Per-user directories. Override the root with `NUGGET_HOME` (tests, portable installs).
#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(home) = std::env::var_os("NUGGET_HOME") {
            return Ok(Self::rooted(PathBuf::from(home)));
        }
        let dirs = directories::ProjectDirs::from("dev", "Nugget", "Nugget")
            .ok_or_else(|| Error::Other("cannot resolve home directory".into()))?;
        Ok(Self {
            data: dirs.data_local_dir().to_path_buf(),
            config: dirs.config_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
        })
    }

    pub fn rooted(root: PathBuf) -> Self {
        Self {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
        }
    }

    pub fn db_file(&self) -> PathBuf {
        self.data.join("nugget.db")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.data.join("models")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }
}
