//! Memory and time of the voice profile over a synthetic corpus, one phase per process so each
//! measurement starts from a fresh heap:
//!
//! ```text
//! cargo run --release -p nugget-voice --example voice_memory -- seed  <dir> [items]
//! cargo run --release -p nugget-voice --example voice_memory -- build <dir>   # first start
//! cargo run --release -p nugget-voice --example voice_memory -- load  <dir>   # restart
//! ```
//!
//! `seed` writes an encrypted store shaped like the `perf --backend real` daemon profile:
//! Markdown notes of ~120 words from a 20k-word Zipf-like vocabulary (one chunk each, no item
//! text), plus every 10th item a sent mail to one of 50 recipients. `build` runs
//! `VoiceProfile::load_or_build` on an empty snapshot (and saves it); `load` runs it again (the
//! daemon's restart path). Both report the time, the resident set size held after the call
//! (`VmRSS` minus the RSS of the open store before it) and the peak (`VmHWM`), then time a
//! few drafts' worth of hints. RSS comes from `/proc/self/status` (Linux); elsewhere only
//! times are printed.

use std::path::Path;
use std::time::Instant;

use nugget_core::ids::SourceId;
use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};
use nugget_voice::{
    Channel, MeIdentity, VOICE_SNAPSHOT_SOURCE, VoiceConfig, VoiceProfile, VoiceQuery, VoiceSource,
};
use serde_json::json;

const SYLLABLES: [&str; 16] = [
    "ka", "lo", "mi", "ne", "ru", "sa", "ti", "vo", "ba", "de", "fi", "go", "hu", "ja", "ke", "pu",
];

fn word(i: usize) -> String {
    let mut n = i;
    let mut s = String::new();
    let mut len = 0;
    loop {
        s.push_str(SYLLABLES[n % 16]);
        n /= 16;
        len += 1;
        if n == 0 && len >= 2 {
            return s;
        }
    }
}

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const VOCAB: usize = 20_000;

fn sentence(rng: &mut Rng, words: usize) -> String {
    (0..words)
        .map(|_| {
            // Log-uniform rank: a few words are very common.
            let idx = (VOCAB as f64).powf(rng.unit()) as usize;
            word(idx.saturating_sub(1).min(VOCAB - 1))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_kib(field: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with(field))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn mib(kib: Option<u64>) -> String {
    kib.map_or_else(|| "n/a".into(), |k| format!("{:.1}", k as f64 / 1024.0))
}

fn open(dir: &Path) -> Store {
    Store::open(dir.join("store.db"), &StoreKey::from_bytes([7; 32])).expect("open store")
}

fn seed(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("create dir");
    let store = open(dir);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let started = Instant::now();
    for i in 0..n {
        let (item, text) = if i % 10 == 9 {
            let to = format!("Person {} <p{}@example.com>", i % 50, i % 50);
            let body = format!("Hi Person,\n\n{}.\n\nThanks,\nAlex", sentence(&mut rng, 40));
            let meta = json!({
                "source": {"labels": ["SENT"], "from": "Alex Morgan <alex@example.com>"},
                "mime": "message/rfc822",
                "extracted": {"from": "Alex Morgan <alex@example.com>", "to": [to],
                              "date": "2026-09-01T10:00:00Z",
                              "message_id": format!("<m{i}@example.com>")},
            });
            let item = NewItem::new("gmail:alex", format!("m{i:06}"), content_hash(&body))
                .with_version("v0")
                .with_mime("message/rfc822")
                .with_metadata(meta);
            (item, body)
        } else {
            let text = format!(
                "{}\n{}\nmk{i}v0",
                sentence(&mut rng, 3),
                sentence(&mut rng, 120)
            );
            let item = NewItem::new("synthetic", format!("doc-{i:06}"), content_hash(&text))
                .with_title(format!("Doc {i}"))
                .with_version("v0")
                .with_mime("text/markdown")
                .with_metadata(json!({"tags": [format!("topic{}", rng.below(40))]}));
            (item, text)
        };
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
            .expect("upsert");
    }
    println!(
        "seeded {n} items in {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

fn run(dir: &Path, phase: &str) {
    let store = open(dir);
    // Touch the store so its own caches are part of the baseline. With `VOICE_MEMORY_WARM=1`,
    // also fill the read connection's page cache (32 MiB) by reading the snapshot once, so
    // the numbers below are the profile's alone.
    let _ = store.source_counts().expect("source counts");
    if std::env::var_os("VOICE_MEMORY_WARM").is_some() {
        let snap = store
            .get_cursor_json(&SourceId::new(VOICE_SNAPSHOT_SOURCE))
            .expect("read snapshot");
        println!(
            "{phase}: snapshot {:.1} MiB",
            snap.map_or(0, |s| s.len()) as f64 / (1024.0 * 1024.0)
        );
    }
    if phase == "build" {
        store
            .clear_cursor(&SourceId::new(VOICE_SNAPSHOT_SOURCE))
            .expect("clear snapshot");
    }
    let base = status_kib("VmRSS:");
    let started = Instant::now();
    let profile =
        VoiceProfile::load_or_build(&store, MeIdentity::default(), VoiceConfig::default())
            .expect("load_or_build");
    let took = started.elapsed();
    let rss = status_kib("VmRSS:");
    let held = match (rss, base) {
        (Some(r), Some(b)) => Some(r.saturating_sub(b)),
        _ => None,
    };
    let report = profile.last_refresh();
    println!(
        "{phase}: {:.2} s, samples {}, fetched {}, RSS {} MiB (held by the profile: {} MiB, \
         estimated heap {:.1} MiB), peak {} MiB",
        took.as_secs_f64(),
        profile.status().samples,
        report.fetched,
        mib(rss),
        mib(held),
        profile.approx_heap_bytes() as f64 / (1024.0 * 1024.0),
        mib(status_kib("VmHWM:")),
    );
    let mut rng = Rng(42);
    if std::env::var_os("VOICE_MEMORY_SEARCH").is_some() {
        let started = Instant::now();
        for _ in 0..10 {
            let q = sentence(&mut rng, 24);
            let _ = store.search_hybrid_with_depth(&q, None, 200, 200);
        }
        println!(
            "{phase}: store search alone {:.1} ms",
            started.elapsed().as_secs_f64() * 100.0
        );
        rng = Rng(42);
    }
    let started = Instant::now();
    let mut exemplars = 0;
    let queries = 10;
    for i in 0..queries {
        let channel = if i % 2 == 0 {
            Channel::Document
        } else {
            Channel::Email
        };
        let q = VoiceQuery::new(channel)
            .with_recipients([format!("p{}@example.com", i % 50)])
            .with_thread(sentence(&mut rng, 60));
        exemplars += VoiceSource::hints(&profile, &q).exemplars.len();
    }
    println!(
        "{phase}: hints {:.1} ms per draft ({exemplars} exemplars over {queries} drafts), RSS {} MiB",
        started.elapsed().as_secs_f64() * 1000.0 / queries as f64,
        mib(status_kib("VmRSS:")),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: voice_memory seed <dir> [items] | build <dir> | load <dir>";
    let (Some(phase), Some(dir)) = (args.first(), args.get(1)) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let dir = Path::new(dir);
    match phase.as_str() {
        "seed" => seed(
            dir,
            args.get(2).and_then(|n| n.parse().ok()).unwrap_or(100_000),
        ),
        "build" | "load" => run(dir, phase),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}
