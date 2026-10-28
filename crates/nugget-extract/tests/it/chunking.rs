//! Chunker invariants: coverage, offsets, heading prefix, max size, overlap, determinism.

use nugget_extract::{ChunkOptions, ChunkOut, Extracted, Section, chunk, estimate_tokens};

use crate::common::*;

/// Check every documented guarantee of `chunk` for `doc` under `opts`.
pub fn check(doc: &Extracted, opts: &ChunkOptions) -> Vec<ChunkOut> {
    let chunks = chunk(doc, opts);
    let max = opts.max_tokens.max(1);
    let overlap_cap = opts.overlap_tokens.min(max / 2);
    let text = doc.text.as_str();
    let mut covered = vec![false; text.len()];
    for c in &chunks {
        assert!(c.start < c.end && c.end <= text.len(), "bad range {c:?}");
        assert!(text.is_char_boundary(c.start) && text.is_char_boundary(c.end));
        let body = &text[c.start..c.end];
        assert_eq!(body, body.trim(), "chunk bodies are trimmed");
        assert!(c.text.ends_with(body), "text is prefix + body: {c:?}");
        let prefix = &c.text[..c.text.len() - body.len()];
        if c.heading_path.is_empty() {
            assert!(prefix.is_empty(), "no prefix outside sections: {c:?}");
        } else if !prefix.is_empty() {
            assert!(prefix.ends_with('\n'));
            let full = c.heading_path.join(" > ");
            assert!(
                full.starts_with(prefix.trim_end()),
                "{prefix:?} vs {full:?}"
            );
        }
        assert_eq!(c.tokens, estimate_tokens(&c.text));
        assert!(c.tokens <= max, "{} tokens > max {max}: {c:?}", c.tokens);
        covered[c.start..c.end].iter_mut().for_each(|b| *b = true);
    }
    for (i, ch) in text.char_indices() {
        assert!(
            ch.is_whitespace() || covered[i],
            "byte {i} ({ch:?}) not covered"
        );
    }
    for w in chunks.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(b.start >= a.start && b.end > a.end, "chunks are ordered");
        if b.start < a.end {
            assert_eq!(a.heading_path, b.heading_path, "no overlap across sections");
            let ov = estimate_tokens(&text[b.start..a.end]);
            assert!(ov <= overlap_cap, "overlap {ov} > {overlap_cap}");
            assert!(b.start > a.start, "overlap never repeats a whole chunk");
        }
    }
    assert_eq!(chunk(doc, opts), chunks, "deterministic");
    chunks
}

fn option_sets() -> Vec<ChunkOptions> {
    let o = |target_tokens, max_tokens, overlap_tokens| ChunkOptions {
        target_tokens,
        max_tokens,
        overlap_tokens,
    };
    vec![
        ChunkOptions::default(),
        o(50, 64, 10),
        o(8, 10, 3),
        o(1, 1, 0),
        o(0, 0, 0),
        o(100, 50, 200),
        o(30, 40, 40),
        o(200, 256, 0),
    ]
}

fn synthetic() -> Extracted {
    let mut text = String::new();
    let mut sections = Vec::new();
    let mut add = |path: &[&str], body: &str, text: &mut String| {
        let start = text.len();
        text.push_str(body);
        sections.push(Section {
            heading_path: path.iter().map(|s| s.to_string()).collect(),
            start,
            end: text.len(),
        });
        text.push_str("\n\n");
    };
    text.push_str("Preamble before any heading.\n\n");
    let sentence = "The quick brown fox jumps over the lazy dog near the riverbank. ";
    add(
        &["Guide"],
        &format!("Guide\n\n{}\n\n{}", sentence.repeat(30), sentence.repeat(3)),
        &mut text,
    );
    add(
        &["Guide", "Install"],
        &format!(
            "Install\n\n{}\n{}",
            "a".repeat(5_000),
            "line two\nline three"
        ),
        &mut text,
    );
    add(
        &["Guide", "日本語"],
        &"日本語のテキストです。とても長い文章が続きます。".repeat(120),
        &mut text,
    );
    add(
        &["Table"],
        &(0..300)
            .map(|i| format!("row {i} | value {i} | https://example.com/{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
        &mut text,
    );
    text.push_str("Trailing text outside sections.");
    Extracted {
        text,
        sections,
        ..Default::default()
    }
}

#[test]
fn invariants_hold_for_many_documents_and_options() {
    let note = run("text/markdown", &fixture("note.md"));
    let html = run("text/html", &fixture("page.html"));
    let ics = run("text/calendar", &fixture("meeting.ics"));
    let messy = Extracted {
        text: "héllo wörld. ünïcode\n\nmore text here 日本".into(),
        sections: vec![
            Section {
                heading_path: vec!["A".into()],
                start: 2, // inside 'é'
                end: 9,
            },
            Section {
                heading_path: vec!["B".into(), "C".into()],
                start: 5,
                end: 500,
            },
            Section {
                heading_path: vec![],
                start: 30,
                end: 10,
            },
            Section {
                heading_path: vec!["".into()],
                start: 0,
                end: 4,
            },
        ],
        ..Default::default()
    };
    let docs = [note, html, ics, synthetic(), messy, Extracted::default()];
    for doc in &docs {
        for opts in option_sets() {
            check(doc, &opts);
        }
    }
}

#[test]
fn empty_and_whitespace_documents_have_no_chunks() {
    for text in ["", "   \n\n\t  "] {
        let doc = Extracted {
            text: text.into(),
            ..Default::default()
        };
        assert!(chunk(&doc, &ChunkOptions::default()).is_empty());
    }
}

#[test]
fn heading_prefix() {
    let doc = run("text/markdown", &fixture("note.md"));
    let chunks = check(&doc, &ChunkOptions::default());
    let install = chunks
        .iter()
        .find(|c| c.heading_path == ["Guide", "Install"])
        .expect("install chunk");
    assert!(
        install.text.starts_with("Guide\n## Install"),
        "own heading is not repeated in the prefix: {:?}",
        install.text
    );
    assert!(
        !install.text.contains("Install\n## Install"),
        "{:?}",
        install.text
    );
    let small = Extracted {
        text: "Hello world.".into(),
        ..Default::default()
    };
    let chunks = check(&small, &ChunkOptions::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "Hello world.");
    assert_eq!((chunks[0].start, chunks[0].end), (0, 12));
    assert_eq!(chunks[0].tokens, 3);
}

#[test]
fn prefers_paragraph_boundaries_and_overlaps() {
    let para = |n: usize| format!("Paragraph {n} talks about topic {n}. ").repeat(4);
    let text = format!("{}\n\n{}\n\n{}", para(1), para(2), para(3));
    let doc = Extracted {
        text: text.clone(),
        ..Default::default()
    };
    let p2_end = text.find(&para(3)).unwrap() - 2;
    let opts = ChunkOptions {
        target_tokens: estimate_tokens(&text[..p2_end]) + 2,
        max_tokens: estimate_tokens(&text[..p2_end]) + 10,
        overlap_tokens: 6,
    };
    let chunks = check(&doc, &opts);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].end, text[..p2_end].trim_end().len());
    assert!(
        chunks[1].start < chunks[0].end,
        "second chunk overlaps the first"
    );
    assert!(text[chunks[1].start..].starts_with(|c: char| c.is_alphanumeric()));
    assert!(chunks[1].text.contains("Paragraph 3"));
}

#[test]
fn long_runs_are_hard_split() {
    let doc = Extracted {
        text: "x".repeat(10_000),
        ..Default::default()
    };
    let opts = ChunkOptions {
        target_tokens: 100,
        max_tokens: 128,
        overlap_tokens: 16,
    };
    let chunks = check(&doc, &opts);
    assert!(chunks.len() >= 10_000 / (128 * 4));
    let cjk = Extracted {
        text: "漢".repeat(1_000),
        ..Default::default()
    };
    let chunks = check(&cjk, &opts);
    assert!(chunks.iter().all(|c| c.tokens <= 128));
    assert!(chunks.len() >= 1_000 / 128);
}

#[test]
fn token_estimate() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("hello world"), 3);
    assert_eq!(estimate_tokens("日本語"), 3);
    assert_eq!(estimate_tokens("한국어 텍스트"), 7);
    assert_eq!(estimate_tokens("ab日本"), 3);
}

#[test]
fn large_document_is_fast_enough() {
    let doc = Extracted {
        text: "Some sentence with several words in it. ".repeat(15_000),
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let chunks = check(&doc, &ChunkOptions::default());
    assert!(!chunks.is_empty());
    assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
}
