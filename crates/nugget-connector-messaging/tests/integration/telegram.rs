//! Telegram Desktop exports: single-chat and full-account JSON, merged.

use nugget_connector::ConnectorFactory;
use nugget_connector::testkit::Replica;
use nugget_connector_messaging::TelegramFactory;
use nugget_core::Error;
use serde_json::json;

use crate::common::*;

#[tokio::test]
async fn single_and_full_exports_are_merged() {
    let env = Env::new();
    let replica = Replica::new();
    let sink = env
        .pass(
            Kind::Telegram,
            &json!({ "path": fixtures("telegram") }),
            &replica,
        )
        .await;
    assert_eq!(
        keys(&replica),
        set(&[
            "tg:1001/2024-03",
            "tg:1001/2024-04",
            "tg:4242/2024-04",
            "tg:4242/2024-05",
            "tg:7/2024-01",
        ])
    );
    for d in upserts(&sink) {
        assert_well_formed(&d);
        assert_eq!(d.metadata["service"], "telegram");
    }

    // One-to-one: present in both exports, de-duplicated; me inferred in the single export.
    let apr = doc(&sink, "tg:4242/2024-04");
    assert_eq!(apr.metadata["message_count"], 2, "{}", text(&apr));
    assert_eq!(apr.metadata["is_group"], false);
    assert!(text(&apr).contains("**Me** · 2024-04-29 08:00\n> Only in the full export"));
    let may = doc(&sink, "tg:4242/2024-05");
    let t = text(&may);
    assert!(
        t.contains("**Alice Example** · 2024-05-01 09:12\n> Check https://example.com and this\n"),
        "{t}"
    );
    assert!(
        t.contains("**Me** · 2024-05-01 09:13\n> [photo: photo_1@01-05-2024_09-13-00.jpg]\n"),
        "{t}"
    );
    assert!(t.contains("**Me** · 2024-05-01 09:14\n> [audio]\n"), "{t}");
    assert!(
        t.contains("_2024-05-01 09:20 · Alice Example called (61 s)_"),
        "{t}"
    );
    // `date` without `date_unixtime` is read as UTC.
    assert!(
        t.contains("**Alice Example** · 2024-05-02 10:00\n> no unixtime here"),
        "{t}"
    );
    let atts = may.metadata["attachments"].as_array().unwrap();
    assert_eq!(atts[0]["size"], 20480);
    assert_eq!(atts[1]["mime"], "audio/ogg");
    assert!(atts[1]["name"].is_null());
    assert!(may.url.as_deref().unwrap().ends_with("result.json"));

    // Group with service lines and a document.
    let club = doc(&sink, "tg:1001/2024-03");
    assert_eq!(club.metadata["is_group"], true);
    assert_eq!(club.title.as_deref(), Some("Book Club 📚 — March 2024"));
    let t = text(&club);
    assert!(t.contains("Bea created the group “Book Club 📚”"), "{t}");
    assert!(t.contains("Bea added Chen, Dara"), "{t}");
    assert!(t.contains("**Me** · 2024-03-01 18:06\n> Yes!"), "{t}");
    let t = text(&doc(&sink, "tg:1001/2024-04")).to_owned();
    assert!(t.contains("> my notes\n> [file: dune-notes.pdf]"), "{t}");

    let saved = doc(&sink, "tg:7/2024-01");
    assert_eq!(
        saved.title.as_deref(),
        Some("Saved Messages — January 2024")
    );
    assert!(text(&saved).contains("**Me** ·"));
}

#[tokio::test]
async fn a_result_json_file_path_works() {
    let env = Env::new();
    let replica = Replica::new();
    let path = fixtures("telegram")
        .join("ChatExport_2024-05-03")
        .join("result.json");
    env.pass(Kind::Telegram, &json!({ "path": path }), &replica)
        .await;
    assert_eq!(keys(&replica), set(&["tg:4242/2024-04", "tg:4242/2024-05"]));
}

#[tokio::test]
async fn groups_can_be_excluded_and_broken_exports_keep_items() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tg");
    copy_dir(&fixtures("telegram"), &dir);
    let env = Env::new();
    let replica = Replica::new();
    env.pass(
        Kind::Telegram,
        &json!({ "path": dir, "include_groups": false }),
        &replica,
    )
    .await;
    assert!(keys(&replica).iter().all(|k| !k.starts_with("tg:1001/")));
    assert!(keys(&replica).contains("tg:7/2024-01"));

    // A half-written export: nothing is deleted.
    std::fs::write(
        dir.join("DataExport_2024-05-03").join("result.json"),
        "{\"chats\": {\"li",
    )
    .unwrap();
    let sink = env
        .pass(Kind::Telegram, &json!({ "path": dir }), &replica)
        .await;
    assert!(deletes(&sink).is_empty(), "{:?}", sink.summary());
    assert!(keys(&replica).contains("tg:7/2024-01"));

    // The export removed: its chats go.
    std::fs::remove_dir_all(dir.join("DataExport_2024-05-03")).unwrap();
    env.pass(Kind::Telegram, &json!({ "path": dir }), &replica)
        .await;
    assert_eq!(keys(&replica), set(&["tg:4242/2024-04", "tg:4242/2024-05"]));
}

#[test]
fn validation() {
    assert!(
        TelegramFactory
            .validate(&json!({ "path": fixtures("telegram") }))
            .is_ok()
    );
    match TelegramFactory.validate(&json!({ "path": fixtures("nope") })) {
        Err(Error::NotFound(m)) => assert!(m.contains("nope")),
        other => panic!("{other:?}"),
    }
    assert!(TelegramFactory.validate(&json!({ "path": "" })).is_err());
}
