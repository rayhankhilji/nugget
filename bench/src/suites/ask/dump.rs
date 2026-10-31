//! `--ask-dump`: one JSON line per (question, arm), written and flushed as soon as that answer
//! is scored, so a run that is killed keeps every finished answer. Each line holds everything
//! scoring needs (the full packed context, raw and filtered text, the product's check, timings,
//! server stats and the failure). The judge runs after all answers and appends each verdict, as
//! soon as it has it, to a sidecar file next to the dump ([`verdict_path`]: `<dump>.judge.jsonl`).
//! `--ask-rescore` ([`read`], [`read_verdicts`]) merges the two and recomputes every metric
//! without a model, so a run killed while judging still reports every answer metric plus the
//! judge on what it reached.

use std::io::{BufRead, Write};

use nugget_core::ipc::{AnswerCheck, ContextResponse};
use serde_json::{Value, json};

use super::Arm;
use super::gold::Question;
use super::judge::{JudgeOutcome, Judgement};
use super::metrics::Row;
use super::stream::AnswerRun;

/// Dump format version (`v` in every line).
pub const DUMP_VERSION: u32 = 2;

/// An open dump file.
pub struct Dump {
    file: std::io::BufWriter<std::fs::File>,
}

impl Dump {
    pub fn create(path: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            file: std::io::BufWriter::new(std::fs::File::create(path)?),
        })
    }

    /// Write one row and flush.
    pub fn write(
        &mut self,
        q: &Question,
        ctx: &ContextResponse,
        row: &Row,
        timeout_s: u64,
    ) -> std::io::Result<()> {
        writeln!(self.file, "{}", line(q, ctx, row, timeout_s))?;
        self.file.flush()
    }
}

/// The JSON line of one row.
pub fn line(q: &Question, ctx: &ContextResponse, row: &Row, timeout_s: u64) -> Value {
    let run = &row.run;
    let mut line = json!({
        "v": DUMP_VERSION,
        "question_id": q.id,
        "question": q.question,
        "kind": q.kind.as_str(),
        "arm": row.arm.name,
        "reference": q.answer,
        "context": ctx,
        "raw": run.raw,
        "text": run.text,
        "check": run.check,
        "ttft_s": run.ttft.map(|d| d.as_secs_f64()),
        "total_s": run.total.as_secs_f64(),
        "timeout_s": timeout_s,
        "input_tokens": run.input_tokens,
        "cached_tokens": run.cached_tokens,
        "output_tokens": run.output_tokens,
        "stop_reason": run.stop_reason,
    });
    if let Some(s) = &row.score {
        line["scores"] = json!({
            "correct": s.correct, "abstained": s.abstained, "must_ok": s.must_ok,
            "stale": s.stale, "canary": s.canary, "complied": s.complied,
            "cited_pairs": s.cited_pairs, "supported_pairs": s.supported_pairs,
            "support_in_context": s.support_in_context, "support_cited": s.support_cited,
            "content_sentences": s.content_sentences, "uncited_sentences": s.uncited_sentences,
            "raw_markers": s.raw_markers, "raw_canonical": s.raw_canonical,
            "invalid_markers": s.invalid_markers,
        });
    }
    if let Some((kind, message)) = &run.failure {
        line["error"] = json!({ "kind": kind, "message": message });
    }
    if let Some(j) = &row.judge {
        line["judge"] = judge_json(j);
    }
    line
}

/// The judge's sidecar of a dump: `<dump>.judge.jsonl`.
pub fn verdict_path(dump: &std::path::Path) -> std::path::PathBuf {
    let mut s = dump.as_os_str().to_owned();
    s.push(".judge.jsonl");
    s.into()
}

/// The judge's sidecar file, one verdict per line, flushed as each arrives.
pub struct VerdictLog {
    file: std::io::BufWriter<std::fs::File>,
}

impl VerdictLog {
    pub fn create(path: &std::path::Path) -> std::io::Result<Self> {
        Ok(Self {
            file: std::io::BufWriter::new(std::fs::File::create(path)?),
        })
    }

    /// Append one verdict and flush.
    pub fn write(&mut self, question_id: &str, arm: Arm, j: &Judgement) -> std::io::Result<()> {
        let line = json!({
            "v": DUMP_VERSION,
            "question_id": question_id,
            "arm": arm.name,
            "judge": judge_json(j),
        });
        writeln!(self.file, "{line}")?;
        self.file.flush()
    }
}

fn judge_json(j: &Judgement) -> Value {
    json!({
        "outcome": j.outcome.as_str(),
        "reference_first": j.reference_first,
        "candidate_first": j.candidate_first,
    })
}

fn judgement_of(j: &Value) -> Judgement {
    Judgement {
        outcome: outcome(j.get("outcome").and_then(Value::as_str).unwrap_or("")),
        reference_first: j.get("reference_first").and_then(Value::as_bool),
        candidate_first: j.get("candidate_first").and_then(Value::as_bool),
    }
}

/// Non-empty lines of a JSONL file as JSON values; a final line cut off by a killed run is
/// skipped (and counted), any other bad line is an error.
fn read_jsonl(path: &std::path::Path) -> Result<(Vec<Value>, usize), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let lines: Vec<String> = std::io::BufReader::new(f)
        .lines()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let lines: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut skipped = 0;
    for (i, l) in lines.iter().enumerate() {
        match serde_json::from_str::<Value>(l) {
            Ok(v) => out.push(v),
            Err(_) if i + 1 == lines.len() => skipped += 1,
            Err(e) => return Err(format!("{} line {}: {e}", path.display(), i + 1)),
        }
    }
    Ok((out, skipped))
}

/// Judge verdicts by (question id, arm name).
pub type Verdicts = std::collections::HashMap<(String, String), Judgement>;

/// The verdicts of a sidecar, and how many cut-off lines were skipped.
pub fn read_verdicts(path: &std::path::Path) -> Result<(Verdicts, usize), String> {
    let (values, skipped) = read_jsonl(path)?;
    let mut out = std::collections::HashMap::new();
    for v in values {
        let (Some(q), Some(arm), Some(j)) = (
            v.get("question_id").and_then(Value::as_str),
            v.get("arm").and_then(Value::as_str),
            v.get("judge"),
        ) else {
            return Err(format!(
                "{}: a line without question_id, arm or judge",
                path.display()
            ));
        };
        out.insert((q.to_owned(), arm.to_owned()), judgement_of(j));
    }
    Ok((out, skipped))
}

/// One row read back from a dump.
#[derive(Debug, Clone)]
pub struct DumpRow {
    pub question_id: String,
    pub arm: Arm,
    pub context: ContextResponse,
    pub run: AnswerRun,
    pub judge: Option<Judgement>,
    pub timeout_s: Option<u64>,
}

fn failure_kind(kind: &str) -> &'static str {
    match kind {
        "timeout" => "timeout",
        "context_overflow" => "context_overflow",
        _ => "error",
    }
}

fn outcome(s: &str) -> JudgeOutcome {
    match s {
        "correct" => JudgeOutcome::Agreed(true),
        "incorrect" => JudgeOutcome::Agreed(false),
        "inconsistent" => JudgeOutcome::Inconsistent,
        "skipped" => JudgeOutcome::Skipped,
        "timeout" => JudgeOutcome::Failed("timeout"),
        "error" => JudgeOutcome::Failed("error"),
        _ => JudgeOutcome::Failed("unparseable"),
    }
}

/// Parse one dump line.
pub fn parse_line(v: &Value) -> Result<DumpRow, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    let u32_of = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as u32);
    let secs = |k: &str| {
        v.get(k)
            .and_then(Value::as_f64)
            .filter(|x| x.is_finite() && *x >= 0.0)
            .map(std::time::Duration::from_secs_f64)
    };
    let question_id = s("question_id").ok_or("no question_id")?.to_owned();
    let arm = match s("arm") {
        Some("plain") => Arm::PLAIN,
        Some("grounded") => Arm::GROUNDED,
        other => return Err(format!("{question_id}: unknown arm {other:?}")),
    };
    let context: ContextResponse = serde_json::from_value(
        v.get("context")
            .cloned()
            .ok_or_else(|| format!("{question_id}: no context"))?,
    )
    .map_err(|e| {
        format!("{question_id}: context is not a full packed context (dump v{DUMP_VERSION}): {e}")
    })?;
    let check: Option<AnswerCheck> = match v.get("check") {
        None | Some(Value::Null) => None,
        Some(c) => Some(
            serde_json::from_value(c.clone()).map_err(|e| format!("{question_id}: check: {e}"))?,
        ),
    };
    let failure = v.get("error").map(|e| {
        (
            failure_kind(e.get("kind").and_then(Value::as_str).unwrap_or("error")),
            e.get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    });
    let run = AnswerRun {
        raw: s("raw").unwrap_or_default().to_owned(),
        text: s("text").unwrap_or_default().to_owned(),
        check,
        ttft: secs("ttft_s"),
        total: secs("total_s").unwrap_or_default(),
        input_tokens: u32_of("input_tokens"),
        output_tokens: u32_of("output_tokens"),
        cached_tokens: u32_of("cached_tokens"),
        stop_reason: s("stop_reason").map(str::to_owned),
        failure,
    };
    let judge = v.get("judge").map(judgement_of);
    Ok(DumpRow {
        question_id,
        arm,
        context,
        run,
        judge,
        timeout_s: v.get("timeout_s").and_then(Value::as_u64),
    })
}

/// Read a dump (see [`read_jsonl`] for a cut-off last line).
pub fn read(path: &std::path::Path) -> Result<(Vec<DumpRow>, usize), String> {
    let (values, skipped) = read_jsonl(path)?;
    let rows = values
        .iter()
        .enumerate()
        .map(|(i, v)| parse_line(v).map_err(|e| format!("{} line {}: {e}", path.display(), i + 1)))
        .collect::<Result<_, _>>()?;
    Ok((rows, skipped))
}
