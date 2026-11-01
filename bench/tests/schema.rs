//! Keeps `bench/schema/report.schema.json` and the Rust report types in sync, and checks real
//! reports from every registered suite against the schema.
//!
//! The validator below implements exactly the JSON Schema subset the schema file uses. It
//! panics on any keyword it does not know, so extending the schema forces this test to be
//! extended too.

#[cfg(target_os = "linux")]
use nugget_bench::Backend;
use nugget_bench::report::{Comparison, Direction, SCHEMA_JSON};
use nugget_bench::runner::valid_metric_name;
use nugget_bench::{
    Report, RunConfig, SCHEMA_VERSION, Scale, Status, find, registry, run_benchmark,
};
use serde_json::{Value, json};

fn schema() -> Value {
    serde_json::from_str(SCHEMA_JSON).expect("schema is valid JSON")
}

fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let name = r.strip_prefix("#/$defs/").expect("only local $defs refs");
            &root["$defs"][name]
        }
        None => node,
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        other => panic!("unsupported type {other}"),
    }
}

fn pattern_ok(pattern: &str, s: &str) -> bool {
    match pattern {
        "^[a-z][a-z0-9_]*$" => {
            s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        }
        "^[a-z][a-z0-9_@]*(\\.[a-z0-9_@]+)*$" => valid_metric_name(s),
        other => panic!("pattern {other} has no test implementation"),
    }
}

fn validate(root: &Value, node: &Value, v: &Value, path: &str, errors: &mut Vec<String>) {
    let node = resolve(root, node);
    let obj = node.as_object().expect("schema nodes are objects");
    for (kw, arg) in obj {
        match kw.as_str() {
            "$schema" | "$id" | "title" | "description" | "$defs" | "$ref" | "format" => {}
            "type" => {
                let ok = match arg {
                    Value::String(t) => type_ok(t, v),
                    Value::Array(ts) => ts.iter().any(|t| type_ok(t.as_str().unwrap(), v)),
                    _ => panic!("bad type keyword"),
                };
                if !ok {
                    errors.push(format!("{path}: expected type {arg}, got {v}"));
                }
            }
            "const" => {
                if arg != v {
                    errors.push(format!("{path}: expected const {arg}, got {v}"));
                }
            }
            "enum" => {
                if !arg.as_array().unwrap().contains(v) {
                    errors.push(format!("{path}: {v} not in {arg}"));
                }
            }
            "minimum" => {
                if v.as_f64().is_some_and(|x| x < arg.as_f64().unwrap()) {
                    errors.push(format!("{path}: {v} below minimum {arg}"));
                }
            }
            "minLength" => {
                if v.as_str()
                    .is_some_and(|s| (s.chars().count() as u64) < arg.as_u64().unwrap())
                {
                    errors.push(format!("{path}: string shorter than {arg}"));
                }
            }
            "pattern" => {
                if let Some(s) = v.as_str()
                    && !pattern_ok(arg.as_str().unwrap(), s)
                {
                    errors.push(format!("{path}: {s:?} does not match {arg}"));
                }
            }
            "required" => {
                if let Some(o) = v.as_object() {
                    for r in arg.as_array().unwrap() {
                        if !o.contains_key(r.as_str().unwrap()) {
                            errors.push(format!("{path}: missing required {r}"));
                        }
                    }
                }
            }
            "properties" => {
                if let Some(o) = v.as_object() {
                    for (k, sub) in arg.as_object().unwrap() {
                        if let Some(child) = o.get(k) {
                            validate(root, sub, child, &format!("{path}.{k}"), errors);
                        }
                    }
                }
            }
            "additionalProperties" => {
                assert_eq!(
                    arg,
                    &Value::Bool(false),
                    "only additionalProperties: false is supported"
                );
                if let Some(o) = v.as_object() {
                    let props = obj["properties"].as_object().unwrap();
                    for k in o.keys() {
                        if !props.contains_key(k) {
                            errors.push(format!("{path}: unexpected property {k}"));
                        }
                    }
                }
            }
            "items" => {
                if let Some(a) = v.as_array() {
                    for (i, item) in a.iter().enumerate() {
                        validate(root, arg, item, &format!("{path}[{i}]"), errors);
                    }
                }
            }
            other => panic!("schema keyword {other} is not supported by the test validator"),
        }
    }
}

fn errors_for(v: &Value) -> Vec<String> {
    let root = schema();
    let mut errors = Vec::new();
    validate(&root, &root, v, "$", &mut errors);
    errors
}

fn smoke_reports() -> Vec<Value> {
    let cfg = RunConfig {
        scale: Scale::Smoke,
        seed: 5,
        ..RunConfig::default()
    };
    registry()
        .iter()
        .map(|b| serde_json::to_value(run_benchmark(b.as_ref(), &cfg)).unwrap())
        .collect()
}

#[test]
fn schema_version_matches_code() {
    assert_eq!(
        schema()["properties"]["schema_version"]["const"],
        json!(SCHEMA_VERSION)
    );
}

#[test]
fn schema_enums_match_rust_enums() {
    let s = schema();
    let status = s["properties"]["status"]["enum"]
        .as_array()
        .unwrap()
        .clone();
    let from_rust: Vec<Value> = [Status::Ok, Status::NotImplemented, Status::Failed]
        .iter()
        .map(|x| serde_json::to_value(x).unwrap())
        .collect();
    assert_eq!(status, from_rust);

    let dir = s["$defs"]["metric"]["properties"]["direction"]["enum"]
        .as_array()
        .unwrap()
        .clone();
    let from_rust: Vec<Value> = [
        Direction::HigherIsBetter,
        Direction::LowerIsBetter,
        Direction::Info,
    ]
    .iter()
    .map(|x| serde_json::to_value(x).unwrap())
    .collect();
    assert_eq!(dir, from_rust);

    let scales: Vec<&str> = s["$defs"]["config"]["properties"]["scale"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for scale in [Scale::Smoke, Scale::Quick, Scale::Standard, Scale::Full] {
        assert!(scales.contains(&scale.as_str()));
    }

    let cmp = s["$defs"]["target"]["properties"]["comparison"]["enum"]
        .as_array()
        .unwrap()
        .clone();
    let from_rust: Vec<Value> = [Comparison::AtLeast, Comparison::AtMost]
        .iter()
        .map(|x| serde_json::to_value(x).unwrap())
        .collect();
    assert_eq!(cmp, from_rust);
}

/// Fields that may be absent (added after version 1 shipped).
const OPTIONAL: &[&str] = &["targets"];

fn check_keys(r: &Value) {
    let root = schema();
    let mut required: Vec<&str> = root["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort_unstable();
    // Every serialised field is required except the optional additions, so optional values of
    // the original fields are explicit nulls.
    let mut keys: Vec<&str> = r
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|k| !OPTIONAL.contains(k))
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, required);
    for k in OPTIONAL {
        assert!(!required.contains(k), "{k} must stay optional");
        assert!(root["properties"].get(*k).is_some());
    }
}

#[test]
fn every_suite_report_conforms() {
    let reports = smoke_reports();
    assert_eq!(reports.len(), 9);
    for r in &reports {
        let errors = errors_for(r);
        assert!(errors.is_empty(), "{} report: {errors:#?}", r["suite"]);
        check_keys(r);
    }
    let status = |suite: &str| {
        reports
            .iter()
            .find(|r| r["suite"] == suite)
            .map(|r| r["status"].as_str().unwrap().to_owned())
            .unwrap()
    };
    assert_eq!(status("memory"), "ok");
    assert_eq!(status("perf"), "ok");
    assert_eq!(status("proactive"), "ok");
    assert_eq!(status("draft"), "ok");
    assert_eq!(status("cua"), "ok");
    // Daemon scenarios are skipped (not failed) without a `nuggetd` binary.
    assert_eq!(status("energy"), "ok");
    assert_eq!(status("ask"), "ok");
    // The real M2 pipeline with the default `pipeline` feature; `not_implemented` without it.
    let expected = if cfg!(feature = "pipeline") {
        "ok"
    } else {
        "not_implemented"
    };
    assert_eq!(status("knowledge"), expected);
    let knowledge = reports.iter().find(|r| r["suite"] == "knowledge").unwrap();
    assert!(knowledge["config"]["params"]["items"].as_u64().unwrap() >= 60);
    // Suites with targets list them.
    let memory = reports.iter().find(|r| r["suite"] == "memory").unwrap();
    let targets = memory["targets"].as_array().unwrap();
    assert!(targets.iter().any(|t| t["metric"] == "freshness.p95_s"));
}

#[test]
fn reports_from_before_targets_still_validate() {
    // A memory report as harness 0.0.1 wrote it in M0: no `targets`.
    let mut old = smoke_reports().remove(0);
    old.as_object_mut().unwrap().remove("targets");
    assert!(errors_for(&old).is_empty());
    let parsed: Report = serde_json::from_value(old).unwrap();
    assert!(parsed.targets.is_empty());
}

#[test]
fn bad_targets_are_rejected() {
    let good = smoke_reports().remove(0);
    assert!(errors_for(&good).is_empty());
    let mut cases = Vec::new();
    for (field, value) in [
        ("pass", json!("yes")),
        ("comparison", json!("equal")),
        ("metric", json!("Not A Metric")),
        ("value", json!(null)),
    ] {
        let mut v = good.clone();
        v["targets"][0][field] = value;
        cases.push(v);
    }
    let mut v = good.clone();
    v["targets"][0].as_object_mut().unwrap().remove("target");
    cases.push(v);
    let mut v = good;
    v["targets"][0]["extra"] = json!(1);
    cases.push(v);
    for (i, case) in cases.iter().enumerate() {
        assert!(!errors_for(case).is_empty(), "case {i} should fail");
    }
}

/// The real stack in process (encrypted store, ingest, files watcher, retriever) at smoke
/// scale: the report conforms, every file operation propagates, and freshness meets its target.
/// The recall gate is evaluated but not asserted here: at smoke scale (50 queries) it can tie the
/// baseline, and score ties break on chunk ids, which follow the directory listing order of the
/// file system. Linux only, like the CI step that runs it from the CLI.
#[cfg(target_os = "linux")]
#[test]
fn real_memory_backend_smoke() {
    let report = run_benchmark(
        find("memory").unwrap().as_ref(),
        &RunConfig {
            scale: Scale::Smoke,
            seed: 42,
            backend: Backend::Real,
            ..RunConfig::default()
        },
    );
    assert_eq!(report.status, Status::Ok, "{:?}", report.message);
    let v = serde_json::to_value(&report).unwrap();
    assert!(errors_for(&v).is_empty(), "{:#?}", errors_for(&v));
    check_keys(&v);
    assert_eq!(report.config.params["backend"], "real");
    let get = |n: &str| report.metric(n).unwrap_or_else(|| panic!("{n}")).value;
    for kind in ["edit", "insert", "delete", "rename"] {
        assert_eq!(get(&format!("propagation.{kind}_rate")), 1.0, "{kind}");
    }
    assert_eq!(get("propagation.final_keys_match"), 1.0);
    assert_eq!(get("propagation.final_content_match"), 1.0);
    assert!(get("freshness.p95_s") > 0.0);
    assert!(get("ingest.files_per_s") > 0.0);
    assert!(get("resources.db_bytes") > 0.0);
    assert_eq!(get("retrieval.hybrid.text_only_fallbacks"), 0.0);
    let check = |t: &str| report.targets.iter().find(|c| c.metric == t).cloned();
    assert!(check("freshness.p95_s").unwrap().pass);
    assert!(check("retrieval.eval.hybrid.recall@10").unwrap().pass);
    let recall = check("retrieval.hybrid.recall@10").expect("recall gate evaluated");
    assert_eq!(
        recall.target,
        get("baseline.fake.retrieval.hybrid.recall@10")
    );
}

#[test]
fn validator_rejects_bad_reports() {
    let good = smoke_reports().remove(1);
    assert!(errors_for(&good).is_empty());

    let mut cases: Vec<Value> = Vec::new();
    let mut v = good.clone();
    v["schema_version"] = json!(2);
    cases.push(v);
    let mut v = good.clone();
    v["status"] = json!("great");
    cases.push(v);
    let mut v = good.clone();
    v.as_object_mut().unwrap().remove("metrics");
    cases.push(v);
    let mut v = good.clone();
    v["extra"] = json!(true);
    cases.push(v);
    let mut v = good.clone();
    v["metrics"][0]["name"] = json!("Bad Name");
    cases.push(v);
    let mut v = good.clone();
    v["metrics"][0]["value"] = json!("fast");
    cases.push(v);
    let mut v = good.clone();
    v["environment"]["cpus"] = json!(0);
    cases.push(v);
    let mut v = good;
    v["config"]["scale"] = json!("galactic");
    cases.push(v);

    for (i, case) in cases.iter().enumerate() {
        assert!(!errors_for(case).is_empty(), "case {i} should fail");
    }
}

#[test]
fn reports_written_to_disk_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let report = run_benchmark(
        find("cua").unwrap().as_ref(),
        &RunConfig {
            scale: Scale::Smoke,
            seed: 1,
            ..RunConfig::default()
        },
    );
    let path = report.write_to_dir(dir.path()).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("cua-") && name.ends_with("Z.json"),
        "{name}"
    );
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(errors_for(&v).is_empty());
}
