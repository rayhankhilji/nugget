//! Spawning `llama-server` in its own process group (Unix) or kill-on-close job object (Windows).

use std::ffi::OsString;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nugget_core::{Error, Result};
use nugget_infer::Secret;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};

use crate::logs::{SharedLog, pump};

/// Environment variable llama-server reads its API key from (keeps it out of `ps` output).
pub(crate) const API_KEY_ENV: &str = "LLAMA_API_KEY";

/// Variables passed through to the child; everything else (notably every `LLAMA_ARG_*` and
/// `HF_*` setting of the parent) is dropped so the environment cannot override our arguments.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "LC_ALL",
];

/// A running server process. Dropping it kills the whole process group / job.
pub(crate) struct ServerProcess {
    child: Box<dyn ChildWrapper>,
    pid: Option<u32>,
    pub(crate) started: Instant,
}

impl std::fmt::Debug for ServerProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerProcess")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl ServerProcess {
    pub(crate) fn spawn(
        binary: &Path,
        args: &[OsString],
        api_key: &Secret,
        log: &SharedLog,
    ) -> Result<Self> {
        let mut cmd = CommandWrap::with_new(binary, |c| {
            c.args(args)
                .env_clear()
                .envs(
                    ENV_ALLOWLIST
                        .iter()
                        .filter_map(|k| std::env::var_os(k).map(|v| (*k, v))),
                )
                .env(API_KEY_ENV, api_key.expose())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(dir) = binary.parent() {
                c.current_dir(dir);
            }
        });
        #[cfg(unix)]
        cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::CREATE_NO_WINDOW;
            cmd.wrap(process_wrap::tokio::CreationFlags(CREATE_NO_WINDOW));
            cmd.wrap(process_wrap::tokio::JobObject);
        }
        // With JobObject this also sets JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: if this process dies,
        // Windows kills the server. On Unix it is tokio's kill-on-drop of the leader; the group
        // kill is done by our own Drop below.
        cmd.wrap(KillOnDrop);
        let mut child = cmd.spawn().map_err(|e| {
            Error::Other(format!(
                "cannot start {}: {e}",
                binary.file_name().unwrap_or_default().to_string_lossy()
            ))
        })?;
        let pid = child.id();
        if let Some(out) = child.stdout().take() {
            tokio::spawn(pump(out, "stdout", log.clone()));
        }
        if let Some(err) = child.stderr().take() {
            tokio::spawn(pump(err, "stderr", log.clone()));
        }
        Ok(Self {
            child,
            pid,
            started: Instant::now(),
        })
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// `Some(status)` if it has exited.
    pub(crate) fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// After the main process exited on its own: kill anything it left behind in its group/job.
    pub(crate) fn kill_stragglers(&mut self) {
        let _ = self.child.start_kill();
    }

    /// Ask nicely (SIGTERM to the group on Unix), then kill after `grace`.
    pub(crate) async fn shutdown(&mut self, grace: Duration) {
        #[cfg(unix)]
        {
            const SIGTERM: i32 = 15;
            if self.child.signal(SIGTERM).is_ok()
                && tokio::time::timeout(grace, self.child.wait()).await.is_ok()
            {
                return;
            }
        }
        #[cfg(not(unix))]
        let _ = grace;
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        // Unix: SIGKILL to the process group. Windows: terminate the job.
        if self.try_wait().is_none() {
            let _ = self.child.start_kill();
        }
    }
}
