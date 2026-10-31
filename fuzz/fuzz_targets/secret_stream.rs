//! Streaming credential stop: a draft that copies a credential from a sensitive fact, fed to the
//! grounding stage in arbitrary chunks.
//!
//! The first byte picks the check (bit 0: `Secrets`, word by word, else `Sentences`) and the
//! credential's length (6 to 37); the next bytes spell it (letters and digits, with at least one
//! of each so that it counts as a credential). The rest, as UTF-8 (lossy), is the draft: each
//! U+0001 becomes the credential, U+0002 the credential spelled out with padding between its
//! characters, U+0004 the credential in fullwidth forms, U+0005 its first half followed by a
//! space, and U+0003 splits the draft into the chunks it is fed in.
//!
//! Invariant: whatever the chunking, the emitted text, reduced to its letters and digits in
//! lower case (fullwidth folded, as the check compares), never contains the credential.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{Citation, ContextResponse, DraftContext};
use nugget_draft::{Grounder, GroundingCheck};
use nugget_voice::VoiceHints;

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// The same reduction the check applies.
fn squash(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            c => c,
        })
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let len = 6 + usize::from(mode >> 3);
    if rest.len() < len {
        return;
    }
    let (spell, draft) = rest.split_at(len);
    let mut secret: Vec<u8> = spell
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()])
        .collect();
    secret[0] = b'a' + spell[0] % 26;
    secret[len - 1] = b'0' + spell[len - 1] % 10;
    let secret = String::from_utf8(secret).expect("ascii");

    let check = if mode & 1 == 1 {
        GroundingCheck::Secrets
    } else {
        GroundingCheck::Sentences
    };
    let context = ContextResponse {
        text: format!("[1] Vault (notes)\nThe admin password is {secret} and rotates monthly."),
        citations: vec![Citation {
            n: 1,
            item: ItemRef {
                source: SourceId::new("notes"),
                key: ItemKey::new("k1"),
            },
            chunk_id: 1,
            title: Some("Vault".into()),
            url: None,
            sensitive: true,
            date: Some("2026-10-01".into()),
        }],
        excluded_sensitive: 0,
    };
    let spaced: String = secret.chars().map(|c| format!("{c} , ")).collect();
    let wide: String = secret
        .chars()
        .map(|c| char::from_u32(c as u32 + 0xfee0).unwrap_or(c))
        .collect();
    let half = format!("{} ", &secret[..len / 2]);
    let draft = String::from_utf8_lossy(draft)
        .replace('\u{1}', &secret)
        .replace('\u{2}', &spaced)
        .replace('\u{4}', &wide)
        .replace('\u{5}', &half);

    let mut g = Grounder::for_draft(
        &DraftContext::default(),
        &context,
        None,
        &VoiceHints::default(),
    )
    .with_check(check);
    let mut out = String::new();
    for chunk in draft.split('\u{3}') {
        out.push_str(&g.push(chunk));
    }
    out.push_str(&g.finish());
    assert!(g.is_finished());
    assert!(
        g.finish().is_empty() && g.push("more").is_empty(),
        "output after the end"
    );

    let needle = squash(&secret);
    assert!(
        !squash(&out).contains(&needle),
        "credential emitted ({check:?}): {out:?}"
    );
});
