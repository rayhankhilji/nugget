//! Rolling capture of a child's stdout/stderr, with secret redaction.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::io::{AsyncRead, AsyncReadExt};

/// Longest line kept; longer output is split.
const MAX_LINE: usize = 8 * 1024;
/// Lines kept in memory for error messages.
const TAIL_LINES: usize = 40;
const REDACTED: &str = "<redacted>";

/// A size-capped log file rotated as `<name>`, `<name>.1`, ... `<name>.<keep>`.
pub(crate) struct RollingLog {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
    secrets: Vec<String>,
    tail: VecDeque<String>,
}

pub(crate) type SharedLog = Arc<Mutex<RollingLog>>;

pub(crate) fn lock(log: &SharedLog) -> MutexGuard<'_, RollingLog> {
    log.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl RollingLog {
    pub(crate) fn open(path: PathBuf, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = open_append(&path)?;
        let size = file.metadata().map_or(0, |m| m.len());
        Ok(Self {
            path,
            max_bytes: max_bytes.max(4096),
            keep: keep.max(1),
            file: Some(file),
            size,
            secrets: Vec::new(),
            tail: VecDeque::new(),
        })
    }

    /// Never write `secret` (an API key) to the file: it is replaced with `<redacted>`. The most recent few are kept, so output of a
    /// server that is still shutting down stays redacted too.
    pub(crate) fn add_secret(&mut self, secret: &str) {
        const KEEP: usize = 4;
        if secret.is_empty() || self.secrets.iter().any(|s| s == secret) {
            return;
        }
        self.secrets.push(secret.to_string());
        if self.secrets.len() > KEEP {
            self.secrets.remove(0);
        }
    }

    /// Append one line, tagged with its stream. Errors are swallowed: logging must never take the
    /// server down.
    pub(crate) fn line(&mut self, stream: &str, text: &str) {
        let mut text = text.trim_end_matches(['\r', '\n']).to_string();
        for s in &self.secrets {
            if text.contains(s.as_str()) {
                text = text.replace(s.as_str(), REDACTED);
            }
        }
        let ts = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let entry = format!("{ts} [{stream}] {text}\n");
        if self.size + entry.len() as u64 > self.max_bytes {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut()
            && f.write_all(entry.as_bytes()).is_ok()
        {
            self.size += entry.len() as u64;
        }
        if self.tail.len() == TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(text);
    }

    /// A marker line from the supervisor itself.
    pub(crate) fn note(&mut self, text: &str) {
        self.line("nugget", text);
    }

    /// The last lines written (already redacted), oldest first.
    pub(crate) fn tail(&self, n: usize) -> Vec<String> {
        self.tail
            .iter()
            .skip(self.tail.len().saturating_sub(n))
            .cloned()
            .collect()
    }

    fn rotate(&mut self) {
        self.file = None;
        let numbered = |i: usize| {
            let mut s = self.path.as_os_str().to_owned();
            s.push(format!(".{i}"));
            PathBuf::from(s)
        };
        let _ = std::fs::remove_file(numbered(self.keep));
        for i in (1..self.keep).rev() {
            let _ = std::fs::rename(numbered(i), numbered(i + 1));
        }
        let _ = std::fs::rename(&self.path, numbered(1));
        self.file = open_append(&self.path).ok();
        self.size = 0;
    }
}

fn open_append(path: &Path) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Copy `pipe` into `log` line by line until EOF. Lines are bounded; invalid UTF-8 is replaced.
pub(crate) async fn pump(mut pipe: impl AsyncRead + Unpin, stream: &'static str, log: SharedLog) {
    let mut buf = vec![0u8; 4096];
    let mut line: Vec<u8> = Vec::new();
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' || line.len() >= MAX_LINE {
                lock(&log).line(stream, &String::from_utf8_lossy(&line));
                line.clear();
                if b == b'\n' {
                    continue;
                }
            }
            line.push(b);
        }
    }
    if !line.is_empty() {
        lock(&log).line(stream, &String::from_utf8_lossy(&line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_redacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/x.log");
        let mut log = RollingLog::open(path.clone(), 4096, 2).unwrap();
        log.add_secret("s3cr3t-key");
        for i in 0..400 {
            log.line("stderr", &format!("line {i} key=s3cr3t-key"));
        }
        let all: String = ["", ".1", ".2"]
            .iter()
            .map(|s| std::fs::read_to_string(format!("{}{s}", path.display())).unwrap())
            .collect();
        assert!(!all.contains("s3cr3t-key"));
        assert!(all.contains("key=<redacted>"));
        assert!(!std::path::Path::new(&format!("{}.3", path.display())).exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 4096);
        assert_eq!(log.tail(2).len(), 2);
        assert!(log.tail(1)[0].starts_with("line 399"));
    }

    #[tokio::test]
    async fn pump_splits_lines_and_bounds_them() {
        let dir = tempfile::tempdir().unwrap();
        let log: SharedLog = Arc::new(Mutex::new(
            RollingLog::open(dir.path().join("p.log"), 1 << 20, 1).unwrap(),
        ));
        let mut input = b"one\ntwo\xff\n".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_LINE + 10));
        pump(&input[..], "stdout", log.clone()).await;
        let tail = lock(&log).tail(10);
        assert_eq!(tail[0], "one");
        assert_eq!(tail[1], "two\u{fffd}");
        assert_eq!(tail[2].len(), MAX_LINE);
        assert_eq!(tail[3].len(), 10);
    }
}
