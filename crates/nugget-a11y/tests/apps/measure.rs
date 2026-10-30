//! Platform-neutral measurement: text normalisation, payloads, thread scoring, diffs, and the
//! regression gate over cell records (`expected.json`). Unit-tested on every OS.

use serde_json::{Value, json};

/// The insertion payloads, by id.
pub fn payloads() -> Vec<(&'static str, String)> {
    vec![
        ("ascii", "Sounds good, see you at 3.".to_string()),
        ("newlines", "Line one\nLine two\n\nLine four".to_string()),
        (
            "emoji",
            "Thanks \u{1F44D}\u{1F3FD} see you \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} soon \u{1F389}"
                .to_string(),
        ),
        ("rtl", "שלום, مرحبا and hello".to_string()),
        ("combining", "Cafe\u{301} nin\u{303}o Z\u{324}\u{308}".to_string()),
        ("long", long_text()),
    ]
}

/// A deterministic ~5000-character multi-paragraph ASCII text (paragraphs joined by `\n`).
pub fn long_text() -> String {
    const WORDS: [&str; 24] = [
        "the", "meeting", "notes", "cover", "budget", "venue", "travel", "agenda", "review",
        "draft", "schedule", "team", "quarter", "plan", "update", "follow", "up", "with", "and",
        "for", "next", "week", "options", "summary",
    ];
    // A small linear congruential generator: same text on every run and OS.
    let mut x: u32 = 0x2545_f491;
    let mut next = || {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (x >> 16) as usize
    };
    let mut paras: Vec<String> = Vec::new();
    let mut total = 0;
    while total < 5000 {
        let mut p = String::new();
        let sentences = 3 + next() % 4;
        for _ in 0..sentences {
            let n = 6 + next() % 10;
            let mut s: Vec<&str> = (0..n).map(|_| WORDS[next() % WORDS.len()]).collect();
            let first = s[0];
            let cap = format!("{}{}", first[..1].to_ascii_uppercase(), &first[1..]);
            s[0] = "";
            if !p.is_empty() {
                p.push(' ');
            }
            p.push_str(&cap);
            p.push_str(&s.join(" "));
            p.push('.');
        }
        total += p.len() + 1;
        paras.push(p);
    }
    let mut out = paras.join("\n");
    // Trim to a whole word near 5000 characters, ending in a full stop.
    if out.len() > 5000 {
        let cut = out[..5000].rfind(' ').unwrap_or(5000);
        out.truncate(cut);
        out = out.trim_end_matches([' ', '\n', '.']).to_string();
        out.push('.');
    }
    out
}

/// The comparison normalisation: `\r\n` and lone `\r` to `\n`, NBSP to space, trailing
/// newlines removed.
pub fn norm(s: &str) -> String {
    let s = s
        .replace("\r\n", "\n")
        .replace(['\r'], "\n")
        .replace('\u{a0}', " ");
    s.trim_end_matches('\n').to_string()
}

/// Whitespace collapsed (any Unicode whitespace, NBSP included) and trimmed.
pub fn ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `n` characters, with an ellipsis when cut.
pub fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

/// Where two texts first differ (character index) and a short excerpt of each around it.
pub fn diff(expected: &str, got: &str) -> Option<Value> {
    if expected == got {
        return None;
    }
    let e: Vec<char> = expected.chars().collect();
    let g: Vec<char> = got.chars().collect();
    let i = e.iter().zip(&g).take_while(|(a, b)| a == b).count();
    let from = i.saturating_sub(15);
    let ex = |v: &[char]| -> String { v.iter().skip(from).take(50).collect() };
    Some(json!({
        "index": i,
        "expected": ex(&e),
        "got": ex(&g),
        "expected_len": e.len(),
        "got_len": g.len(),
    }))
}

/// One message of the ground truth.
#[derive(Debug, Clone, PartialEq)]
pub struct TruthMessage {
    pub text: String,
    pub from_me: bool,
}

/// One message as `focused().thread` reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct GotMessage {
    pub text: String,
    pub from_me: Option<bool>,
}

fn texts_match(got: &str, truth: &str) -> bool {
    let (g, t) = (ws(got), ws(truth));
    if g.is_empty() || t.is_empty() {
        return false;
    }
    if g == t {
        return true;
    }
    // Containment either way, but not of a fragment too short to mean anything.
    let (short, long) = if g.chars().count() < t.chars().count() {
        (&g, &t)
    } else {
        (&t, &g)
    };
    short.chars().count() >= 8 && long.contains(short.as_str())
}

/// Thread scores: precision and recall of the reported messages (each truth message matched at
/// most once, in order of the report), and `from_me` correctness over the matched pairs.
pub fn score_thread(got: &[GotMessage], truth: &[TruthMessage]) -> Value {
    let mut used = vec![false; truth.len()];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (gi, g) in got.iter().enumerate() {
        if let Some(ti) =
            (0..truth.len()).find(|&ti| !used[ti] && texts_match(&g.text, &truth[ti].text))
        {
            used[ti] = true;
            pairs.push((gi, ti));
        }
    }
    let m = pairs.len() as f64;
    let precision = (!got.is_empty()).then(|| m / got.len() as f64);
    let recall = (!truth.is_empty()).then(|| m / truth.len() as f64);
    let from_me_right = pairs
        .iter()
        .filter(|(gi, ti)| got[*gi].from_me == Some(truth[*ti].from_me))
        .count();
    let (from_me_ok, from_me_accuracy) = if pairs.is_empty() {
        (None, None)
    } else {
        (
            Some(from_me_right == pairs.len()),
            Some(from_me_right as f64 / pairs.len() as f64),
        )
    };
    json!({
        "got": got.len(),
        "truth": truth.len(),
        "matched": pairs.len(),
        "precision": precision.map(round3),
        "recall": recall.map(round3),
        "from_me_ok": from_me_ok,
        "from_me_accuracy": from_me_accuracy.map(round3),
    })
}

/// Every truth recipient appears (case-insensitive, whitespace-collapsed) in some reported one.
pub fn recipients_ok(got: &[String], truth: &[String]) -> Option<bool> {
    if truth.is_empty() {
        return None;
    }
    let got: Vec<String> = got.iter().map(|g| ws(g).to_lowercase()).collect();
    Some(truth.iter().all(|t| {
        let t = ws(t).to_lowercase();
        got.iter().any(|g| g.contains(&t))
    }))
}

/// The reported subject equals or contains the truth subject.
pub fn subject_ok(got: Option<&str>, truth: Option<&str>) -> Option<bool> {
    let t = ws(truth?).to_lowercase();
    if t.is_empty() {
        return None;
    }
    Some(got.is_some_and(|g| ws(g).to_lowercase().contains(&t)))
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Median and max of durations in milliseconds.
pub fn median_max(ms: &mut [f64]) -> Value {
    if ms.is_empty() {
        return Value::Null;
    }
    ms.sort_by(f64::total_cmp);
    let n = ms.len();
    let median = if n % 2 == 1 {
        ms[n / 2]
    } else {
        (ms[n / 2 - 1] + ms[n / 2]) / 2.0
    };
    json!({"median": round3(median), "max": round3(ms[n - 1])})
}

// ---- the regression gate ----

/// Whether metric `name` passes on a cell record: `Some(pass)`, or `None` for an unknown metric
/// name. Names: `available`, `no_error`, `detect`, `read`, `secure`, `thread_precision`,
/// `thread_recall`, `from_me`, `recipients`, `subject`, `insert:<payload>` (the oracle shows
/// exactly the expected text), `verified:<payload>` (the API said verified), `undo:<payload>`,
/// `insert:*` / `verified:*` / `undo:*` (every payload), and `read_ms` (median). Numeric
/// metrics take a threshold: `thread_recall>=0.8`, `read_ms<=250`; without one, the thread
/// scores must be 1.
pub fn metric_passes(rec: &Value, name: &str) -> Option<bool> {
    let name = name.trim();
    let (base, op, limit) = if let Some((b, l)) = name.split_once(">=") {
        (b.trim(), ">=", l.trim().parse::<f64>().ok()?)
    } else if let Some((b, l)) = name.split_once("<=") {
        (b.trim(), "<=", l.trim().parse::<f64>().ok()?)
    } else {
        (name, "", f64::NAN)
    };
    let available = rec["available"].as_bool() == Some(true);
    let no_error = available && rec.get("error").is_none_or(Value::is_null);
    let num = |v: &Value, default_limit: f64, default_op: &str| -> bool {
        let Some(x) = v.as_f64() else { return false };
        let (op, l) = if op.is_empty() {
            (default_op, default_limit)
        } else {
            (op, limit)
        };
        if op == ">=" { x >= l } else { x <= l }
    };
    let truthy = |v: &Value| v.as_bool() == Some(true);
    let inserts = rec["inserts"].as_array().cloned().unwrap_or_default();
    let per_payload = |field: &str, p: &str| -> bool {
        if p == "*" {
            !inserts.is_empty() && inserts.iter().all(|i| truthy(&i[field]))
        } else {
            inserts
                .iter()
                .find(|i| i["payload"] == p)
                .is_some_and(|i| truthy(&i[field]))
        }
    };
    Some(
        match base {
            "available" => available,
            "no_error" => no_error,
            "detect" => truthy(&rec["detect"]),
            "read" => truthy(&rec["read_ok"]),
            "secure" => truthy(&rec["secure_ok"]),
            "thread_precision" => num(&rec["thread"]["precision"], 1.0, ">="),
            "thread_recall" => num(&rec["thread"]["recall"], 1.0, ">="),
            "from_me" => truthy(&rec["thread"]["from_me_ok"]),
            "recipients" => truthy(&rec["thread"]["recipients_ok"]),
            "subject" => truthy(&rec["thread"]["subject_ok"]),
            "read_ms" => {
                if op.is_empty() {
                    return None;
                }
                num(&rec["read_ms"]["median"], limit, op)
            }
            other => {
                let (kind, p) = other.split_once(':')?;
                let field = match kind {
                    "insert" => "exact",
                    "verified" => "verified",
                    "undo" => "undo_one_step",
                    _ => return None,
                };
                if p != "*" && !payloads().iter().any(|(id, _)| *id == p) {
                    return None;
                }
                per_payload(field, p)
            }
        } && (available || base == "available"),
    )
}

/// The cell key used by `expected.json`: `<os>/<app>/<fixture>`.
pub fn cell_key(rec: &Value) -> String {
    format!(
        "{}/{}/{}",
        rec["os"].as_str().unwrap_or("?"),
        rec["app"].as_str().unwrap_or("?"),
        rec["fixture"].as_str().unwrap_or("?")
    )
}

/// Gate failures: each expected metric that fails on an available cell (or is unknown). Cells
/// that did not run on this machine (another OS, app missing or filtered out) are skipped.
pub fn gate(expected: &Value, records: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(map) = expected.as_object() else {
        return vec!["expected.json is not a JSON object".into()];
    };
    for (key, metrics) in map {
        let Some(rec) = records.iter().find(|r| cell_key(r) == *key) else {
            continue;
        };
        if rec["available"].as_bool() != Some(true) {
            continue;
        }
        let Some(list) = metrics.as_array() else {
            out.push(format!("{key}: expected a list of metric names"));
            continue;
        };
        for m in list {
            let name = m.as_str().unwrap_or_default();
            match metric_passes(rec, name) {
                Some(true) => {}
                Some(false) => out.push(format!(
                    "{key}: {name} failed{}",
                    rec["error"]
                        .as_str()
                        .map(|e| format!(" (cell error: {e})"))
                        .unwrap_or_default()
                )),
                None => out.push(format!("{key}: unknown metric {name:?}")),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_ids_and_long_text() {
        let ids: Vec<&str> = payloads().iter().map(|(i, _)| *i).collect();
        assert_eq!(
            ids,
            ["ascii", "newlines", "emoji", "rtl", "combining", "long"]
        );
        let l = long_text();
        assert_eq!(l, long_text(), "deterministic");
        assert!((4500..=5000).contains(&l.len()), "{}", l.len());
        assert!(l.is_ascii());
        assert!(l.matches('\n').count() >= 5);
        assert!(!l.contains("\n\n") && !l.ends_with('\n') && l.ends_with('.'));
        assert!(!l.contains("  "));
    }

    #[test]
    fn normalisation() {
        assert_eq!(norm("a\r\nb\rc\u{a0}d\n\n"), "a\nb\nc d");
        assert_eq!(norm("Draft: "), "Draft: ");
        assert_eq!(ws("  a\u{a0} b\n\tc "), "a b c");
        assert_eq!(trunc("héllo", 3), "hél…");
        assert_eq!(trunc("hi", 3), "hi");
    }

    #[test]
    fn diffs() {
        assert_eq!(diff("abc", "abc"), None);
        let d = diff("Draft: hello", "Draft: help").unwrap();
        assert_eq!(d["index"], 10);
        assert_eq!(d["expected_len"], 12);
        let d = diff("Café", "Cafe").unwrap();
        assert_eq!(d["index"], 3);
    }

    fn t(text: &str, from_me: bool) -> TruthMessage {
        TruthMessage {
            text: text.into(),
            from_me,
        }
    }

    fn g(text: &str, from_me: Option<bool>) -> GotMessage {
        GotMessage {
            text: text.into(),
            from_me,
        }
    }

    #[test]
    fn thread_scores() {
        let truth = [
            t("Are we still on for Thursday?", false),
            t("Yes, 3pm works for me.", true),
            t("Great, see you then", false),
        ];
        // All found, one with its author glued on, one decoy.
        let got = [
            g("Maya Chen: Are we still on for  Thursday?", Some(false)),
            g("Yes, 3pm works for me.", Some(true)),
            g("Great, see you then", None),
            g("Sidebar preview text", Some(false)),
        ];
        let s = score_thread(&got, &truth);
        assert_eq!(s["matched"], 3);
        assert_eq!(s["precision"], 0.75);
        assert_eq!(s["recall"], 1.0);
        assert_eq!(s["from_me_ok"], false);
        assert_eq!(s["from_me_accuracy"], 0.667);
        // Each truth message is matched at most once.
        let dup = [
            g("Yes, 3pm works for me.", Some(true)),
            g("Yes, 3pm works for me.", Some(true)),
        ];
        let s = score_thread(&dup, &truth);
        assert_eq!(s["matched"], 1);
        assert_eq!(s["precision"], 0.5);
        // Short fragments do not match by containment.
        let s = score_thread(&[g("Yes", None)], &truth);
        assert_eq!(s["matched"], 0);
        // Nothing reported: precision undefined, recall 0.
        let s = score_thread(&[], &truth);
        assert_eq!(s["precision"], Value::Null);
        assert_eq!(s["recall"], 0.0);
        assert_eq!(s["from_me_ok"], Value::Null);
    }

    #[test]
    fn recipients_and_subject() {
        let truth = vec!["Jordan Lee".to_string()];
        assert_eq!(
            recipients_ok(&["jordan lee <j@x>".into()], &truth),
            Some(true)
        );
        assert_eq!(recipients_ok(&[], &truth), Some(false));
        assert_eq!(recipients_ok(&[], &[]), None);
        assert_eq!(
            subject_ok(Some("Re: Q3 offsite venue"), Some("Q3 offsite venue")),
            Some(true)
        );
        assert_eq!(subject_ok(None, Some("Q3 offsite venue")), Some(false));
        assert_eq!(subject_ok(Some("x"), None), None);
    }

    #[test]
    fn medians() {
        assert_eq!(
            median_max(&mut [5.0, 1.0, 3.0]),
            json!({"median": 3.0, "max": 5.0})
        );
        assert_eq!(
            median_max(&mut [4.0, 1.0]),
            json!({"median": 2.5, "max": 4.0})
        );
        assert_eq!(median_max(&mut []), Value::Null);
    }

    fn record() -> Value {
        json!({
            "os": "linux", "app": "firefox", "fixture": "quill", "available": true,
            "detect": true, "read_ok": false, "secure_ok": true,
            "thread": {"precision": 1.0, "recall": 0.75, "from_me_ok": true,
                       "recipients_ok": null, "subject_ok": null},
            "inserts": [
                {"payload": "ascii", "verified": true, "exact": true, "undo_one_step": true},
                {"payload": "emoji", "verified": false, "exact": false, "undo_one_step": null}
            ],
            "read_ms": {"median": 120.0, "max": 300.0}
        })
    }

    #[test]
    fn metrics() {
        let r = record();
        let pass = |m: &str| metric_passes(&r, m);
        assert_eq!(pass("detect"), Some(true));
        assert_eq!(pass("no_error"), Some(true));
        assert_eq!(pass("read"), Some(false));
        assert_eq!(pass("secure"), Some(true));
        assert_eq!(pass("thread_precision"), Some(true));
        assert_eq!(pass("thread_recall"), Some(false));
        assert_eq!(pass("thread_recall>=0.7"), Some(true));
        assert_eq!(pass("from_me"), Some(true));
        assert_eq!(pass("subject"), Some(false));
        assert_eq!(pass("insert:ascii"), Some(true));
        assert_eq!(pass("undo:ascii"), Some(true));
        assert_eq!(pass("insert:emoji"), Some(false));
        assert_eq!(pass("insert:rtl"), Some(false), "not run is not a pass");
        assert_eq!(pass("insert:*"), Some(false));
        assert_eq!(pass("read_ms<=150"), Some(true));
        assert_eq!(pass("read_ms<=100"), Some(false));
        assert_eq!(pass("read_ms"), None);
        assert_eq!(pass("insert:nope"), None);
        assert_eq!(pass("bogus"), None);
        let mut err = r.clone();
        err["error"] = json!("boom");
        assert_eq!(metric_passes(&err, "no_error"), Some(false));
        let mut na = r.clone();
        na["available"] = json!(false);
        assert_eq!(metric_passes(&na, "detect"), Some(false));
    }

    #[test]
    fn gate_reports_only_expected_failures() {
        let recs = vec![
            record(),
            json!({"os": "linux", "app": "chrome", "fixture": "*", "available": false}),
        ];
        assert!(gate(&json!({}), &recs).is_empty());
        let exp = json!({
            "linux/firefox/quill": ["detect", "insert:ascii", "read", "wat"],
            "linux/chrome/*": ["detect"],
            "macos/safari/quill": ["detect"]
        });
        let f = gate(&exp, &recs);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f[0].contains("read failed"));
        assert!(f[1].contains("unknown metric"));
        assert_eq!(gate(&json!([]), &recs).len(), 1);
        assert_eq!(cell_key(&recs[0]), "linux/firefox/quill");
    }
}
