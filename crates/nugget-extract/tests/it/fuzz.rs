//! Fuzz-style robustness: random, truncated and bit-flipped bytes through every extractor (and
//! every valid sample through every extractor). Nothing may panic, hang or break the output
//! contract. Set `NUGGET_FUZZ_ROUNDS` to run more rounds (default 1).

use std::time::Instant;

use nugget_core::Error;
use nugget_extract::{ChunkOptions, ExtractOptions, OcrModels, Source, extract_with_ocr};

use crate::chunking;
use crate::common::*;

/// xorshift64*: small, deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

const MIMES: &[&str] = &[
    "text/plain",
    "text/x-rust",
    "text/markdown",
    "text/html",
    "application/pdf",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "text/csv",
    "message/rfc822",
    "text/calendar",
    "image/png",
    "image/jpeg",
    "image/webp",
];

/// Extractors built on third-party parsers known to contain panicking paths; a panic there is
/// contained (and reported as `Error::Invalid`) rather than being a bug in this crate.
fn third_party_may_panic(mime: &str) -> bool {
    mime == "application/pdf" || mime.starts_with("image/")
}

fn samples() -> Vec<Vec<u8>> {
    vec![
        fixture("note.md"),
        fixture("page.html"),
        fixture("people.csv"),
        fixture("reply.eml"),
        fixture("html_only.eml"),
        fixture("meeting.ics"),
        make_pdf("Fuzz", &[&["alpha beta"], &["gamma"]]),
        make_docx(),
        make_xlsx(),
        make_png(),
        b"\xff\xd8\xff\xe0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0".to_vec(),
        b"RIFF\x1a\0\0\0WEBPVP8 \x0e\0\0\0\x30\x01\0\x9d\x01\x2a\x01\0\x01\0".to_vec(),
    ]
}

fn mutations(rng: &mut Rng, sample: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for _ in 0..6 {
        out.push(sample[..rng.below(sample.len() + 1)].to_vec());
    }
    for _ in 0..6 {
        let mut m = sample.to_vec();
        for _ in 0..1 + rng.below(8) {
            if !m.is_empty() {
                let i = rng.below(m.len());
                m[i] = rng.next() as u8;
            }
        }
        out.push(m);
    }
    // Splice random bytes into the middle.
    let mut m = sample.to_vec();
    let at = rng.below(m.len() + 1);
    let n = rng.below(64);
    m.splice(at..at, rng.bytes(n));
    out.push(m);
    out
}

#[test]
fn every_extractor_survives_hostile_bytes() {
    let rounds: u64 = std::env::var("NUGGET_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let dir = temp_dir("fuzz-ocr");
    let fake = OcrModels::from_dir(&dir);
    std::fs::write(&fake.detection, b"x").unwrap();
    std::fs::write(&fake.recognition, b"x").unwrap();
    let opts = ExtractOptions {
        max_input_bytes: 1024 * 1024,
        max_text_chars: 20_000,
        ocr: true,
    };
    let chunk_opts = ChunkOptions {
        target_tokens: 24,
        max_tokens: 32,
        overlap_tokens: 6,
    };
    let samples = samples();
    let started = Instant::now();
    let mut calls = 0usize;
    let mut contained = 0usize;
    for round in 0..rounds {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (round + 1));
        let mut inputs: Vec<Vec<u8>> = Vec::new();
        for s in &samples {
            inputs.push(s.clone());
            inputs.extend(mutations(&mut rng, s));
        }
        for _ in 0..24 {
            let n = rng.below(4096);
            let mut b = rng.bytes(n);
            // Sometimes start with a plausible signature to get past the first checks.
            let magic: &[u8] = match rng.below(8) {
                0 => b"%PDF-1.4\n",
                1 => b"PK\x03\x04",
                2 => b"BEGIN:VCALENDAR\n",
                3 => b"From: a@b.c\nSubject: s\n\n",
                4 => b"---\n",
                5 => b"\x89PNG\r\n\x1a\n",
                _ => b"",
            };
            b.splice(0..0, magic.iter().copied());
            inputs.push(b);
        }
        for mime in MIMES {
            for input in &inputs {
                calls += 1;
                match extract_with_ocr(mime, Source::Bytes(input), &opts, &fake) {
                    Ok(e) => {
                        assert!(e.text.chars().count() <= opts.max_text_chars, "{mime}");
                        assert!(e.metadata.is_object(), "{mime}");
                        for s in &e.sections {
                            assert!(s.start < s.end && s.end <= e.text.len(), "{mime}");
                            assert!(e.text.is_char_boundary(s.start), "{mime}");
                            assert!(e.text.is_char_boundary(s.end), "{mime}");
                        }
                        chunking::check(&e, &chunk_opts);
                    }
                    Err(Error::Invalid(msg)) if msg.contains("panicked") => {
                        assert!(
                            third_party_may_panic(mime),
                            "{mime} extractor panicked on input of {} bytes: {msg}",
                            input.len()
                        );
                        contained += 1;
                    }
                    Err(Error::Invalid(_) | Error::Unsupported(_) | Error::Other(_)) => {}
                    Err(other) => panic!("{mime}: unexpected error kind {other:?}"),
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
    eprintln!(
        "fuzz: {calls} extractions in {:?}, {contained} third-party panics contained",
        started.elapsed()
    );
}
