//! `[proactive]` in `config.toml`, and the clock math around it (brief time, snooze default,
//! refresh debounce, the machine's UTC offset).
//!
//! ```toml
//! [proactive]
//! enabled = true          # false: methods answer (cards.list is empty), nothing runs
//! brief_time = "08:00"    # local time of the morning brief (`brief.ready`)
//! judge = "local"         # "local" | "cloud" | "off": where the optional LLM judge may run
//! refresh_minutes = 30    # periodic refresh
//! debounce_secs = 60      # quiet time after ingest/knowledge runs before refreshing
//! # utc_offset = "+02:00" # default: the machine's offset, asked again every 15 minutes and
//!                         # before the brief fires (daylight saving changes while running)
//!
//! [proactive.engine]      # nugget_proactive::ProactiveConfig (thresholds, inbox size, ...)
//! max_inbox = 12
//! user_addresses = ["me@example.com"]
//! ```
//!
//! `engine.utc_offset` and `engine.judge_allow_cloud` are derived from `utc_offset` and `judge`
//! and any value given there is replaced. An explicit `utc_offset` stays fixed; otherwise
//! [`LocalOffset`] follows the machine's.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nugget_core::{Error, Result};
use nugget_proactive::ProactiveConfig;
use serde::Deserialize;
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// Where the LLM judge may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum JudgeMode {
    /// On-device runtime and local-network providers only.
    #[default]
    Local,
    /// A cloud provider may be used; sensitive candidates are still never sent to it.
    Cloud,
    /// No judge: deterministic ranking only.
    Off,
}

/// The raw `[proactive]` table.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Section {
    pub enabled: bool,
    pub brief_time: String,
    pub judge: JudgeMode,
    pub refresh_minutes: u64,
    pub debounce_secs: u64,
    pub utc_offset: Option<String>,
    pub engine: ProactiveConfig,
}

impl Default for Section {
    fn default() -> Self {
        Self {
            enabled: true,
            brief_time: "08:00".into(),
            judge: JudgeMode::Local,
            refresh_minutes: 30,
            debounce_secs: 60,
            utc_offset: None,
            engine: ProactiveConfig::default(),
        }
    }
}

/// Validated settings.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub brief_time: Time,
    pub judge: JudgeMode,
    pub refresh: Duration,
    pub debounce: Duration,
    /// The offset at start: `utc_offset`, else the machine's then (see [`LocalOffset`]).
    pub offset: UtcOffset,
    /// `utc_offset` was given: the offset never follows the machine's.
    pub offset_fixed: bool,
    /// Engine config, with `utc_offset` and `judge_allow_cloud` filled in.
    pub engine: ProactiveConfig,
}

impl Settings {
    /// Validate `section`. `local_offset` is asked only when `utc_offset` is unset.
    pub fn from_section(
        section: Section,
        local_offset: impl FnOnce() -> UtcOffset,
    ) -> Result<Self> {
        let bad = |m: String| Error::Invalid(format!("[proactive]: {m}"));
        let brief_time = parse_hhmm(&section.brief_time)
            .ok_or_else(|| bad(format!("brief_time {:?} is not HH:MM", section.brief_time)))?;
        if section.refresh_minutes == 0 {
            return Err(bad("refresh_minutes must be at least 1".into()));
        }
        let offset = match &section.utc_offset {
            Some(s) => {
                parse_offset(s).ok_or_else(|| bad(format!("utc_offset {s:?} is not ±HH:MM")))?
            }
            None => local_offset(),
        };
        let mut engine = section.engine;
        engine.utc_offset = offset;
        engine.judge_allow_cloud = section.judge == JudgeMode::Cloud;
        Ok(Self {
            enabled: section.enabled,
            brief_time,
            judge: section.judge,
            refresh: Duration::from_secs(section.refresh_minutes * 60),
            debounce: Duration::from_secs(section.debounce_secs),
            offset,
            offset_fixed: section.utc_offset.is_some(),
            engine,
        })
    }

    /// `[proactive]` parsed from its raw table.
    pub fn parse(table: &toml::Table, local_offset: impl FnOnce() -> UtcOffset) -> Result<Self> {
        Self::from_section(
            crate::features::section::<Section>("proactive", table)?,
            local_offset,
        )
    }

    /// The user's local date at `now` in `offset` (the current one, see [`LocalOffset`]).
    pub fn local_date(&self, now: OffsetDateTime, offset: UtcOffset) -> Date {
        now.to_offset(offset).date()
    }

    /// Default end of a snooze: tomorrow (local) at the brief time.
    pub fn default_snooze_until(&self, now: OffsetDateTime, offset: UtcOffset) -> OffsetDateTime {
        let tomorrow = self.local_date(now, offset).next_day().unwrap_or(Date::MAX);
        local_at(tomorrow, self.brief_time, offset)
    }

    /// When the brief should fire next, in `offset`. `last_fired` is the local date it last fired.
    /// When today's brief time has passed without a brief today, that is now (catch up after a
    /// late start).
    pub fn next_brief_at(
        &self,
        now: OffsetDateTime,
        last_fired: Option<Date>,
        offset: UtcOffset,
    ) -> OffsetDateTime {
        let today = self.local_date(now, offset);
        let at_today = local_at(today, self.brief_time, offset);
        if last_fired.is_some_and(|d| d >= today) {
            local_at(
                today.next_day().unwrap_or(Date::MAX),
                self.brief_time,
                offset,
            )
        } else if now < at_today {
            at_today
        } else {
            now
        }
    }
}

/// `date` at local `time` in `offset`.
pub(crate) fn local_at(date: Date, time: Time, offset: UtcOffset) -> OffsetDateTime {
    PrimitiveDateTime::new(date, time).assume_offset(offset)
}

/// `HH:MM` (24 h).
pub(crate) fn parse_hhmm(s: &str) -> Option<Time> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    Time::from_hms(h.parse().ok()?, m.parse().ok()?, 0).ok()
}

/// `+HH:MM`, `-HH:MM`, `+HHMM`, `Z` / `UTC`.
pub(crate) fn parse_offset(s: &str) -> Option<UtcOffset> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("z") || s.eq_ignore_ascii_case("utc") {
        return Some(UtcOffset::UTC);
    }
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1i8, &s[1..]),
        b'-' => (-1i8, &s[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) || rest.len() > 5 {
        return None;
    }
    let h: i8 = digits[..2].parse().ok()?;
    let m: i8 = digits[2..].parse().ok()?;
    if h > 18 || m > 59 {
        return None;
    }
    UtcOffset::from_hms(sign * h, sign * m, 0).ok()
}

/// How long a machine offset read by [`LocalOffset`] is trusted before it is asked again.
pub(crate) const OFFSET_RECHECK: Duration = Duration::from_secs(15 * 60);

/// Reads the machine's UTC offset (blocking); `None` when it cannot.
pub(crate) type OffsetSource = Arc<dyn Fn() -> Option<UtcOffset> + Send + Sync>;

/// The user's current UTC offset. Fixed when `utc_offset` is configured; otherwise the machine's,
/// asked again ([`LocalOffset::recheck`]) at most every `recheck` unless forced, so a
/// daylight-saving change while the daemon runs moves the brief and "today".
pub(crate) struct LocalOffset {
    /// Whole seconds east of UTC.
    current: AtomicI32,
    /// `None`: fixed.
    source: Option<OffsetSource>,
    recheck: Duration,
    checked: Mutex<Option<Instant>>,
}

impl LocalOffset {
    pub fn fixed(offset: UtcOffset) -> Self {
        Self {
            current: AtomicI32::new(offset.whole_seconds()),
            source: None,
            recheck: Duration::MAX,
            checked: Mutex::new(None),
        }
    }

    /// Follow `source`, starting from `initial` (just read from it, so the first recheck waits
    /// `recheck` unless forced).
    pub fn following(initial: UtcOffset, source: OffsetSource, recheck: Duration) -> Self {
        Self {
            current: AtomicI32::new(initial.whole_seconds()),
            source: Some(source),
            recheck,
            checked: Mutex::new(Some(Instant::now())),
        }
    }

    /// Fixed for an explicit `utc_offset`, else following the machine's (from the OS).
    pub fn for_settings(settings: &Settings) -> Self {
        if settings.offset_fixed {
            Self::fixed(settings.offset)
        } else {
            Self::following(settings.offset, Arc::new(os_offset), OFFSET_RECHECK)
        }
    }

    /// The offset as last read.
    pub fn current(&self) -> UtcOffset {
        UtcOffset::from_whole_seconds(self.current.load(Ordering::Relaxed))
            .unwrap_or(UtcOffset::UTC)
    }

    /// Ask the source again when the last read is older than `recheck` (or `force`). Returns the
    /// new offset when it changed. Blocking (it may run a process): call it off the async
    /// workers. A failed read keeps the current offset.
    pub fn recheck(&self, force: bool) -> Option<UtcOffset> {
        let source = self.source.as_ref()?;
        {
            let mut checked = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if !force && checked.is_some_and(|at| now.duration_since(at) < self.recheck) {
                return None;
            }
            *checked = Some(now);
        }
        let read = source()?;
        let before = self.current.swap(read.whole_seconds(), Ordering::Relaxed);
        (before != read.whole_seconds()).then_some(read)
    }
}

/// The machine's current UTC offset, asked from the OS (`date +%z` on Unix, PowerShell on
/// Windows); UTC (logged) when that fails. Blocking: call it off the async workers.
///
/// `time`'s own lookup is not used: it refuses to answer in a multi-threaded process on Unix.
pub(crate) fn detect_local_offset() -> UtcOffset {
    match os_offset() {
        Some(o) => o,
        None => {
            tracing::warn!(
                "cannot read the local UTC offset; using UTC for the brief time (set [proactive] utc_offset)"
            );
            UtcOffset::UTC
        }
    }
}

#[cfg(unix)]
fn os_offset() -> Option<UtcOffset> {
    let out = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_offset(String::from_utf8_lossy(&out.stdout).trim()))
        .flatten()
}

#[cfg(windows)]
fn os_offset() -> Option<UtcOffset> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Date).ToString('zzz')",
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_offset(String::from_utf8_lossy(&out.stdout).trim()))
        .flatten()
}

#[cfg(not(any(unix, windows)))]
fn os_offset() -> Option<UtcOffset> {
    None
}

/// Trailing debounce: each poke pushes the deadline to `delay` after it, so a burst of changes
/// (a backfill) runs once when it goes quiet. The periodic refresh covers bursts that never do.
#[derive(Debug, Clone)]
pub(crate) struct Debounce<I> {
    delay: Duration,
    due: Option<I>,
}

impl<I> Debounce<I>
where
    I: Copy + Ord + std::ops::Add<Duration, Output = I>,
{
    pub fn new(delay: Duration) -> Self {
        Self { delay, due: None }
    }

    /// Something changed at `now`.
    pub fn poke(&mut self, now: I) {
        self.due = Some(now + self.delay);
    }

    /// Schedule a run at `at` unless one is already due sooner.
    pub fn at(&mut self, at: I) {
        self.due = Some(self.due.map_or(at, |d| d.min(at)));
    }

    /// When the pending run is due.
    pub fn due(&self) -> Option<I> {
        self.due
    }

    /// The run started: clear the pending burst.
    pub fn fired(&mut self) {
        self.due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap()
    }
    fn hm(h: u8, m: u8) -> Time {
        Time::from_hms(h, m, 0).unwrap()
    }
    fn off(h: i8, m: i8) -> UtcOffset {
        UtcOffset::from_hms(h, m, 0).unwrap()
    }
    /// `y-m-d h:min` UTC.
    fn utc(y: i32, m: u8, d: u8, h: u8, min: u8) -> OffsetDateTime {
        local_at(date(y, m, d), hm(h, min), UtcOffset::UTC)
    }

    fn settings(toml_src: &str, local: UtcOffset) -> Result<Settings> {
        let t: toml::Table = toml::from_str(toml_src).unwrap();
        Settings::parse(&t, || local)
    }

    #[test]
    fn defaults() {
        let s = settings("", off(2, 0)).unwrap();
        assert!(s.enabled);
        assert_eq!(s.brief_time, hm(8, 0));
        assert_eq!(s.judge, JudgeMode::Local);
        assert_eq!(s.refresh, Duration::from_secs(1800));
        assert_eq!(s.debounce, Duration::from_secs(60));
        assert_eq!(s.offset, off(2, 0));
        assert!(!s.offset_fixed);
        assert_eq!(s.engine.utc_offset, off(2, 0));
        assert!(!s.engine.judge_allow_cloud);
        assert_eq!(s.engine.max_inbox, ProactiveConfig::default().max_inbox);
    }

    #[test]
    fn explicit_values_and_engine_table() {
        let s = settings(
            r#"
            enabled = false
            brief_time = "7:30"
            judge = "cloud"
            refresh_minutes = 5
            debounce_secs = 0
            utc_offset = "-05:00"
            [engine]
            max_inbox = 3
            user_addresses = ["me@example.com"]
            judge_allow_cloud = false
            "#,
            off(9, 0),
        )
        .unwrap();
        assert!(!s.enabled);
        assert_eq!(s.brief_time, hm(7, 30));
        assert_eq!(s.judge, JudgeMode::Cloud);
        assert!(s.engine.judge_allow_cloud, "derived from judge");
        assert_eq!(s.refresh, Duration::from_secs(300));
        assert_eq!(s.debounce, Duration::ZERO);
        assert_eq!(s.offset, off(-5, 0));
        assert!(s.offset_fixed);
        assert_eq!(s.engine.utc_offset, off(-5, 0));
        assert_eq!(s.engine.max_inbox, 3);
        assert_eq!(s.engine.user_addresses, ["me@example.com"]);
        let s = settings("judge = \"off\"", UtcOffset::UTC).unwrap();
        assert_eq!(s.judge, JudgeMode::Off);
    }

    #[test]
    fn errors_name_the_section() {
        for bad in [
            "brief_time = \"25:00\"",
            "brief_time = \"8\"",
            "brief_time = \"08:5\"",
            "judge = \"remote\"",
            "refresh_minutes = 0",
            "utc_offset = \"02:00\"",
            "nope = 1",
        ] {
            let err = settings(bad, UtcOffset::UTC).unwrap_err().to_string();
            assert!(err.contains("[proactive]"), "{bad}: {err}");
        }
    }

    #[test]
    fn offsets_parse() {
        assert_eq!(parse_offset("+0530"), Some(off(5, 30)));
        assert_eq!(parse_offset("+05:30"), Some(off(5, 30)));
        assert_eq!(parse_offset("-0800"), Some(off(-8, 0)));
        assert_eq!(parse_offset("Z"), Some(UtcOffset::UTC));
        assert_eq!(parse_offset("utc"), Some(UtcOffset::UTC));
        for bad in ["", "0530", "+5", "+19:00", "+05:60", "+05:3x", "+05::30"] {
            assert_eq!(parse_offset(bad), None, "{bad}");
        }
        assert_eq!(parse_hhmm("23:59"), Some(hm(23, 59)));
        assert_eq!(parse_hhmm("24:00"), None);
    }

    #[test]
    fn brief_fires_today_tomorrow_or_now() {
        // 08:00 at UTC+2 is 06:00 UTC.
        let s = settings("utc_offset = \"+02:00\"", UtcOffset::UTC).unwrap();
        let before = utc(2026, 3, 10, 5, 0);
        assert_eq!(
            s.next_brief_at(before, None, s.offset),
            utc(2026, 3, 10, 6, 0)
        );
        // Past the time and not fired today: catch up now.
        let after = utc(2026, 3, 10, 9, 0);
        assert_eq!(s.next_brief_at(after, None, s.offset), after);
        assert_eq!(
            s.next_brief_at(after, Some(date(2026, 3, 9)), s.offset),
            after,
            "yesterday's brief does not count"
        );
        // Fired today: tomorrow.
        assert_eq!(
            s.next_brief_at(after, Some(date(2026, 3, 10)), s.offset),
            utc(2026, 3, 11, 6, 0)
        );
    }

    #[test]
    fn brief_math_across_midnight_and_offset() {
        // 23:30 UTC on the 10th is already the 11th at UTC+2 (01:30 local).
        let s = settings("utc_offset = \"+02:00\"", UtcOffset::UTC).unwrap();
        let now = utc(2026, 3, 10, 23, 30);
        assert_eq!(s.local_date(now, s.offset), date(2026, 3, 11));
        assert_eq!(
            s.next_brief_at(now, Some(date(2026, 3, 10)), s.offset),
            utc(2026, 3, 11, 6, 0)
        );
        // West of UTC: 03:00 UTC on the 11th is 22:00 on the 10th at UTC-5; the 21:00 brief
        // already fired on the 10th, so the next is the 11th at 21:00 local (02:00 UTC on the 12th).
        let s = settings(
            "utc_offset = \"-05:00\"\nbrief_time = \"21:00\"",
            UtcOffset::UTC,
        )
        .unwrap();
        let now = utc(2026, 3, 11, 3, 0);
        assert_eq!(s.local_date(now, s.offset), date(2026, 3, 10));
        assert_eq!(
            s.next_brief_at(now, Some(date(2026, 3, 10)), s.offset),
            utc(2026, 3, 12, 2, 0)
        );
        // Snooze default: tomorrow (local) at the brief time.
        assert_eq!(
            s.default_snooze_until(now, s.offset),
            utc(2026, 3, 12, 2, 0)
        );
    }

    #[test]
    fn a_dst_change_between_briefs_moves_the_next_one() {
        // Brief at 08:00; fired on the 28th at UTC+2 (06:00 UTC).
        let s = settings("", off(2, 0)).unwrap();
        let after = utc(2026, 3, 28, 7, 0);
        let fired = Some(date(2026, 3, 28));
        assert_eq!(
            s.next_brief_at(after, fired, off(1, 0)),
            utc(2026, 3, 29, 7, 0),
            "08:00 at UTC+1"
        );
        assert_eq!(
            s.next_brief_at(after, fired, off(2, 0)),
            utc(2026, 3, 29, 6, 0),
            "08:00 at UTC+2"
        );
    }

    #[test]
    fn local_offset_follows_the_machine_unless_fixed() {
        let machine = Arc::new(AtomicI32::new(3600));
        let m = machine.clone();
        let source: OffsetSource = Arc::new(move || {
            let secs = m.load(Ordering::Relaxed);
            (secs != i32::MIN).then(|| UtcOffset::from_whole_seconds(secs).unwrap())
        });
        let o = LocalOffset::following(off(1, 0), source.clone(), Duration::ZERO);
        assert_eq!(o.current(), off(1, 0));
        assert_eq!(o.recheck(false), None, "unchanged");
        machine.store(7200, Ordering::Relaxed);
        assert_eq!(o.recheck(false), Some(off(2, 0)));
        assert_eq!(o.current(), off(2, 0));
        // A failed read keeps the last offset.
        machine.store(i32::MIN, Ordering::Relaxed);
        assert_eq!(o.recheck(true), None);
        assert_eq!(o.current(), off(2, 0));

        // Throttled: not asked again before `recheck` unless forced.
        machine.store(3600, Ordering::Relaxed);
        let o = LocalOffset::following(off(2, 0), source, Duration::from_secs(3600));
        assert_eq!(o.recheck(false), None);
        assert_eq!(o.current(), off(2, 0));
        assert_eq!(o.recheck(true), Some(off(1, 0)));

        // An explicit `utc_offset` never moves.
        let s = settings("utc_offset = \"+05:30\"", off(1, 0)).unwrap();
        let o = LocalOffset::for_settings(&s);
        assert_eq!(o.recheck(true), None);
        assert_eq!(o.current(), off(5, 30));
    }

    #[test]
    fn debounce_trails() {
        let d = Duration::from_secs(60);
        let t0 = std::time::Instant::now();
        let secs = |s: u64| t0 + Duration::from_secs(s);
        let mut b = Debounce::new(d);
        assert_eq!(b.due(), None);
        b.poke(t0);
        assert_eq!(b.due(), Some(secs(60)));
        b.poke(secs(30));
        assert_eq!(b.due(), Some(secs(90)), "pushed back");
        for s in (60..600).step_by(30) {
            b.poke(secs(s));
        }
        assert_eq!(b.due(), Some(secs(630)), "runs once the burst is quiet");
        b.fired();
        assert_eq!(b.due(), None);
        // `at` only moves a pending run earlier.
        b.at(secs(10));
        b.at(secs(50));
        assert_eq!(b.due(), Some(secs(10)));
    }
}
