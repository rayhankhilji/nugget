//! A seeded store for a fictional user, Alex Morgan: sent and received mail (metadata shaped as
//! `nugget-ingest` stores it: `source` from the connector, `extracted` from `nugget-extract`),
//! an IMAP Sent folder, a draft, a sensitive message and Markdown notes.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::DraftFeedback;
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use nugget_voice::{
    Channel, FeedbackContext, FeedbackSink, MeIdentity, StoreFeedbackSink, VOICE_CURSOR_SOURCE,
    VoiceConfig, VoiceProfile, VoiceQuery, VoiceSource, style_distance,
};
use serde_json::{Value, json};

const GMAIL: &str = "gmail:alex@morgan.example";
const IMAP: &str = "imap:work";
const NOTES: &str = "files:notes";

const ALEX: &str = "Alex Morgan <alex@morgan.example>";
const SAM: &str = "Sam Lee <sam@acme.example>";
const OKAFOR: &str = "Dr. Ngozi Okafor <n.okafor@clinic.example>";
const PRIYA: &str = "Priya Shah <priya@studio.example>";

struct Mail<'a> {
    source: &'a str,
    key: &'a str,
    from: &'a str,
    to: &'a [&'a str],
    date: &'a str,
    subject: &'a str,
    body: &'a str,
    /// Gmail labels (`source.labels`) or, for IMAP, the folder.
    labels: &'a [&'a str],
    folder: Option<&'a str>,
    quoted: Option<&'a str>,
    signature: Option<&'a str>,
    message_id: Option<&'a str>,
    sensitive: bool,
}

impl Default for Mail<'_> {
    fn default() -> Self {
        Self {
            source: GMAIL,
            key: "",
            from: ALEX,
            to: &[],
            date: "2026-09-01T10:00:00Z",
            subject: "",
            body: "",
            labels: &["SENT"],
            folder: None,
            quoted: None,
            signature: None,
            message_id: None,
            sensitive: false,
        }
    }
}

fn put_mail(store: &Store, m: Mail<'_>) {
    let source = match m.folder {
        Some(f) => json!({ "folder": f, "uid": 1, "uidvalidity": 7, "flags": ["\\Seen"] }),
        None => {
            json!({ "labels": m.labels, "thread_id": "t", "from": m.from, "subject": m.subject })
        }
    };
    let mut extracted = json!({
        "from": m.from,
        "to": m.to,
        "date": m.date,
        "subject": m.subject,
    });
    if let Some(q) = m.quoted {
        extracted["quoted"] = json!(q);
    }
    if let Some(s) = m.signature {
        extracted["signature"] = json!(s);
    }
    if let Some(id) = m.message_id {
        extracted["message_id"] = json!(id);
    }
    let meta = json!({
        "source": source,
        "mime": "message/rfc822",
        "extracted": extracted,
        "sensitive": m.sensitive,
        "junk": false,
    });
    put(store, m.source, m.key, m.subject, m.body, meta, m.date);
}

fn put(store: &Store, source: &str, key: &str, title: &str, text: &str, meta: Value, v: &str) {
    let item = NewItem::new(source, key, content_hash(format!("{text}{meta}")))
        .with_title(title)
        .with_text(text)
        .with_mime(meta["mime"].as_str().unwrap_or("text/plain"))
        .with_version(format!("v:{v}:{}", content_hash(text)))
        .with_metadata(meta);
    store
        .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
        .unwrap();
}

fn put_note(store: &Store, key: &str, text: &str) {
    let meta = json!({ "source": { "path": key }, "mime": "text/markdown", "sensitive": false,
                       "junk": false });
    put(store, NOTES, key, key, text, meta, "1");
}

fn seeded() -> Store {
    let store = Store::open_in_memory().unwrap();
    // Casual mail to Sam.
    let sam_mails = [
        (
            "s1",
            "2026-09-20T09:00:00Z",
            "Offsite venue",
            "hey Sam! the offsite venue by the lake looks great 🙂 can we lock it for friday?\n\ncheers,\nAlex",
        ),
        (
            "s2",
            "2026-09-10T09:00:00Z",
            "Lunch",
            "hey Sam! yeah lunch thursday works, gonna be a few mins late lol\n\ncheers,\nAlex",
        ),
        (
            "s3",
            "2026-08-01T09:00:00Z",
            "Deck",
            "hey Sam! deck's in the shared folder, lmk if the numbers look off 🙂\n\ncheers,\nAlex",
        ),
        (
            "s4",
            "2026-06-01T09:00:00Z",
            "Re: Tickets",
            "hey Sam! got the tickets, see you there!!\n\ncheers,\nAlex",
        ),
    ];
    for (key, date, subject, body) in sam_mails {
        put_mail(
            &store,
            Mail {
                key,
                to: &[SAM],
                date,
                subject,
                body,
                ..Default::default()
            },
        );
    }
    // A reply whose quoted text was moved into metadata by extraction...
    put_mail(
        &store,
        Mail {
            key: "s5",
            to: &[SAM],
            date: "2026-09-25T09:00:00Z",
            subject: "Re: Budget",
            body: "hey Sam! budget's fine by me, ship it 🙂\n\ncheers,\nAlex",
            quoted: Some("QUOTED-MARKER: Sam asked whether the budget is final."),
            signature: Some("Alex Morgan | Morgan & Co"),
            message_id: Some("<s5@morgan.example>"),
            ..Default::default()
        },
    );
    // ...and one where quoting survived in the text (HTML mail, odd client).
    put_mail(
        &store,
        Mail {
            key: "s6",
            to: &[SAM],
            date: "2026-09-26T09:00:00Z",
            subject: "Re: Hiring",
            body: "hey Sam! yep, let's hire her 🙂\n\ncheers,\nAlex\n\nOn Fri, Sam Lee wrote:\n> LEFTOVER-QUOTE: should we hire Dana?",
            ..Default::default()
        },
    );
    // Formal mail to Dr. Okafor, sent from the work account (IMAP Sent folder).
    let okafor = [
        (
            "Sent Items/7/1",
            "2026-09-15T09:00:00Z",
            "Appointment",
            "Dear Dr. Okafor,\n\nThank you for your message. I would appreciate it if we could move the appointment to Thursday morning, at your convenience.\n\nKind regards,\nAlex Morgan",
        ),
        (
            "Sent Items/7/2",
            "2026-08-15T09:00:00Z",
            "Referral",
            "Dear Dr. Okafor,\n\nPlease find attached the referral letter you requested. Kindly let me know whether anything further is required.\n\nKind regards,\nAlex Morgan",
        ),
        (
            "Sent Items/7/3",
            "2026-07-15T09:00:00Z",
            "Results",
            "Dear Dr. Okafor,\n\nI have received the results and would be grateful for a short call regarding the next steps.\n\nKind regards,\nAlex Morgan",
        ),
    ];
    for (key, date, subject, body) in okafor {
        put_mail(
            &store,
            Mail {
                source: IMAP,
                key,
                from: "Alex Morgan <alex@morgan.example>",
                to: &[OKAFOR],
                date,
                subject,
                body,
                folder: Some("Sent Items"),
                ..Default::default()
            },
        );
    }
    // The same message stored by both connectors (same Message-ID): counted once.
    for (source, key, folder) in [
        (GMAIL, "dup", None),
        (IMAP, "Sent Items/7/9", Some("Sent Items")),
    ] {
        put_mail(
            &store,
            Mail {
                source,
                key,
                to: &[PRIYA],
                date: "2026-09-05T09:00:00Z",
                subject: "Logo",
                body: "Hi Priya,\n\nThe new logo looks great, thanks for turning it around so fast.\n\nThanks,\nAlex",
                folder,
                message_id: Some("<logo@morgan.example>"),
                ..Default::default()
            },
        );
    }
    // A sensitive sent message (PII found at ingest).
    put_mail(
        &store,
        Mail {
            key: "secret",
            to: &[SAM],
            date: "2026-09-27T09:00:00Z",
            subject: "Bank details",
            body: "hey Sam! SENSITIVE-MARKER my bank account for the venue deposit is below 🙂\n\ncheers,\nAlex",
            sensitive: true,
            ..Default::default()
        },
    );
    // Received mail and a draft: never the user's voice.
    put_mail(
        &store,
        Mail {
            key: "in1",
            from: SAM,
            to: &[ALEX],
            subject: "Offsite venue",
            body: "RECEIVED-MARKER Hey Alex, what do you think of the venue by the lake?\n\nSam",
            labels: &["INBOX"],
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "in2",
            from: "Recruiter <jobs@spam.example>",
            to: &[ALEX],
            subject: "Opportunity",
            body: "RECEIVED-MARKER Dear Candidate, we have an opportunity.",
            labels: &["INBOX", "CATEGORY_PROMOTIONS"],
            ..Default::default()
        },
    );
    put_mail(
        &store,
        Mail {
            key: "draft",
            to: &[SAM],
            subject: "Unsent",
            body: "DRAFT-MARKER hey Sam, not sure yet",
            labels: &["DRAFT"],
            ..Default::default()
        },
    );
    // Notes.
    put_note(
        &store,
        "ideas.md",
        "# Ideas\n\nNOTE-MARKER A lakeside offsite with short sessions and long walks.",
    );
    store
}

fn alex() -> Vec<String> {
    vec![SAM.to_string()]
}

fn all_exemplars(p: &VoiceProfile, channel: Channel, allow_sensitive: bool) -> Vec<String> {
    p.exemplars(
        &VoiceQuery::new(channel)
            .with_thread("anything")
            .allow_sensitive(allow_sensitive),
    )
}

fn big_config() -> VoiceConfig {
    VoiceConfig {
        max_exemplars: 100,
        min_exemplars: 100,
        exemplar_chars: 10_000,
        ..Default::default()
    }
}

#[test]
fn infers_me_from_sent_mail() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    assert_eq!(p.me().emails, vec!["alex@morgan.example"]);
    assert_eq!(p.me().names, vec!["Alex Morgan"]);
    // Configured identity is kept and merged.
    let p = VoiceProfile::build(
        &store,
        MeIdentity::new(["Alex@Work.example"], ["Alex M"]),
        VoiceConfig::default(),
    )
    .unwrap();
    assert!(p.me().has_email("alex@work.example"));
    assert!(p.me().has_email("alex@morgan.example"));
}

#[test]
fn collects_only_own_messages_without_quotes() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), big_config()).unwrap();
    let ex = all_exemplars(&p, Channel::Email, true);
    let joined = ex.join("\n---\n");
    for marker in [
        "QUOTED-MARKER",
        "LEFTOVER-QUOTE",
        "RECEIVED-MARKER",
        "DRAFT-MARKER",
        "NOTE-MARKER",
    ] {
        assert!(!joined.contains(marker), "{marker} leaked: {joined}");
    }
    assert!(!joined.contains("Morgan & Co"), "signature leaked");
    assert!(joined.contains("let's hire her"));
    // 6 to Sam + secret + 3 to Okafor + 1 deduplicated to Priya.
    assert_eq!(ex.len(), 11, "{joined}");
    // Plus one note.
    assert_eq!(p.status().samples, 12);
    // Notes are exemplars for documents only.
    let notes = all_exemplars(&p, Channel::Document, true);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("NOTE-MARKER"));
}

#[test]
fn normalises_greetings_and_signoffs_per_recipient() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let sam = p
        .recipient_stats("SAM@acme.example")
        .expect("sam has stats");
    assert_eq!(sam.samples, 7);
    assert_eq!(sam.greetings[0], ("hey {first_name}!".to_string(), 1.0));
    assert_eq!(sam.signoffs[0], ("cheers,\n{my_name}".to_string(), 1.0));
    assert_eq!(sam.lowercase_start_rate, 1.0);
    assert!(sam.emoji_per_message > 0.5);

    let okafor = p.recipient_stats(OKAFOR).expect("okafor has stats");
    assert_eq!(okafor.samples, 3);
    assert_eq!(okafor.greetings[0].0, "Dear Dr. {last_name},");
    assert_eq!(okafor.signoffs[0].0, "Kind regards,\n{my_name}");
    assert_eq!(okafor.emoji_per_message, 0.0);
    assert!(
        okafor.formality > sam.formality + 0.3,
        "okafor {} sam {}",
        okafor.formality,
        sam.formality
    );
    assert_eq!(okafor.languages[0].0, "eng");

    // Priya has a single message: no per-recipient stats; the channel's are used.
    assert!(p.recipient_stats(PRIYA).is_none());
    let email = p.channel_stats(Channel::Email).expect("email stats");
    assert_eq!(email.samples, 11);
    assert_eq!(p.stats_for(Channel::Email, &[PRIYA.into()]), email);
    assert_eq!(p.stats_for(Channel::Email, &alex()), sam);
    // Chat has no samples: overall stats.
    assert_eq!(p.stats_for(Channel::Chat, &[]), p.overall_stats());
}

#[test]
fn exemplars_rank_recipient_then_channel_then_topic() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();

    // To Sam about the venue: Sam's venue mail first, only Sam's mail (6 available).
    let h = p.hints(
        Channel::Email,
        &alex(),
        "Sam: did you book the offsite venue by the lake?",
    );
    assert_eq!(h.channel, Some(Channel::Email));
    assert_eq!(h.exemplars.len(), 6);
    assert!(
        h.exemplars[0].contains("offsite venue"),
        "{:?}",
        h.exemplars
    );
    assert!(
        h.exemplars.iter().all(|e| e.starts_with("hey Sam!")),
        "{:?}",
        h.exemplars
    );
    assert!(h.exemplars.iter().all(|e| e.chars().count() <= 600));

    // To Okafor about results: the results mail first, then the rest of Okafor's, then the
    // channel.
    let h = p.hints(
        Channel::Email,
        &[OKAFOR.into()],
        "Dr. Okafor: the results are in, shall we discuss next steps?",
    );
    assert!(
        h.exemplars[0].contains("received the results"),
        "{:?}",
        h.exemplars
    );
    assert!(
        h.exemplars[..3]
            .iter()
            .all(|e| e.starts_with("Dear Dr. Okafor"))
    );
    assert_eq!(h.exemplars.len(), 6);

    // A new recipient: same channel, by topic.
    let h = p.hints(
        Channel::Email,
        &["new@person.example".into()],
        "Could you send the referral letter?",
    );
    assert!(
        h.exemplars[0].contains("referral letter"),
        "{:?}",
        h.exemplars
    );

    // Chat has no samples: other channels fill up to the minimum (3), never notes.
    let h = p.hints(Channel::Chat, &[], "lunch thursday?");
    assert_eq!(h.exemplars.len(), 3);
    assert!(
        h.exemplars[0].contains("lunch thursday"),
        "{:?}",
        h.exemplars
    );
    assert!(h.exemplars.iter().all(|e| !e.contains("NOTE-MARKER")));
}

#[test]
fn exemplars_are_deduplicated_and_truncated() {
    let store = seeded();
    for (i, date) in ["2026-10-01T09:00:00Z", "2026-10-02T09:00:00Z"]
        .iter()
        .enumerate()
    {
        put_mail(
            &store,
            Mail {
                key: if i == 0 { "rep1" } else { "rep2" },
                to: &[PRIYA],
                date,
                subject: "Logo",
                body: "Hi Priya,\n\nThe new logo looks great, thanks for turning it around so fast!\n\nThanks,\nAlex",
                ..Default::default()
            },
        );
    }
    let long = format!(
        "Hi Priya,\n\n{}\n\nThanks,\nAlex",
        "The brand guide needs another pass. ".repeat(40)
    );
    put_mail(
        &store,
        Mail {
            key: "long",
            to: &[PRIYA],
            date: "2026-10-03T09:00:00Z",
            subject: "Brand guide",
            body: &long,
            ..Default::default()
        },
    );
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let h = p.hints(Channel::Email, &[PRIYA.into()], "logo and brand guide");
    let logos = h
        .exemplars
        .iter()
        .filter(|e| e.contains("new logo"))
        .count();
    assert_eq!(logos, 1, "{:?}", h.exemplars);
    let guide = h
        .exemplars
        .iter()
        .find(|e| e.contains("brand guide"))
        .unwrap();
    assert!(guide.chars().count() <= 600);
    assert!(guide.ends_with('…'));
}

#[test]
fn sensitive_items_stay_out_of_cloud_exemplars() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), big_config()).unwrap();
    let cloud = p.hints(
        Channel::Email,
        &alex(),
        "bank account for the venue deposit",
    );
    assert!(
        cloud
            .exemplars
            .iter()
            .all(|e| !e.contains("SENSITIVE-MARKER"))
    );
    let local = p.exemplars(
        &VoiceQuery::new(Channel::Email)
            .with_recipients(alex())
            .with_thread("bank account for the venue deposit")
            .allow_sensitive(true),
    );
    assert!(local[0].contains("SENSITIVE-MARKER"), "{local:?}");
}

#[test]
fn refresh_is_incremental() {
    let store = seeded();
    let mut p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let before = p.status().samples;
    let last = p.status().last_built.unwrap();
    let cursor = store
        .get_cursor(&SourceId::new(VOICE_CURSOR_SOURCE))
        .unwrap()
        .unwrap();
    assert_eq!(cursor["samples"], json!(before));

    let r = p.refresh().unwrap();
    assert_eq!(r.fetched, 0);
    assert_eq!(r.removed, 0);
    assert_eq!(r.samples, before);
    assert!(p.status().last_built.unwrap() >= last);

    // A new sent mail is read; nothing else is.
    put_mail(
        &store,
        Mail {
            key: "new",
            to: &[SAM],
            date: "2026-10-05T09:00:00Z",
            subject: "Kayaks",
            body: "hey Sam! booked kayaks for the offsite 🙂\n\ncheers,\nAlex",
            ..Default::default()
        },
    );
    let r = p.refresh().unwrap();
    assert_eq!(r.fetched, 1);
    assert_eq!(r.samples, before + 1);
    assert_eq!(p.recipient_stats(SAM).unwrap().samples, 8);

    // An edited item (new version) is read again; a deleted one is dropped.
    put_note(
        &store,
        "ideas.md",
        "# Ideas\n\nKayaks at the lakeside offsite.",
    );
    store
        .delete_item(&ItemRef {
            source: SourceId::new(GMAIL),
            key: ItemKey::new("s4"),
        })
        .unwrap();
    let r = p.refresh().unwrap();
    assert_eq!(r.fetched, 1);
    assert_eq!(r.removed, 1);
    assert_eq!(r.samples, before);
    let notes = all_exemplars(&p, Channel::Document, true);
    assert!(notes[0].contains("Kayaks"));

    // Removing a whole source drops its messages.
    store.delete_source(&SourceId::new(IMAP)).unwrap();
    p.refresh().unwrap();
    assert!(p.recipient_stats(OKAFOR).is_none());
}

#[test]
fn feedback_final_texts_join_the_profile() {
    let store = seeded();
    let mut p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let before = p.status().samples;
    let sink = StoreFeedbackSink::new(store.clone());
    let ctx = FeedbackContext {
        channel: Channel::Chat,
        recipients: vec!["Jamie".into()],
        subject: None,
        sensitive: false,
    };
    let fb = |id: &str, outcome: &str, text: Option<&str>| DraftFeedback {
        stream_id: id.into(),
        outcome: outcome.into(),
        final_text: text.map(str::to_owned),
        insert_verified: Some(true),
    };
    assert!(
        sink.record(&fb("d1", "edited", Some("sure jamie, 5 works 👍")), &ctx)
            .unwrap()
    );
    assert!(
        !sink
            .record(&fb("d2", "rejected", Some("no")), &ctx)
            .unwrap()
    );
    assert!(!sink.record(&fb("d3", "accepted", None), &ctx).unwrap());
    let secret = FeedbackContext {
        sensitive: true,
        ..ctx.clone()
    };
    assert!(
        sink.record(
            &fb("d4", "accepted", Some("pin is in the vault 👍")),
            &secret
        )
        .unwrap()
    );

    let r = p.refresh().unwrap();
    assert_eq!(r.fetched, 2);
    assert_eq!(p.status().samples, before + 2);
    let h = p.hints(Channel::Chat, &["jamie".into()], "can you do 5?");
    assert_eq!(h.exemplars[0], "sure jamie, 5 works 👍");
    assert!(h.exemplars.iter().all(|e| !e.contains("vault")));
    // Stored as a searchable (encrypted at rest) item.
    let hits = store.search_text("jamie works", 5).unwrap();
    assert!(
        hits.iter()
            .any(|h| h.item.source.as_str() == nugget_voice::FEEDBACK_SOURCE)
    );
}

#[test]
fn status_reports_channels() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let s = VoiceSource::status(&p);
    assert_eq!(s.samples, 12);
    assert_eq!(s.channels, vec!["email"]);
    assert!(s.last_built.is_some());

    // Shared behind a lock, as the daemon does.
    let shared = std::sync::Arc::new(std::sync::RwLock::new(p));
    let h = shared.hints(&VoiceQuery::new(Channel::Email).with_recipients(alex()));
    assert_eq!(h.exemplars.len(), 6);
}

#[test]
fn own_held_out_text_is_closer_than_another_persona() {
    let store = seeded();
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    let sam_stats = p.recipient_stats(SAM).unwrap();
    let mine =
        "hey Sam! the caterer confirmed for friday, gonna grab drinks too 🙂\n\ncheers,\nAlex";
    let other = "Dear Samuel,\n\nI am writing to confirm that the catering arrangements for Friday \
                 have been finalised. Please do not hesitate to contact me should you require \
                 any further information.\n\nYours sincerely,\nJordan Whitfield";
    let d_mine = style_distance(sam_stats, mine);
    let d_other = style_distance(sam_stats, other);
    assert!(d_mine < d_other, "mine {d_mine} other {d_other}");
    assert!(d_mine < 0.2, "{d_mine}");
    assert!(d_other > 0.5, "{d_other}");

    let okafor = p.recipient_stats(OKAFOR).unwrap();
    let formal_mine = "Dear Dr. Okafor,\n\nThank you for the update. I would appreciate a copy of the \
                       report at your convenience.\n\nKind regards,\nAlex Morgan";
    assert!(style_distance(okafor, formal_mine) < style_distance(okafor, mine));

    assert!(p.similarity(mine) > p.similarity(other));
    assert!(
        p.similarity_for(Channel::Email, &alex(), mine)
            > p.similarity_for(Channel::Email, &alex(), other)
    );
    assert!(nugget_voice::similarity(&p, mine) > nugget_voice::similarity(&p, other));
}

#[test]
fn configured_address_claims_mail_outside_sent_folders() {
    let store = Store::open_in_memory().unwrap();
    // A copy of the user's own mail in the inbox (e.g. sent from another client, Bcc to self).
    put_mail(
        &store,
        Mail {
            source: IMAP,
            key: "INBOX/7/1",
            from: "Alex <alex@work.example>",
            to: &[SAM],
            subject: "Copy",
            body: "hey Sam! copy of what I sent 🙂",
            folder: Some("INBOX"),
            ..Default::default()
        },
    );
    let p = VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
    assert_eq!(p.status().samples, 0);
    let p = VoiceProfile::build(
        &store,
        MeIdentity::new(["alex@work.example"], ["Alex"]),
        VoiceConfig::default(),
    )
    .unwrap();
    assert_eq!(p.status().samples, 1);
}
