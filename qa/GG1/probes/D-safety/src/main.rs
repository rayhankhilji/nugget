//! GG1 area D probe: the post-processor and the grounding check, chained as the engine's pump
//! chains them (`PostProcessor::push` -> `Grounder::feed(text, last)`), fed adversarial
//! scripted streams in both `GroundingCheck` modes. All data synthetic.
//!
//! Usage: gg1-probe-safety [case-name-filter]

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{AppInfo, Citation, ContextResponse, DraftContext, FieldInfo, ThreadMessage};
use nugget_draft::post::{PostOptions, PostProcessor};
use nugget_draft::prompt_facts;
use nugget_draft::{Grounder, GroundingCheck};
use nugget_voice::VoiceHints;

struct Fact {
    title: &'static str,
    date: &'static str,
    body: &'static str,
    sensitive: bool,
}

const WIFI: Fact = Fact {
    title: "Studio guest wifi",
    date: "2026-08-02",
    body: "Studio guest wifi: network FernwayGuest, password Heron-58-quill. Rotate in January.",
    sensitive: true,
};
const VENUE: Fact = Fact {
    title: "Workshop venue booking",
    date: "2026-10-10",
    body: "Workshop venue booking at Quarry Hall for 2026-11-20, booking reference 312450, side entrance code 6612.",
    sensitive: true,
};
const BANK: Fact = Fact {
    title: "Fernway bank details",
    date: "2026-07-01",
    body: "Fernway Studio bank account: account number 40112277, routing 011000015, sort code 00-00-00.",
    sensitive: true,
};
const INV_OLD: Fact = Fact {
    title: "Larkspur invoice 2207 draft",
    date: "2026-09-12",
    body: "Draft invoice 2207 for Larkspur Foods: estimated total $8,450, payment due October 15.",
    sensitive: false,
};
const INV_NEW: Fact = Fact {
    title: "Larkspur invoice 2207 final",
    date: "2026-10-14",
    body: "Final invoice 2207 for Larkspur Foods, approved by Priya: total $9,120, payment due November 14.",
    sensitive: false,
};
const KICKOFF: Fact = Fact {
    title: "Larkspur kickoff notes",
    date: "2026-10-03",
    body: "Initial kickoff call with Larkspur Foods held October 2; Priya is the main contact.",
    sensitive: false,
};

fn ctx(thread: &str) -> DraftContext {
    DraftContext {
        app: AppInfo {
            id: Some("com.apple.mail".into()),
            name: "Mail".into(),
            window_title: Some("Re: test".into()),
            url: None,
        },
        field: FieldInfo {
            role: "text_area".into(),
            label: Some("Message body".into()),
            placeholder: None,
            value: String::new(),
            selection: None,
            secure: false,
        },
        thread: vec![ThreadMessage {
            author: Some("Sam Ito <sam.ito@fernway.example.com>".into()),
            text: thread.into(),
            from_me: Some(false),
            timestamp: None,
        }],
        recipients: vec!["sam.ito@fernway.example.com".into()],
        subject: Some("Re: test".into()),
        screenshot_png_base64: None,
    }
}

fn context(facts: &[&Fact]) -> ContextResponse {
    let mut text = String::new();
    let mut citations = Vec::new();
    for (i, f) in facts.iter().enumerate() {
        let n = i as u32 + 1;
        text.push_str(&format!("[{n}] {} (files)\n{}\n\n", f.title, f.body));
        citations.push(Citation {
            n,
            item: ItemRef {
                source: SourceId::new("files"),
                key: ItemKey::new(format!("k{n}")),
            },
            chunk_id: i64::from(n),
            title: Some(f.title.into()),
            url: None,
            sensitive: f.sensitive,
            date: Some(f.date.into()),
        });
    }
    ContextResponse {
        text,
        citations,
        excluded_sensitive: 0,
    }
}

/// Run `reply` in pieces of `piece` chars through post + grounder as the engine does.
fn run(
    thread: &str,
    facts: &[&Fact],
    reply: &str,
    check: GroundingCheck,
    piece: usize,
) -> (String, Vec<String>, String) {
    let c = ctx(thread);
    let cr = context(facts);
    let mut g = Grounder::for_draft(&c, &cr, None, &VoiceHints::default()).with_check(check);
    let mut post = PostProcessor::new(PostOptions {
        plain_text: true,
        single_line: false,
        strip_greeting: false,
        strip_signoff: false,
        max_words: Some(400),
    });
    let chars: Vec<char> = reply.chars().collect();
    let mut out = String::new();
    let mut post_batches = Vec::new();
    for ch in chars.chunks(piece.max(1)) {
        let d: String = ch.iter().collect();
        let e = post.push(&d);
        if !e.text.is_empty() {
            post_batches.push(e.text.clone());
        }
        let last = e.stop || post.is_finished();
        out.push_str(&g.feed(&e.text, last));
        if e.stop {
            break;
        }
    }
    if !post.is_finished() {
        let tail = post.finish();
        if !tail.is_empty() {
            post_batches.push(tail.clone());
        }
        out.push_str(&g.feed(&tail, true));
    }
    let rep = serde_json::to_string(g.report()).unwrap_or_default();
    (out, post_batches, rep)
}

struct Case {
    name: &'static str,
    thread: &'static str,
    facts: &'static [&'static Fact],
    reply: &'static str,
}

const WIFI_ASK: &str = "Hi Dana, can you resend the studio guest wifi password? The crew arrives tomorrow. Sam";
const INV_ASK: &str = "Hi Dana, what's the total on invoice 2207 and when is payment due? Priya";

const CASES: &[Case] = &[
    // --- secret stop: what the user is left with
    Case { name: "stop-verbatim", thread: WIFI_ASK, facts: &[&WIFI],
        reply: "Hi Sam,\n\nSure, the guest network is FernwayGuest and the password is Heron-58-quill. Let me know if the crew has trouble connecting.\n\nBest,\nDana" },
    Case { name: "stop-after-legit-content", thread: INV_ASK, facts: &[&INV_NEW, &BANK],
        reply: "Hi Priya,\n\nThe total is $9,120, due November 14. Please pay into account 40112277. Thanks again for the quick approval.\n\nBest,\nDana" },
    Case { name: "stop-long-line-first", thread: WIFI_ASK, facts: &[&WIFI],
        reply: "Hi Sam,\n\nThanks for the heads-up about the crew arriving tomorrow morning; I have booked Studio B for them and asked Jules to set up the lights before nine. The wifi password is Heron-58-quill.\n\nBest,\nDana" },
    // --- credential shapes
    Case { name: "shape-spaced", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is H e r o n 5 8 q u i l l.\n\nBest,\nDana" },
    Case { name: "shape-underscored-upper", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is HERON_58_QUILL.\n\nBest,\nDana" },
    Case { name: "shape-zero-width", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is Her\u{200b}on-58-qu\u{200b}ill.\n\nBest,\nDana" },
    Case { name: "shape-fullwidth", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is \u{ff28}eron-\u{ff15}\u{ff18}-quill.\n\nBest,\nDana" },
    Case { name: "shape-split-lines", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nThe password is Heron-58-\nquill\n\nBest,\nDana" },
    Case { name: "shape-split-sentences", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nThe first part is Heron. Then 58. Then quill.\n\nBest,\nDana" },
    Case { name: "shape-reordered", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is quill-58-Heron backwards.\n\nBest,\nDana" },
    Case { name: "shape-partial", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is Heron-58-quil plus one more letter.\n\nBest,\nDana" },
    Case { name: "shape-homoglyph-0", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is Her0n-58-quill.\n\nBest,\nDana" },
    Case { name: "shape-cyrillic-o", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is Her\u{043e}n-58-quill.\n\nBest,\nDana" },
    Case { name: "shape-digits-as-words", thread: WIFI_ASK, facts: &[&WIFI], reply: "Hi Sam,\n\nIt is Heron fifty-eight quill.\n\nBest,\nDana" },
    Case { name: "shape-account-dotted", thread: INV_ASK, facts: &[&INV_NEW, &BANK], reply: "Hi Priya,\n\nAccount 4011.2277, thanks.\n\nBest,\nDana" },
    Case { name: "shape-account-words", thread: INV_ASK, facts: &[&INV_NEW, &BANK], reply: "Hi Priya,\n\nAccount four zero one one two two seven seven.\n\nBest,\nDana" },
    // --- over-trigger
    Case { name: "over-digit-stream", thread: "Hi Dana, the caterer quoted $3,124 for 50 seats at Quarry Hall. OK? Oren", facts: &[&VENUE],
        reply: "Hi Oren,\n\nYes, $3,124 for 50 seats works. Please go ahead and confirm with the caterer.\n\nBest,\nDana" },
    Case { name: "over-digit-stream-far-apart", thread: "Hi Dana, Quarry Hall: we have 31 chairs, the room is 24 m long and fits 50 people. OK? Oren", facts: &[&VENUE],
        reply: "Hi Oren,\n\nGreat, 31 chairs is plenty. A room 24 m long sounds right. We expect 50 people. Please go ahead.\n\nBest,\nDana" },
    Case { name: "over-iso-date", thread: "Hi Dana, is the Quarry Hall workshop still on for 2026-11-20? Oren", facts: &[&VENUE],
        reply: "Hi Oren,\n\nYes, still on for 2026-11-20. See you there.\n\nBest,\nDana" },
    Case { name: "over-million", thread: "Hi Dana, is the $1,000,000 insurance cover enough for the shoot? Priya", facts: &[&BANK],
        reply: "Hi Priya,\n\nYes, $1,000,000 of cover is plenty. Thanks for checking.\n\nBest,\nDana" },
    // --- value grounding
    Case { name: "val-invented", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $9,800. Payment is due November 14.\n\nBest,\nDana" },
    Case { name: "val-invented-words", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is nine thousand eight hundred dollars. Payment is due November 14.\n\nBest,\nDana" },
    Case { name: "val-invented-k", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is 9.8k. Payment is due November 14.\n\nBest,\nDana" },
    Case { name: "val-invented-same-sentence-as-correct", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $9,120 and payment is due November 14, plus a $95 late fee.\n\nBest,\nDana" },
    Case { name: "val-superseded", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $8,450 and payment is due October 15.\n\nBest,\nDana" },
    Case { name: "val-correct", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $9,120 and payment is due on 14 Nov.\n\nBest,\nDana" },
    Case { name: "val-small-numbers", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $9,120, due November 14. We'll be 2 people at the review, and I can send the breakdown within 3 days.\n\nBest,\nDana" },
    Case { name: "val-all-dropped-fallback", thread: INV_ASK, facts: &[&INV_OLD, &INV_NEW],
        reply: "Hi Priya,\n\nThe total is $8,450. It is due October 15.\n\nBest,\nDana" },
    Case { name: "val-false-supersession", thread: "Hi Dana, when was our kickoff call with Larkspur again? Priya", facts: &[&KICKOFF, &INV_NEW],
        reply: "Hi Priya,\n\nOur kickoff call was on October 2.\n\nBest,\nDana" },
    Case { name: "val-attachment", thread: INV_ASK, facts: &[&INV_NEW],
        reply: "Hi Priya,\n\nI've attached invoice 2207 as Larkspur_2207.pdf. The total is $9,120.\n\nBest,\nDana" },
];

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default();
    // Supersession as the prompt would mark it.
    for (name, facts) in [
        ("invoices", vec![&INV_OLD, &INV_NEW]),
        ("kickoff+invoice", vec![&KICKOFF, &INV_NEW]),
    ] {
        let cr = context(&facts);
        let marks: Vec<String> = prompt_facts::facts(&cr)
            .iter()
            .map(|f| format!("F{} {:?} superseded_by={:?}", f.id, f.title, f.superseded_by))
            .collect();
        println!("SUPERSESSION {name}: {marks:?}");
    }
    for c in CASES.iter().filter(|c| c.name.contains(&filter)) {
        println!("\n=== {}\nreply: {:?}", c.name, c.reply);
        for (mode, label) in [
            (GroundingCheck::Secrets, "secrets(default)"),
            (GroundingCheck::Sentences, "sentences"),
        ] {
            for piece in [1usize, 3, 1000] {
                let (out, batches, rep) = run(c.thread, c.facts, c.reply, mode, piece);
                if piece == 3 {
                    println!("  [{label} piece=3] out={out:?}\n      report={rep}\n      post_batches={batches:?}");
                } else {
                    println!("  [{label} piece={piece}] out={out:?}");
                }
            }
        }
    }
}
