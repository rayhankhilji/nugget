//! WhatsApp exports: locale variants, zips, merging, filters and validation.

use std::io::Write as _;
use std::path::Path;

use nugget_connector::ConnectorFactory;
use nugget_connector::testkit::Replica;
use nugget_connector_messaging::WhatsAppFactory;
use nugget_core::Error;
use serde_json::json;
use time::OffsetDateTime;

use crate::common::*;

const ALL_KEYS: &[&str] = &[
    "wa:alice/2023-12",
    "wa:bob/2023-12",
    "wa:bob/2024-01",
    "wa:carol/2024-01",
    "wa:dave/2023-12",
    "wa:dave/2024-02",
    "wa:family/2023-12",
    "wa:family/2024-01",
    "wa:oma/2023-12",
    "wa:yusuf/2023-12",
    "wa:zoé/2023-12",
    "wa:김민수/2023-12",
    "wa:田中/2023-12",
];

fn settings(folder: &Path) -> serde_json::Value {
    json!({ "folder": folder, "me": "Tanuj" })
}

#[tokio::test]
async fn every_locale_variant_is_imported() {
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::WhatsApp, &settings(&fixtures("whatsapp")), &replica)
        .await;
    assert_eq!(keys(&replica), set(ALL_KEYS));
    for d in upserts(&sink) {
        assert_well_formed(&d);
        assert_eq!(d.metadata["service"], "whatsapp");
        assert_eq!(d.metadata["sensitive"], true);
        assert!(d.url.as_deref().is_some_and(|u| u.starts_with("file://")));
    }

    // iOS group: system lines, media, multi-line text, me.
    let fam = doc(&sink, "wa:family/2023-12");
    let t = text(&fam);
    assert_eq!(fam.metadata["is_group"], true);
    assert_eq!(fam.title.as_deref(), Some("Family — December 2023"));
    assert!(t.contains("conversation: \"Family\""), "{t}");
    assert!(
        t.contains("_2023-12-31 21:10 · Messages and calls are end-to-end encrypted."),
        "{t}"
    );
    assert!(t.contains("Mum created group “Family”"), "{t}");
    assert!(
        t.contains("**Mum** · 2023-12-31 21:15\n> Happy new year everyone! 🎆👩‍👩‍👧‍👦\n> Can't wait to see you all\n"),
        "{t}"
    );
    assert!(
        t.contains(
            "**Dad** · 2023-12-31 21:16\n> [photo: 00000012-PHOTO-2023-12-31-21-16-00.jpg]\n"
        ),
        "{t}"
    );
    assert!(t.contains("**Sam** · 2023-12-31 21:17\n> [photo]\n"), "{t}");
    assert!(
        t.contains("**Me** · 2023-12-31 21:18\n> Love you all\n"),
        "{t}"
    );
    let atts = fam.metadata["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0]["mime"], "image/jpeg");
    let me: Vec<_> = fam.metadata["participants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["me"] == true)
        .collect();
    assert_eq!(me.len(), 1);
    assert_eq!(me[0]["name"], "Tanuj");
    let jan = doc(&sink, "wa:family/2024-01");
    let t = text(&jan);
    assert!(t.contains("> [file: Report final.pdf]"), "{t}");
    assert!(
        t.contains("**Sam** · 2024-01-01 08:01\n> _(message deleted)_"),
        "{t}"
    );

    // iOS US 12-hour clock with U+202F, and text that looks like Markdown structure.
    let t = text(&doc(&sink, "wa:alice/2023-12")).to_owned();
    assert!(
        t.contains("**Alice** · 2023-12-31 21:15\n> Are you coming tonight?"),
        "{t}"
    );
    assert!(
        t.contains("**Alice** · 2023-12-31 23:59\n> --- not frontmatter\n> # not a heading\n"),
        "{t}"
    );
    assert_eq!(doc(&sink, "wa:alice/2023-12").metadata["is_group"], false);

    // Android: media omitted, file attached with caption, deleted, phone-number author.
    let t = text(&doc(&sink, "wa:bob/2023-12")).to_owned();
    assert!(t.contains("**Bob** · 2023-12-31 21:15\n> [file]\n"), "{t}");
    assert!(
        t.contains("> Look at this view\n> [photo: IMG-20231231-WA0001.jpg]\n"),
        "{t}"
    );
    assert!(
        t.contains("**Me** · 2023-12-31 21:17\n> _(message deleted)_"),
        "{t}"
    );
    let jan = doc(&sink, "wa:bob/2024-01");
    let t = text(&jan);
    assert!(
        t.contains("> Multi-line\n> message\n>   indented third line\n"),
        "{t}"
    );
    let contacts = jan.metadata["contacts"].as_array().unwrap();
    assert_eq!(contacts.len(), 1, "{contacts:?}");
    assert_eq!(contacts[0]["phones"][0], "+15551234567");

    // Ambiguous dates + AM/PM → month-first.
    let t = text(&doc(&sink, "wa:carol/2024-01")).to_owned();
    assert!(t.contains("**Carol** · 2024-01-02 21:15"), "{t}");
    assert!(t.contains("**Me** · 2024-01-03 10:00"), "{t}");

    assert!(text(&doc(&sink, "wa:oma/2023-12")).contains("> [file]\n"));
    assert!(text(&doc(&sink, "wa:dave/2024-02")).contains("> [video: VID-20240201-WA0003.mp4]"));
    assert!(
        text(&doc(&sink, "wa:김민수/2023-12"))
            .contains("**김민수** · 2023-12-31 21:15\n> 새해 복 많이 받으세요")
    );
    assert!(text(&doc(&sink, "wa:김민수/2023-12")).contains("**Me** · 2023-12-31 11:20"));
    assert!(text(&doc(&sink, "wa:田中/2023-12")).contains("**田中** · 2023-12-31 21:15"));
    assert!(text(&doc(&sink, "wa:田中/2023-12")).contains("**Me** · 2023-12-31 09:00"));
    let t = text(&doc(&sink, "wa:yusuf/2023-12")).to_owned();
    assert!(
        t.contains("> مرحبا بالعالم") && t.contains("> שלום עולם"),
        "{t}"
    );
    assert!(t.contains("**Me** · 2023-12-31 09:17"), "{t}");
    let t = text(&doc(&sink, "wa:zoé/2023-12")).to_owned();
    assert!(t.contains("conversation: \"Zoé\""), "{t}");
    assert!(
        t.contains("**Zoé** · 2023-12-31 21:15\n> Bonne année !\n"),
        "{t}"
    );
    assert!(t.contains("> Merci\n> à toi aussi\n"), "{t}");
    assert!(!t.contains('\r'));

    // A second pass is a no-op (stable versions).
    let again = env
        .pass(Kind::WhatsApp, &settings(&fixtures("whatsapp")), &replica)
        .await;
    assert_eq!(again.sends(), 0, "{:?}", again.summary());
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

#[tokio::test]
async fn zip_exports_read_only_the_chat_text() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("exports");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    // iOS: `_chat.txt` plus media.
    let ios = "[31/12/2023, 21:15:42] Eve: \u{200e}<attached: 00000003-PHOTO-2023-12-31-21-15-42.jpg>\n\
[31/12/2023, 21:16:00] Eve: nice\n";
    std::fs::write(
        folder.join("WhatsApp Chat - Eve.zip"),
        zip_bytes(&[
            ("_chat.txt", ios.as_bytes()),
            ("00000003-PHOTO-2023-12-31-21-15-42.jpg", &[0u8; 1234]),
        ]),
    )
    .unwrap();
    // Android in a subfolder: the txt is named after the chat.
    let android = "31/12/2023, 21:15 - Finn: IMG-20231231-WA0007.jpg (file attached)\n";
    std::fs::write(
        folder.join("sub").join("WhatsApp Chat with Finn.zip"),
        zip_bytes(&[
            ("WhatsApp Chat with Finn.txt", android.as_bytes()),
            ("IMG-20231231-WA0007.jpg", &[1u8; 77]),
        ]),
    )
    .unwrap();
    // A zip without a chat is ignored; a corrupt one is skipped.
    std::fs::write(folder.join("photos.zip"), zip_bytes(&[("a.jpg", b"x")])).unwrap();

    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    assert_eq!(keys(&replica), set(&["wa:eve/2023-12", "wa:finn/2023-12"]));
    let eve = doc(&sink, "wa:eve/2023-12");
    assert_eq!(eve.metadata["attachments"][0]["size"], 1234);
    assert!(text(&eve).contains("> [photo: 00000003-PHOTO-2023-12-31-21-15-42.jpg]"));
    let finn = doc(&sink, "wa:finn/2023-12");
    assert_eq!(finn.metadata["attachments"][0]["size"], 77);
    assert!(eve.url.as_deref().unwrap().ends_with(".zip"));

    // A broken re-export keeps what was imported for that chat.
    std::fs::write(
        folder.join("WhatsApp Chat - Eve.zip"),
        b"PK\x03\x04 truncated",
    )
    .unwrap();
    let sink = env
        .pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    assert!(deletes(&sink).is_empty(), "{:?}", sink.summary());
    assert_eq!(keys(&replica).len(), 2);
}

#[tokio::test]
async fn duplicate_exports_merge_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("exports");
    write(
        &folder,
        "WhatsApp Chat with Gus.txt",
        "31/12/2023, 21:15 - Gus: one\n31/12/2023, 21:16 - Gus: two\n",
    );
    write(
        &folder,
        "WhatsApp Chat with Gus (1).txt",
        "31/12/2023, 21:16 - Gus: two\n15/01/2024, 10:00 - Gus: three\n",
    );
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    assert_eq!(keys(&replica), set(&["wa:gus/2023-12", "wa:gus/2024-01"]));
    let dec = doc(&sink, "wa:gus/2023-12");
    assert_eq!(dec.metadata["message_count"], 2);
    assert_eq!(text(&dec).matches("> two").count(), 1);
}

fn stamp(t: OffsetDateTime) -> String {
    format!(
        "{:02}/{:02}/{:04}, {:02}:{:02}",
        t.day(),
        t.month() as u8,
        t.year(),
        t.hour(),
        t.minute()
    )
}

#[tokio::test]
async fn since_days_and_include_groups_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("exports");
    let now = OffsetDateTime::now_utc();
    let old = now - time::Duration::days(400);
    let recent = now - time::Duration::days(2);
    write(
        &folder,
        "WhatsApp Chat with Hal.txt",
        &format!(
            "{} - Hal: old news\n{} - Hal: fresh\n",
            stamp(old),
            stamp(recent)
        ),
    );
    write(
        &folder,
        "WhatsApp Chat - Club.txt",
        &format!(
            "{} - Ann: hi\n{} - Ben: hi\n{} - Cy: hi\n",
            stamp(recent),
            stamp(recent),
            stamp(recent)
        ),
    );
    let key =
        |name: &str, t: OffsetDateTime| format!("wa:{name}/{:04}-{:02}", t.year(), t.month() as u8);

    let env = Env::new();
    let replica = Replica::new();
    env.pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    let all = keys(&replica);
    assert!(
        all.contains(&key("hal", old)) && all.contains(&key("club", recent)),
        "{all:?}"
    );

    // Narrowing settings (cursor reset → reconciling pass) deletes what fell out of scope.
    let narrowed = json!({ "folder": folder, "since_days": 30, "include_groups": false });
    let sink = env.pass(Kind::WhatsApp, &narrowed, &replica).await;
    assert_eq!(keys(&replica), set(&[&key("hal", recent)]));
    assert!(deletes(&sink).contains(&key("hal", old)));
    assert!(deletes(&sink).contains(&key("club", recent)));
    let hal = replica.get(&key("hal", recent)).unwrap();
    assert!(hal.content.contains("fresh") && !hal.content.contains("old news"));
}

#[tokio::test]
async fn private_false_is_not_sensitive_and_toggling_resends() {
    let folder = fixtures("whatsapp");
    let env = Env::new();
    let replica = Replica::new();
    env.pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    let sink = env
        .pass(
            Kind::WhatsApp,
            &json!({ "folder": folder, "private": false }),
            &replica,
        )
        .await;
    let ups = upserts(&sink);
    assert_eq!(ups.len(), ALL_KEYS.len(), "every document re-sent");
    assert!(ups.iter().all(|d| d.metadata["sensitive"] == false));
}

#[tokio::test]
async fn removed_export_deletes_its_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("w");
    copy_dir(&fixtures("whatsapp"), &folder);
    let env = Env::new();
    let replica = Replica::new();
    env.pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    std::fs::remove_file(folder.join("WhatsApp Chat with Bob.txt")).unwrap();
    let sink = env
        .pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    assert_eq!(deletes(&sink), set(&["wa:bob/2023-12", "wa:bob/2024-01"]));
    assert_eq!(sink.upserts().len(), 0);
}

#[tokio::test]
async fn missing_folder_is_an_error_not_a_wipe() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("w");
    copy_dir(&fixtures("whatsapp"), &folder);
    let env = Env::new();
    let replica = Replica::new();
    env.pass(Kind::WhatsApp, &json!({ "folder": folder }), &replica)
        .await;
    std::fs::remove_dir_all(&folder).unwrap();
    let conn = env.build(
        Kind::WhatsApp,
        &json!({ "folder": folder }),
        replica.clone(),
    );
    let sink = nugget_connector::testkit::ReplicaSink::new(replica.clone());
    let err = nugget_connector::testkit::run_pass(&*conn, None, &sink)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert_eq!(keys(&replica).len(), ALL_KEYS.len());
}

#[test]
fn validation() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        WhatsAppFactory
            .validate(&json!({ "folder": tmp.path() }))
            .is_ok()
    );
    let missing = tmp.path().join("nope");
    match WhatsAppFactory.validate(&json!({ "folder": missing })) {
        Err(Error::NotFound(m)) => assert!(m.contains("nope"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        WhatsAppFactory.validate(&json!({})),
        Err(Error::Invalid(_))
    ));
    let file = write(tmp.path(), "f.txt", "x");
    assert!(matches!(
        WhatsAppFactory.validate(&json!({ "folder": file })),
        Err(Error::Invalid(_))
    ));
    let info = WhatsAppFactory.info();
    assert_eq!(
        (info.kind, info.display_name),
        ("whatsapp", "WhatsApp exports")
    );
}
