//! A reusable meter: CPU time and wakeups of a set of processes and their descendants, and system
//! (package) energy, over a window.
//!
//! ```ignore
//! let meter = Meter::start(&[child.id()]);
//! // ... the work being measured ...
//! let reading = meter.stop();
//! let base = baseline(Duration::from_secs_f64(reading.wall_s)); // or measured before
//! let joules = reading.attributable_j(&base);
//! ```
//!
//! Several groups at once (`Meter::start_groups(&[&[shell], &[daemon]])` … `stop_groups()`, one
//! reading per group) share one sampler and, on macOS, one `powermetrics`.
//!
//! - **Processes.** Each pid passed to [`Meter::start`] counts with every descendant (found by
//!   walking the parent pids of all processes on each sample: the desktop shell's webview runs
//!   in child processes). A background thread samples every [`SAMPLE_INTERVAL`] and keeps the
//!   last value seen for every thread (Linux) or process (macOS), so processes and threads that
//!   exit mid-window still count up to their last sample; ones that start and exit between two
//!   samples are missed. Linux reads `/proc/<pid>/task/<tid>/schedstat` (CPU, ns) and the
//!   voluntary + involuntary context switches in `.../status` (wakeups), as `perf_daemon` does.
//!   macOS takes CPU time and wakeups (idle + interrupt) from `powermetrics`' tasks sampler when
//!   it runs, else CPU time from `ps` (no wakeups). Other platforms report no process figures.
//! - **System energy** ([`EnergySource`]). Linux: RAPL package energy, the sum of the top-level
//!   `package-*` zones in `/sys/class/powercap/intel-rapl:*/energy_uj`, read on every sample so
//!   counter wraparound (`max_energy_range_uj`) is handled; the files are root-only on most
//!   kernels since 2020 and absent on most VMs, and then there is no energy figure. macOS:
//!   `sudo -n powermetrics --samplers cpu_power,tasks --show-process-energy -f plist`, run as a
//!   child for the window (needs passwordless sudo); package energy comes from each sample's
//!   `processor` dict (energy in mJ or power in mW, whichever the machine reports; virtual Macs
//!   may report none) and a per-process energy impact from its tasks. Windows: none. Missing
//!   sources never fail a run: the reading says which source it used and why there was none.
//! - **Attribution.** Package energy is the whole machine's. [`baseline`] meters an equal
//!   window with nothing launched; `attributable = window − baseline` (as mean watts, so the two
//!   windows need not be exactly equal). On a shared VM or a busy laptop this difference is
//!   noisy and can even be negative; the process figures are not affected.
//!
//! The sampler is a thread of the calling process: when the process metered is the caller
//! itself (`a11y_poll`), compare against a window metered the same way, not against zero.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the background sampler reads process counters and RAPL.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// The RAPL tree on Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const POWERCAP: &str = "/sys/class/powercap";

/// Where a [`Reading`]'s system energy came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EnergySource {
    /// Linux RAPL package counters.
    Rapl,
    /// macOS `powermetrics` (`cpu_power` sampler).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Powermetrics,
    /// No system energy figure on this machine.
    #[default]
    None,
}

impl EnergySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Rapl => "rapl",
            Self::Powermetrics => "powermetrics",
            Self::None => "none",
        }
    }
}

/// What a [`Meter`] measured over its window. `None` where the platform gave nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Reading {
    /// Wall-clock length of the window.
    pub(crate) wall_s: f64,
    /// CPU time (user + system) of the metered processes and their descendants.
    pub(crate) cpu_s: Option<f64>,
    /// Wakeups of the metered processes: context switches (Linux) or idle + interrupt wakeups
    /// (macOS `powermetrics`).
    pub(crate) wakeups: Option<f64>,
    /// Distinct processes counted (the pids given and every descendant seen).
    pub(crate) processes: usize,
    /// Where the process figures came from: `proc`, `powermetrics`, `ps` or `none`.
    pub(crate) process_source: &'static str,
    /// System package energy over the window, joules.
    pub(crate) package_j: Option<f64>,
    /// Mean system package power over the window, watts.
    pub(crate) package_w: Option<f64>,
    /// macOS only: mean energy impact per second of the metered processes (`powermetrics`'
    /// unitless score, the one Activity Monitor shows).
    pub(crate) energy_impact: Option<f64>,
    pub(crate) energy_source: EnergySource,
    /// Why there is no energy figure (or what limited it).
    pub(crate) energy_note: Option<String>,
}

impl Reading {
    /// CPU time as a share of one core over the window, percent.
    pub(crate) fn cpu_percent(&self) -> Option<f64> {
        self.cpu_s.map(|c| c / self.wall_s.max(1e-9) * 100.0)
    }

    pub(crate) fn wakeups_per_s(&self) -> Option<f64> {
        self.wakeups.map(|w| w / self.wall_s.max(1e-9))
    }

    /// Mean package power above `base` (a [`baseline`] window), watts.
    pub(crate) fn attributable_w(&self, base: &Reading) -> Option<f64> {
        Some(self.package_w? - base.package_w?)
    }

    /// Package energy above `base` over this window: [`Self::attributable_w`] times this
    /// window's length, joules.
    pub(crate) fn attributable_j(&self, base: &Reading) -> Option<f64> {
        self.attributable_w(base).map(|w| w * self.wall_s)
    }
}

/// Which system energy source a [`Meter`] would use on this machine, and why there is none when
/// there is none. Cached after the first call (on macOS it runs `powermetrics` once).
pub(crate) fn energy_source() -> (EnergySource, Option<String>) {
    #[cfg(target_os = "linux")]
    {
        match Rapl::open(Path::new(POWERCAP)) {
            Ok(_) => (EnergySource::Rapl, None),
            Err(why) => (EnergySource::None, Some(why)),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let p = pm::probe();
        if p.energy {
            (EnergySource::Powermetrics, None)
        } else {
            (EnergySource::None, Some(p.note.clone()))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        (
            EnergySource::None,
            Some("no energy meter on this platform (CPU time and wakeups only)".into()),
        )
    }
}

/// Meter `window` with nothing launched: the reference for [`Reading::attributable_w`].
/// Blocks the calling thread for `window`.
pub(crate) fn baseline(window: Duration) -> Reading {
    let m = Meter::start(&[]);
    std::thread::sleep(window);
    m.stop()
}

/// A running measurement. Dropping it without [`stop`](Self::stop) stops its sampler.
pub(crate) struct Meter {
    started: Instant,
    state: Arc<Mutex<State>>,
    stop: Option<mpsc::Sender<()>>,
    sampler: Option<JoinHandle<()>>,
    #[cfg(target_os = "macos")]
    powermetrics: Option<pm::Powermetrics>,
}

impl Meter {
    /// Start metering `pids` (and their descendants) and system energy. An empty slice meters
    /// system energy alone.
    pub(crate) fn start(pids: &[u32]) -> Meter {
        Self::start_groups(&[pids])
    }

    /// Start metering several groups of processes at once, each pid with its descendants, read
    /// back separately by [`stop_groups`](Self::stop_groups) (one reading per group, all with
    /// the same system energy). One sampler and, on macOS, one `powermetrics` serve them all.
    pub(crate) fn start_groups(groups: &[&[u32]]) -> Meter {
        let mut state = State {
            groups: groups
                .iter()
                .map(|g| Group {
                    roots: g.to_vec(),
                    counters: Counters::default(),
                })
                .collect(),
            rapl: None,
            energy_note: None,
        };
        #[cfg(target_os = "linux")]
        match Rapl::open(Path::new(POWERCAP)) {
            Ok(r) => state.rapl = Some(r),
            Err(why) => state.energy_note = Some(why),
        }
        #[cfg(target_os = "macos")]
        let powermetrics = {
            let probe = pm::probe();
            if !probe.energy {
                state.energy_note = Some(probe.note.clone());
            }
            if probe.runs {
                pm::Powermetrics::spawn(SAMPLE_INTERVAL)
            } else {
                None
            }
        };
        state.tick(true);
        let state = Arc::new(Mutex::new(state));
        let (tx, rx) = mpsc::channel::<()>();
        let shared = state.clone();
        let sampler = std::thread::Builder::new()
            .name("energy-meter".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(SAMPLE_INTERVAL) {
                    lock(&shared).tick(false);
                }
            })
            .ok();
        Meter {
            started: Instant::now(),
            state,
            stop: Some(tx),
            sampler,
            #[cfg(target_os = "macos")]
            powermetrics,
        }
    }

    /// Stop and read the window.
    pub(crate) fn stop(self) -> Reading {
        self.stop_groups().swap_remove(0)
    }

    /// Stop and read the window of every group, in the order given to
    /// [`start_groups`](Self::start_groups).
    pub(crate) fn stop_groups(mut self) -> Vec<Reading> {
        let wall_s = self.started.elapsed().as_secs_f64().max(1e-9);
        self.halt();
        let mut state = lock(&self.state);
        state.tick(false);
        #[cfg(target_os = "macos")]
        let samples = self.powermetrics.take().map(|p| p.finish());
        let mut out = Vec::new();
        for g in &state.groups {
            let mut r = Reading {
                wall_s,
                processes: g.counters.pids.len(),
                energy_note: state.energy_note.clone(),
                process_source: "none",
                ..Reading::default()
            };
            if g.counters.seen {
                let (cpu, switches) = g.counters.totals();
                r.cpu_s = Some(cpu);
                r.wakeups = switches.map(|s| s as f64);
                r.process_source = if cfg!(target_os = "linux") {
                    "proc"
                } else {
                    "ps"
                };
            }
            if let Some(rapl) = &state.rapl {
                let j = rapl.total_uj as f64 / 1e6;
                r.package_j = Some(j);
                r.package_w = Some(j / wall_s);
                r.energy_source = EnergySource::Rapl;
            }
            #[cfg(target_os = "macos")]
            if let Some(samples) = &samples {
                let pids = if g.counters.pids.is_empty() {
                    g.roots.iter().copied().collect()
                } else {
                    g.counters.pids.clone()
                };
                pm::apply(&mut r, samples, &pids, !g.roots.is_empty());
            }
            out.push(r);
        }
        if out.is_empty() {
            out.push(Reading {
                wall_s,
                process_source: "none",
                ..Reading::default()
            });
        }
        out
    }

    fn halt(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.sampler.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.halt();
        #[cfg(target_os = "macos")]
        if let Some(p) = self.powermetrics.take() {
            p.finish();
        }
    }
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What the sampler thread updates.
struct State {
    groups: Vec<Group>,
    rapl: Option<Rapl>,
    energy_note: Option<String>,
}

/// One group of metered processes.
struct Group {
    roots: Vec<u32>,
    counters: Counters,
}

impl State {
    fn tick(&mut self, initial: bool) {
        for g in self.groups.iter_mut().filter(|g| !g.roots.is_empty()) {
            let obs = sample_procs(&g.roots);
            g.counters.observe(&obs, initial);
        }
        if let Some(r) = self.rapl.as_mut() {
            r.tick();
        }
    }
}

/// One reading of one thread (Linux) or process (macOS): cumulative CPU time and context
/// switches since it started.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Obs {
    /// Thread id on Linux, pid on macOS.
    key: u32,
    pid: u32,
    cpu_s: f64,
    switches: Option<u64>,
}

/// First and last value seen per key; the window's total is the sum of the differences.
#[derive(Debug, Default)]
struct Counters {
    first: HashMap<u32, (f64, u64)>,
    last: HashMap<u32, (f64, u64)>,
    pids: HashSet<u32>,
    switches: bool,
    seen: bool,
}

impl Counters {
    /// Record a sample. A key first seen after the initial sample started inside the window,
    /// so all of its counters count.
    fn observe(&mut self, obs: &[Obs], initial: bool) {
        for o in obs {
            let v = (o.cpu_s, o.switches.unwrap_or(0));
            self.first
                .entry(o.key)
                .or_insert(if initial { v } else { (0.0, 0) });
            self.last.insert(o.key, v);
            self.pids.insert(o.pid);
            self.switches |= o.switches.is_some();
            self.seen = true;
        }
    }

    /// CPU seconds and context switches over the window.
    fn totals(&self) -> (f64, Option<u64>) {
        let (mut cpu, mut sw) = (0.0, 0u64);
        for (k, (c1, s1)) in &self.last {
            let (c0, s0) = self.first.get(k).copied().unwrap_or((0.0, 0));
            cpu += (c1 - c0).max(0.0);
            sw += s1.saturating_sub(s0);
        }
        (cpu, self.switches.then_some(sw))
    }
}

/// `roots` and all their descendants in a `(pid, ppid)` table; roots not in the table (exited)
/// are left out.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn tree(roots: &[u32], table: &[(u32, u32)]) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let alive: HashSet<u32> = table.iter().map(|(p, _)| *p).collect();
    for &(pid, ppid) in table {
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|p| alive.contains(p))
        .collect();
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        out.push(p);
        if let Some(c) = children.get(&p) {
            stack.extend(c.iter().copied());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Linux: /proc

/// The parent pid in a `/proc/<pid>/stat` line (field 4). The command name (field 2) is in
/// parentheses and may contain spaces, so fields are counted after the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Nanoseconds on CPU: the first field of `schedstat`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_schedstat_ns(schedstat: &str) -> Option<u64> {
    schedstat.split_whitespace().next()?.parse().ok()
}

/// Voluntary + involuntary context switches in a `status` file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_ctxt_switches(status: &str) -> Option<u64> {
    let field = |key: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    Some(field("voluntary_ctxt_switches:")? + field("nonvoluntary_ctxt_switches:")?)
}

#[cfg(target_os = "linux")]
fn sample_procs(roots: &[u32]) -> Vec<Obs> {
    let mut table = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for e in dir.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            if let Some(ppid) = std::fs::read_to_string(e.path().join("stat"))
                .ok()
                .and_then(|s| parse_stat_ppid(&s))
            {
                table.push((pid, ppid));
            }
        }
    }
    let mut out = Vec::new();
    for pid in tree(roots, &table) {
        // Processes and threads can exit between the listing and the reads: skip them.
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for t in tasks.flatten() {
            let Some(tid) = t.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let dir = t.path();
            let Some(ns) = std::fs::read_to_string(dir.join("schedstat"))
                .ok()
                .and_then(|s| parse_schedstat_ns(&s))
            else {
                continue;
            };
            let switches = std::fs::read_to_string(dir.join("status"))
                .ok()
                .and_then(|s| parse_ctxt_switches(&s));
            out.push(Obs {
                key: tid,
                pid,
                cpu_s: ns as f64 / 1e9,
                switches,
            });
        }
    }
    out
}

/// RAPL package zones, accumulated across counter wraps.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
struct Rapl {
    zones: Vec<Zone>,
    /// Energy since [`Rapl::open`], microjoules.
    total_uj: u64,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
struct Zone {
    energy: PathBuf,
    max_uj: u64,
    last_uj: u64,
}

/// Energy between two readings of a counter that wraps to 0 after `max_uj`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn rapl_delta(prev: u64, cur: u64, max_uj: u64) -> u64 {
    if cur >= prev {
        cur - prev
    } else {
        max_uj.saturating_sub(prev) + cur
    }
}

/// A top-level zone directory (`intel-rapl:0`, not the subzone `intel-rapl:0:1` or
/// `intel-rapl-mmio:0`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_top_level_zone(dir_name: &str) -> bool {
    dir_name
        .strip_prefix("intel-rapl:")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl Rapl {
    /// Every readable top-level `package-*` zone under `root`, or why there is none.
    fn open(root: &Path) -> Result<Rapl, String> {
        let dir = std::fs::read_dir(root)
            .map_err(|e| format!("rapl: {} unreadable ({e}): no RAPL here", root.display()))?;
        let mut names: Vec<String> = dir
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| is_top_level_zone(n))
            .collect();
        names.sort();
        if names.is_empty() {
            return Err(format!(
                "rapl: no intel-rapl package zones in {} (VM or unsupported CPU)",
                root.display()
            ));
        }
        let mut zones = Vec::new();
        let mut errors = Vec::new();
        for n in names {
            let d = root.join(&n);
            let name = std::fs::read_to_string(d.join("name")).unwrap_or_default();
            // Package zones only: `psys` (platform) on laptops already includes the packages.
            if !name.trim().starts_with("package") {
                continue;
            }
            let read = |f: &str| -> Result<u64, String> {
                let p = d.join(f);
                std::fs::read_to_string(&p)
                    .map_err(|e| format!("{}: {e}", p.display()))?
                    .trim()
                    .parse()
                    .map_err(|e| format!("{}: {e}", p.display()))
            };
            match (read("energy_uj"), read("max_energy_range_uj")) {
                (Ok(last_uj), Ok(max_uj)) => zones.push(Zone {
                    energy: d.join("energy_uj"),
                    max_uj,
                    last_uj,
                }),
                (Err(e), _) | (_, Err(e)) => errors.push(e),
            }
        }
        if zones.is_empty() {
            return Err(if errors.is_empty() {
                "rapl: no package zones".to_owned()
            } else {
                format!(
                    "rapl: package counters unreadable (root-only on most kernels since 2020): {}",
                    errors.join("; ")
                )
            });
        }
        Ok(Rapl { zones, total_uj: 0 })
    }

    fn tick(&mut self) {
        for z in &mut self.zones {
            let Some(cur) = std::fs::read_to_string(&z.energy)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
            else {
                continue;
            };
            self.total_uj += rapl_delta(z.last_uj, cur, z.max_uj);
            z.last_uj = cur;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// macOS: ps and powermetrics

/// `ps` CPU time in seconds: `[[dd-]hh:]mm:ss[.frac]` (macOS prints minutes past 59).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_cputime(s: &str) -> Option<f64> {
    let (days, clock) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, s),
    };
    let mut total = 0.0;
    for part in clock.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + total)
}

/// Rows of `ps -A -o pid=,ppid=,time=`: pid, parent pid, CPU seconds.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_ps_rows(text: &str) -> Vec<(u32, u32, f64)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((
                f.next()?.parse().ok()?,
                f.next()?.parse().ok()?,
                parse_cputime(f.next()?)?,
            ))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn sample_procs(roots: &[u32]) -> Vec<Obs> {
    let Ok(o) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,time="])
        .output()
    else {
        return Vec::new();
    };
    let rows = parse_ps_rows(&String::from_utf8_lossy(&o.stdout));
    let table: Vec<(u32, u32)> = rows.iter().map(|r| (r.0, r.1)).collect();
    let cpu: HashMap<u32, f64> = rows.iter().map(|r| (r.0, r.2)).collect();
    tree(roots, &table)
        .into_iter()
        .filter_map(|pid| {
            Some(Obs {
                key: pid,
                pid,
                cpu_s: *cpu.get(&pid)?,
                switches: None,
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sample_procs(_roots: &[u32]) -> Vec<Obs> {
    Vec::new()
}

/// A value of a property list (the XML form `powermetrics -f plist` writes).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Plist {
    Dict(Vec<(String, Plist)>),
    Array(Vec<Plist>),
    Integer(i64),
    Real(f64),
    String(String),
    Bool(bool),
    /// `<date>`, `<data>` and anything else, as text.
    Other(String),
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Plist {
    fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Self::Dict(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn f64(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            Self::Integer(i) => Some(*i as f64),
            Self::Real(r) => Some(*r),
            _ => None,
        }
    }
}

/// Parse one XML plist document. A minimal reader for what `powermetrics` writes: no comments,
/// no CDATA; `None` on anything malformed (a sample cut off when the process was stopped).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_plist(xml: &str) -> Option<Plist> {
    let at = xml.find("<plist")?;
    let rest = &xml[at..];
    let mut c = Cursor {
        s: &rest[rest.find('>')? + 1..],
    };
    c.value()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Cursor<'a> {
    s: &'a str,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl<'a> Cursor<'a> {
    /// The next tag's name and whether it is self-closing (`<true/>`); consumes it.
    fn tag(&mut self) -> Option<(&'a str, bool)> {
        let rest = self.s.trim_start().strip_prefix('<')?;
        let end = rest.find('>')?;
        let (inner, empty) = match rest[..end].strip_suffix('/') {
            Some(i) => (i, true),
            None => (&rest[..end], false),
        };
        self.s = &rest[end + 1..];
        Some((inner.split_whitespace().next()?, empty))
    }

    /// Consume `</name>` if it comes next.
    fn close(&mut self, name: &str) -> bool {
        let t = self.s.trim_start();
        match t
            .strip_prefix("</")
            .and_then(|r| r.strip_prefix(name))
            .and_then(|r| r.strip_prefix('>'))
        {
            Some(r) => {
                self.s = r;
                true
            }
            None => false,
        }
    }

    /// Text up to `</name>`, unescaped; consumes the closing tag.
    fn text(&mut self, name: &str) -> Option<String> {
        let close = format!("</{name}>");
        let end = self.s.find(&close)?;
        let t = unescape(&self.s[..end]);
        self.s = &self.s[end + close.len()..];
        Some(t)
    }

    fn value(&mut self) -> Option<Plist> {
        let (name, empty) = self.tag()?;
        Some(match (name, empty) {
            ("true", _) => Plist::Bool(true),
            ("false", _) => Plist::Bool(false),
            ("dict", true) => Plist::Dict(Vec::new()),
            ("array", true) => Plist::Array(Vec::new()),
            ("string", true) => Plist::String(String::new()),
            (_, true) => Plist::Other(String::new()),
            ("dict", false) => {
                let mut kv = Vec::new();
                while !self.close("dict") {
                    let (k, k_empty) = self.tag()?;
                    if k != "key" {
                        return None;
                    }
                    let key = if k_empty {
                        String::new()
                    } else {
                        self.text("key")?
                    };
                    kv.push((key, self.value()?));
                }
                Plist::Dict(kv)
            }
            ("array", false) => {
                let mut items = Vec::new();
                while !self.close("array") {
                    items.push(self.value()?);
                }
                Plist::Array(items)
            }
            ("integer", false) => {
                let t = self.text("integer")?;
                let t = t.trim();
                // Counters past i64 are kept as reals rather than dropped.
                t.parse::<i64>()
                    .map(Plist::Integer)
                    .or_else(|_| t.parse::<f64>().map(Plist::Real))
                    .ok()?
            }
            ("real", false) => Plist::Real(self.text("real")?.trim().parse().ok()?),
            ("string", false) => Plist::String(self.text("string")?),
            (other, false) => Plist::Other(self.text(other)?),
        })
    }
}

/// XML entities: the five named ones and numeric references.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find(';') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let ent = &after[..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16).ok())
                .unwrap_or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(ent);
                out.push(';');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One `powermetrics` sample, reduced to what the meter uses.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct PmSample {
    elapsed_s: f64,
    /// Package (or CPU + GPU + ANE) energy over the sample, millijoules.
    energy_mj: Option<f64>,
    tasks: Vec<PmTask>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct PmTask {
    pid: u32,
    cputime_ms_per_s: f64,
    /// Idle + interrupt wakeups per second.
    wakeups_per_s: f64,
    energy_impact_per_s: Option<f64>,
}

/// Energy over a sample from its `processor` dict, whichever form the machine reports:
/// `package_energy` (mJ), `combined_power` (mW), `cpu_energy` + `gpu_energy` + `ane_energy`
/// (mJ), `package_joules`, `package_watts`, `cpu_power` (mW). `None` when none is present (some
/// virtual Macs).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn processor_energy_mj(p: &Plist, elapsed_s: f64) -> Option<f64> {
    if let Some(e) = p.f64("package_energy") {
        return Some(e);
    }
    if let Some(mw) = p.f64("combined_power") {
        return Some(mw * elapsed_s);
    }
    if let Some(cpu) = p.f64("cpu_energy") {
        return Some(cpu + p.f64("gpu_energy").unwrap_or(0.0) + p.f64("ane_energy").unwrap_or(0.0));
    }
    if let Some(j) = p.f64("package_joules") {
        return Some(j * 1e3);
    }
    if let Some(w) = p.f64("package_watts") {
        return Some(w * 1e3 * elapsed_s);
    }
    p.f64("cpu_power").map(|mw| mw * elapsed_s)
}

/// The samples of a `powermetrics -f plist` stream (plists separated by NUL bytes); cut-off or
/// malformed samples are skipped. `default_s`: the sample length when `elapsed_ns` is missing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_powermetrics(bytes: &[u8], default_s: f64) -> Vec<PmSample> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|doc| parse_plist(&String::from_utf8_lossy(doc)))
        .map(|s| {
            let elapsed_s = s.f64("elapsed_ns").map_or(default_s, |ns| ns / 1e9);
            let energy_mj = s
                .get("processor")
                .and_then(|p| processor_energy_mj(p, elapsed_s));
            let tasks = match s.get("tasks") {
                Some(Plist::Array(ts)) => ts
                    .iter()
                    .filter_map(|t| {
                        Some(PmTask {
                            pid: u32::try_from(t.f64("pid")? as i64).ok()?,
                            cputime_ms_per_s: t.f64("cputime_ms_per_s").unwrap_or(0.0),
                            wakeups_per_s: t.f64("idle_wakeups_per_s").unwrap_or(0.0)
                                + t.f64("intr_wakeups_per_s").unwrap_or(0.0),
                            energy_impact_per_s: t
                                .f64("energy_impact_per_s")
                                .or_else(|| t.f64("energy_impact")),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            PmSample {
                elapsed_s,
                energy_mj,
                tasks,
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod pm {
    //! Running `powermetrics` for a window.

    use std::collections::HashSet;
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    use std::sync::OnceLock;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::{EnergySource, PmSample, Reading, parse_powermetrics};

    pub(super) struct Probe {
        /// `sudo -n powermetrics` runs.
        pub(super) runs: bool,
        /// Its samples carry processor energy.
        pub(super) energy: bool,
        pub(super) note: String,
    }

    fn wait_bounded(child: &mut Child, limit: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < limit {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Whether `powermetrics` runs without a password and reports energy (once per process).
    pub(super) fn probe() -> &'static Probe {
        static PROBE: OnceLock<Probe> = OnceLock::new();
        PROBE.get_or_init(|| {
            let child = Command::new("sudo")
                .args(["-n", "powermetrics", "--samplers", "cpu_power", "-i", "200"])
                .args(["-n", "1", "-f", "plist"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    return Probe {
                        runs: false,
                        energy: false,
                        note: format!("powermetrics: cannot run sudo: {e}"),
                    };
                }
            };
            let mut out = Vec::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_end(&mut out);
            }
            if !wait_bounded(&mut child, Duration::from_secs(10)) {
                let _ = child.kill();
            }
            let ok = child.wait().is_ok_and(|s| s.success());
            let samples = parse_powermetrics(&out, 0.2);
            if !ok || samples.is_empty() {
                return Probe {
                    runs: false,
                    energy: false,
                    note: "powermetrics: `sudo -n powermetrics` failed (needs passwordless sudo)"
                        .into(),
                };
            }
            let energy = samples.iter().any(|s| s.energy_mj.is_some());
            Probe {
                runs: true,
                energy,
                note: if energy {
                    String::new()
                } else {
                    "powermetrics: no processor energy or power in its samples (virtual Mac?)"
                        .into()
                },
            }
        })
    }

    /// A `powermetrics` child streaming plists; its output is read on a thread.
    pub(super) struct Powermetrics {
        child: Child,
        chunks: mpsc::Receiver<Vec<u8>>,
        interval_s: f64,
    }

    impl Powermetrics {
        pub(super) fn spawn(interval: Duration) -> Option<Self> {
            let ms = interval.as_millis().max(100).to_string();
            let mut child = Command::new("sudo")
                .args(["-n", "powermetrics", "--samplers", "cpu_power,tasks"])
                .args(["--show-process-energy", "-i", &ms, "-f", "plist"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let mut stdout = child.stdout.take()?;
            let (tx, rx) = mpsc::channel();
            // Detached: if a root `powermetrics` outlives `sudo`, its pipe stays open and this
            // read never ends; `finish` stops waiting instead of joining.
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                while let Ok(n) = stdout.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            Some(Self {
                child,
                chunks: rx,
                interval_s: interval.as_secs_f64(),
            })
        }

        /// Stop it (SIGTERM to `sudo`, which relays it; then `pkill`; then a kill) and parse what
        /// it wrote.
        pub(super) fn finish(mut self) -> Vec<PmSample> {
            let pid = self.child.id().to_string();
            let _ = Command::new("kill").args(["-TERM", &pid]).status();
            if !wait_bounded(&mut self.child, Duration::from_secs(3)) {
                let _ = Command::new("sudo")
                    .args(["-n", "pkill", "-TERM", "-x", "powermetrics"])
                    .status();
                if !wait_bounded(&mut self.child, Duration::from_secs(2)) {
                    let _ = self.child.kill();
                }
            }
            let _ = self.child.wait();
            let mut out = Vec::new();
            while let Ok(c) = self.chunks.recv_timeout(Duration::from_millis(500)) {
                out.extend(c);
            }
            parse_powermetrics(&out, self.interval_s)
        }
    }

    /// Fill `r` from `samples`: package energy, and the tasks of `pids` (when `processes`).
    /// Rates are averaged over the time the samples cover and scaled to the window.
    pub(super) fn apply(
        r: &mut Reading,
        samples: &[PmSample],
        pids: &HashSet<u32>,
        processes: bool,
    ) {
        let covered: f64 = samples.iter().map(|s| s.elapsed_s).sum();
        if samples.is_empty() || covered <= 0.0 {
            r.energy_note
                .get_or_insert_with(|| "powermetrics: no complete sample in the window".into());
            return;
        }
        let with_energy: Vec<&PmSample> =
            samples.iter().filter(|s| s.energy_mj.is_some()).collect();
        if !with_energy.is_empty() {
            let t: f64 = with_energy.iter().map(|s| s.elapsed_s).sum();
            let mj: f64 = with_energy.iter().filter_map(|s| s.energy_mj).sum();
            let w = mj / 1e3 / t.max(1e-9);
            r.package_w = Some(w);
            r.package_j = Some(w * r.wall_s);
            r.energy_source = EnergySource::Powermetrics;
        }
        if !processes || samples.iter().all(|s| s.tasks.is_empty()) {
            return;
        }
        let (mut cpu, mut wake, mut impact, mut has_impact) = (0.0, 0.0, 0.0, false);
        for s in samples {
            for t in s.tasks.iter().filter(|t| pids.contains(&t.pid)) {
                cpu += t.cputime_ms_per_s / 1e3 * s.elapsed_s;
                wake += t.wakeups_per_s * s.elapsed_s;
                if let Some(e) = t.energy_impact_per_s {
                    impact += e * s.elapsed_s;
                    has_impact = true;
                }
            }
        }
        let scale = r.wall_s / covered;
        r.cpu_s = Some(cpu * scale);
        r.wakeups = Some(wake * scale);
        r.energy_impact = has_impact.then_some(impact / covered);
        r.process_source = "powermetrics";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_parsers() {
        let stat = "1234 (my (odd) proc) S 77 1234 1234 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 4";
        assert_eq!(parse_stat_ppid(stat), Some(77));
        assert_eq!(parse_stat_ppid("garbage"), None);
        assert_eq!(parse_schedstat_ns("123456789 2000 42\n"), Some(123_456_789));
        let status = "Name:\tx\nvoluntary_ctxt_switches:\t10\nnonvoluntary_ctxt_switches:\t5\n";
        assert_eq!(parse_ctxt_switches(status), Some(15));
        assert_eq!(parse_ctxt_switches("Name:\tx\n"), None);
    }

    #[test]
    fn process_tree_includes_descendants_only() {
        // 1 -> 10 -> 11 -> 12, 1 -> 20, 30 unrelated; 99 is gone.
        let table = [(1, 0), (10, 1), (11, 10), (12, 11), (20, 1), (30, 0)];
        let mut t = tree(&[10, 99], &table);
        t.sort();
        assert_eq!(t, [10, 11, 12]);
        assert!(tree(&[], &table).is_empty());
        assert!(tree(&[99], &table).is_empty());
    }

    #[test]
    fn counters_tolerate_exits_and_count_new_threads_whole() {
        let o = |key, cpu_s, sw| Obs {
            key,
            pid: 1,
            cpu_s,
            switches: Some(sw),
        };
        let mut c = Counters::default();
        c.observe(&[o(1, 10.0, 100), o(2, 5.0, 50)], true);
        // Thread 2 exits after this sample; thread 3 starts.
        c.observe(&[o(1, 10.5, 110), o(2, 5.25, 52), o(3, 0.25, 4)], false);
        c.observe(&[o(1, 11.0, 120), o(3, 0.5, 8)], false);
        let (cpu, sw) = c.totals();
        assert!((cpu - (1.0 + 0.25 + 0.5)).abs() < 1e-9, "{cpu}");
        assert_eq!(sw, Some(20 + 2 + 8));
        assert_eq!(c.pids.len(), 1);
    }

    #[test]
    fn rapl_wraps_and_skips_non_package_zones() {
        assert_eq!(rapl_delta(100, 250, 1_000), 150);
        assert_eq!(rapl_delta(900, 50, 1_000), 150);
        assert_eq!(rapl_delta(5, 5, 1_000), 0);
        assert!(is_top_level_zone("intel-rapl:0"));
        assert!(is_top_level_zone("intel-rapl:12"));
        assert!(!is_top_level_zone("intel-rapl:0:1"));
        assert!(!is_top_level_zone("intel-rapl-mmio:0"));
        assert!(!is_top_level_zone("intel-rapl:"));

        let root = tempfile::tempdir().unwrap();
        let zone = |dir: &str, name: &str, uj: u64| {
            let d = root.path().join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(d.join("energy_uj"), format!("{uj}\n")).unwrap();
            std::fs::write(d.join("max_energy_range_uj"), "1000000\n").unwrap();
        };
        assert!(
            Rapl::open(root.path())
                .unwrap_err()
                .contains("no intel-rapl")
        );
        zone("intel-rapl:0", "package-0", 999_000);
        zone("intel-rapl:0:0", "core", 1);
        zone("intel-rapl:1", "psys", 5);
        let mut r = Rapl::open(root.path()).unwrap();
        assert_eq!(r.zones.len(), 1, "only the package zone");
        std::fs::write(root.path().join("intel-rapl:0/energy_uj"), "4000").unwrap();
        r.tick();
        assert_eq!(r.total_uj, 1_000 + 4_000, "wrapped once");
        std::fs::write(root.path().join("intel-rapl:0/energy_uj"), "6000").unwrap();
        r.tick();
        assert_eq!(r.total_uj, 7_000);
    }

    #[test]
    fn rapl_reports_unreadable_counters() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("intel-rapl:0");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("name"), "package-0").unwrap();
        std::fs::write(d.join("max_energy_range_uj"), "10").unwrap();
        // A directory where the counter should be: unreadable as a number, even for root.
        std::fs::create_dir(d.join("energy_uj")).unwrap();
        let err = Rapl::open(root.path()).unwrap_err();
        assert!(err.contains("unreadable"), "{err}");
    }

    #[test]
    fn ps_rows_and_cputime() {
        assert_eq!(parse_cputime("0:01.50"), Some(1.5));
        assert_eq!(parse_cputime("75:00.00"), Some(4_500.0));
        assert_eq!(parse_cputime("1-01:00:00"), Some(90_000.0));
        assert_eq!(parse_cputime("x"), None);
        let rows = parse_ps_rows("  1     0   0:10.00\n 42     1   1:00.25\nbogus\n");
        assert_eq!(rows, [(1, 0, 10.0), (42, 1, 60.25)]);
    }

    /// Two samples as `powermetrics -f plist` writes them (Apple silicon keys in the first, an
    /// Intel `package_joules` in the second), NUL-separated, plus a cut-off third.
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>is_delta</key><true/>
  <key>elapsed_ns</key><integer>1000000000</integer>
  <key>hw_model</key><string>Mac &amp; co</string>
  <key>timestamp</key><date>2026-10-30T10:00:00Z</date>
  <key>tasks</key>
  <array>
    <dict>
      <key>pid</key><integer>42</integer>
      <key>name</key><string>nuggetd</string>
      <key>cputime_ms_per_s</key><real>20.5</real>
      <key>idle_wakeups_per_s</key><real>3</real>
      <key>intr_wakeups_per_s</key><real>1</real>
      <key>energy_impact</key><real>2.5</real>
      <key>energy_impact_per_s</key><real>2.0</real>
    </dict>
    <dict>
      <key>pid</key><integer>7</integer>
      <key>cputime_ms_per_s</key><real>500</real>
    </dict>
    <dict/>
  </array>
  <key>processor</key>
  <dict>
    <key>clusters</key><array/>
    <key>cpu_energy</key><integer>800</integer>
    <key>gpu_energy</key><integer>150</integer>
    <key>ane_energy</key><integer>50</integer>
  </dict>
</dict>
</plist>
"#;

    fn stream() -> Vec<u8> {
        let second = FIXTURE
            .replace(
                "<key>cpu_energy</key><integer>800</integer>",
                "<key>package_joules</key><real>3.0</real>",
            )
            .replace(
                "<integer>1000000000</integer>",
                "<integer>2000000000</integer>",
            );
        let mut b = FIXTURE.as_bytes().to_vec();
        b.push(0);
        b.extend(second.as_bytes());
        b.push(0);
        b.extend(&FIXTURE.as_bytes()[..FIXTURE.len() / 2]);
        b
    }

    #[test]
    fn plist_reader() {
        let p = parse_plist(FIXTURE).unwrap();
        assert_eq!(p.get("is_delta"), Some(&Plist::Bool(true)));
        assert_eq!(p.f64("elapsed_ns"), Some(1e9));
        assert_eq!(p.get("hw_model"), Some(&Plist::String("Mac & co".into())));
        assert!(matches!(p.get("tasks"), Some(Plist::Array(a)) if a.len() == 3));
        assert_eq!(unescape("a&lt;b&#62;c&#x41;&bogus;"), "a<b>cA&bogus;");
        assert!(parse_plist("<plist><dict><key>a</key>").is_none());
        assert!(parse_plist("no plist").is_none());
    }

    #[test]
    fn powermetrics_samples() {
        let s = parse_powermetrics(&stream(), 1.0);
        assert_eq!(s.len(), 2, "the cut-off third sample is skipped");
        assert_eq!(s[0].elapsed_s, 1.0);
        assert_eq!(s[0].energy_mj, Some(1_000.0), "cpu + gpu + ane");
        assert_eq!(s[1].elapsed_s, 2.0);
        assert_eq!(s[1].energy_mj, Some(3_000.0), "package_joules");
        let t = &s[0].tasks;
        assert_eq!(t.len(), 2, "the task without a pid is skipped");
        assert_eq!(t[0].pid, 42);
        assert_eq!(t[0].cputime_ms_per_s, 20.5);
        assert_eq!(t[0].wakeups_per_s, 4.0);
        assert_eq!(t[0].energy_impact_per_s, Some(2.0));
        assert_eq!(t[1].energy_impact_per_s, None);

        let power =
            parse_plist("<plist><dict><key>combined_power</key><real>1500</real></dict></plist>")
                .unwrap();
        assert_eq!(processor_energy_mj(&power, 2.0), Some(3_000.0));
        let none = parse_plist("<plist><dict><key>clusters</key><array/></dict></plist>").unwrap();
        assert_eq!(processor_energy_mj(&none, 1.0), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn powermetrics_reading() {
        let s = parse_powermetrics(&stream(), 1.0);
        let mut r = Reading {
            wall_s: 3.0,
            ..Reading::default()
        };
        pm::apply(&mut r, &s, &HashSet::from([42]), true);
        assert_eq!(r.energy_source, EnergySource::Powermetrics);
        // 4 J over 3 s of samples.
        assert!((r.package_w.unwrap() - 4.0 / 3.0).abs() < 1e-9);
        assert!((r.cpu_s.unwrap() - 0.0205 * 3.0).abs() < 1e-9);
        assert!((r.wakeups.unwrap() - 12.0).abs() < 1e-9);
        assert_eq!(r.process_source, "powermetrics");
    }

    #[test]
    fn reading_arithmetic() {
        let r = Reading {
            wall_s: 10.0,
            cpu_s: Some(0.5),
            wakeups: Some(30.0),
            package_w: Some(12.0),
            ..Reading::default()
        };
        let base = Reading {
            wall_s: 5.0,
            package_w: Some(10.0),
            ..Reading::default()
        };
        assert_eq!(r.cpu_percent(), Some(5.0));
        assert_eq!(r.wakeups_per_s(), Some(3.0));
        assert_eq!(r.attributable_w(&base), Some(2.0));
        assert_eq!(r.attributable_j(&base), Some(20.0));
        assert_eq!(r.attributable_j(&Reading::default()), None);
        assert_eq!(EnergySource::default().as_str(), "none");
    }

    /// This process, metered for a moment while it spins: CPU time and descendants are seen.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn meters_this_process_and_a_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let m = Meter::start(&[std::process::id()]);
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(300) {
            x = x.wrapping_add(std::hint::black_box(1));
        }
        let r = m.stop();
        let _ = child.kill();
        let _ = child.wait();
        assert!(x > 0);
        assert!(r.wall_s >= 0.3);
        assert!(r.processes >= 2, "the child counts: {r:?}");
        if cfg!(target_os = "linux") {
            assert_eq!(r.process_source, "proc");
            assert!(r.cpu_s.unwrap() > 0.1, "{r:?}");
            assert!(r.wakeups.is_some());
        }
        let (source, note) = energy_source();
        assert_eq!(source == EnergySource::None, note.is_some());
        if cfg!(target_os = "linux") {
            assert_eq!(r.energy_source, source);
        }
    }

    #[test]
    fn groups_are_read_separately() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let me = std::process::id();
        let m = Meter::start_groups(&[&[me], &[child.id()], &[]]);
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(300) {
            x = x.wrapping_add(std::hint::black_box(1));
        }
        let r = m.stop_groups();
        let _ = child.kill();
        let _ = child.wait();
        assert!(x > 0);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|r| r.wall_s >= 0.3));
        assert_eq!(r[2].processes, 0);
        assert_eq!(r[2].cpu_s, None);
        if cfg!(target_os = "linux") {
            // The bench process (with its child) and the sleeping child alone.
            assert!(r[0].processes >= 2, "{:?}", r[0]);
            assert_eq!(r[1].processes, 1, "{:?}", r[1]);
            assert!(r[0].cpu_s.unwrap() > 0.1, "{:?}", r[0]);
            assert!(r[1].cpu_s.unwrap() < r[0].cpu_s.unwrap());
            assert_eq!(r[0].package_w, r[1].package_w, "one system energy figure");
        }
    }
}
