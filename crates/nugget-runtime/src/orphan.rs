//! Pid files for running servers, so a server orphaned by a crashed daemon is cleaned up by the
//! next one.
//!
//! On Windows the job object already kills servers when the daemon dies. On Unix a SIGKILLed
//! daemon cannot run destructors and a portable parent-death signal needs `unsafe`, so the next
//! [`crate::Runtime::new`] kills any process still listed here, **only if** its executable path
//! is exactly the one recorded (a recycled pid running something else is left alone).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PidRecord {
    pub pid: u32,
    pub exe: PathBuf,
    pub port: u16,
}

pub(crate) fn write(path: &Path, rec: &PidRecord) {
    let res = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("pid.tmp");
        std::fs::write(&tmp, serde_json::to_vec(rec)?)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = res {
        tracing::warn!(path = %path.display(), error = %e, "cannot write runtime pid file");
    }
}

pub(crate) fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Kill servers recorded in `dir/*.pid` that are still running the recorded executable.
/// Returns how many were killed.
pub(crate) fn reap(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut killed = 0;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("pid") {
            continue;
        }
        let rec: Option<PidRecord> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(rec) = rec
            && kill_if_ours(&rec)
        {
            tracing::warn!(
                pid = rec.pid,
                port = rec.port,
                "killed orphaned llama-server"
            );
            killed += 1;
        }
        remove(&path);
    }
    killed
}

fn kill_if_ours(rec: &PidRecord) -> bool {
    let pid = Pid::from_u32(rec.pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let Some(p) = sys.process(pid) else {
        return false;
    };
    if matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead) {
        return false;
    }
    let same = match (p.exe(), rec.exe.canonicalize()) {
        (Some(exe), Ok(want)) => exe == want || exe == rec.exe,
        (Some(exe), Err(_)) => exe == rec.exe,
        (None, _) => false,
    };
    same && p.kill()
}
