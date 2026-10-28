//! One test (or more) per supported format, plus limits and routing.

use nugget_core::Error;
use nugget_extract::{
    ExtractOptions, OcrModels, Source, detect_mime, extract, extract_with_ocr, is_supported,
};
use serde_json::json;

use crate::common::*;

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

fn paths(e: &nugget_extract::Extracted) -> Vec<Vec<String>> {
    e.sections.iter().map(|s| s.heading_path.clone()).collect()
}

#[test]
fn markdown_frontmatter_links_tags_sections() {
    let e = run("text/markdown", &fixture("note.md"));
    assert_eq!(e.title.as_deref(), Some("Garden Guide"));
    assert!(e.text.starts_with("# Guide"), "frontmatter is stripped");
    assert_eq!(
        e.tags,
        ["gardening", "outdoors", "compost", "season/spring"]
    );
    assert_eq!(
        e.links,
        [
            "Tomatoes",
            "Soil",
            "https://www.rhs.org.uk/",
            "diagram.png",
            "https://example.com/docs"
        ]
    );
    assert_eq!(
        paths(&e),
        [
            vec!["Guide"],
            vec!["Guide", "Install"],
            vec!["Guide", "Install", "Deep #heading-tag"],
            vec!["Guide", "Usage"],
        ]
    );
    for s in &e.sections {
        let heading = s.heading_path.last().unwrap();
        assert!(e.text[s.start..s.end].contains(heading.as_str()));
    }
    assert_eq!(e.metadata["frontmatter"]["rating"], 4);
    assert_eq!(e.metadata["aliases"], json!(["Allotment notes"]));
    let wl = e.metadata["wikilinks"].as_array().unwrap();
    assert_eq!(
        wl[0],
        json!({"target": "Tomatoes", "alias": "tomato plants", "embed": false})
    );
    assert_eq!(
        wl[1],
        json!({"target": "Soil", "heading": "pH", "embed": false})
    );
    assert_eq!(wl[2]["embed"], true);
    assert_eq!(wl.len(), 3, "wikilinks in code are ignored");
}

#[test]
fn markdown_title_falls_back_to_h1_then_none() {
    let e = run("text/markdown", b"intro\n\n# First\n\n# Second\n");
    assert_eq!(e.title.as_deref(), Some("First"));
    let e = run("text/markdown", b"## Only h2\ntext #tag");
    assert_eq!(e.title, None);
    assert_eq!(e.tags, ["tag"]);
    // Invalid frontmatter is reported, not fatal.
    let e = run("text/markdown", b"---\na: [unclosed\n---\nbody");
    assert!(e.metadata["frontmatter_error"].is_string());
    assert_eq!(e.text, "body");
    // Tags given as a comma-separated string.
    let e = run("text/markdown", b"---\ntags: \"a, #b c\"\n---\n");
    assert_eq!(e.tags, ["a", "b", "c"]);
}

#[test]
fn html_readable_text() {
    let e = run("text/html; charset=utf-8", &fixture("page.html"));
    assert_eq!(e.title.as_deref(), Some("Release Notes"));
    for want in [
        "Version 2.0",
        "We rewrote the engine & the UI.",
        "- Crash on start\n- Slow sync",
        "code   keeps   spacing",
        "Area | Status\nSync | Fixed",
    ] {
        assert!(e.text.contains(want), "missing {want:?} in {:?}", e.text);
    }
    for unwanted in [
        "injected",
        "color: red",
        "svg text",
        "Enable JavaScript",
        "<",
    ] {
        assert!(!e.text.contains(unwanted), "{unwanted:?} leaked");
    }
    assert_eq!(e.links, ["/home", "https://example.com/sync"]);
    assert_eq!(
        paths(&e),
        [vec!["Version 2.0"], vec!["Version 2.0", "Fixes"]]
    );
    assert_eq!(e.metadata["description"], "What changed in 2.0");
    assert_eq!(e.metadata["lang"], "en");
}

#[test]
fn html_legacy_charset_and_deep_nesting() {
    let e = run("text/html", b"<meta charset=\"iso-8859-1\"><p>caf\xe9</p>");
    assert_eq!(e.text, "café");
    let deep = "<div>".repeat(200_000) + "deep" + &"</div>".repeat(200_000);
    let e = run("text/html", deep.as_bytes());
    assert_eq!(e.text, "deep");
}

#[test]
fn plain_text_and_code() {
    let e = run(
        "text/plain; charset=iso-8859-1",
        b"Caf\xe9 at https://x.example/a.",
    );
    assert_eq!(e.text, "Café at https://x.example/a.");
    assert_eq!(e.links, ["https://x.example/a"]);
    assert_eq!(e.metadata, json!({}));

    let path = fixture_path("sample.rs");
    let mime = detect_mime(Some(&path), &fixture("sample.rs"));
    assert_eq!(mime, "text/x-rust");
    let e = extract(&mime, Source::File(&path), &ExtractOptions::default()).unwrap();
    assert_eq!(e.metadata["language"], "rust");
    assert!(e.text.contains("println!"));
    assert_eq!(e.links, ["https://www.rust-lang.org/"]);
    // Language from the MIME alone.
    let e = run("application/json", br#"{"a": 1}"#);
    assert_eq!(e.metadata["language"], "json");
    let e = run("text/x-python", b"print(1)");
    assert_eq!(e.metadata["language"], "python");
    // Text sources and UTF-16.
    let e = extract("text/plain", Source::Text("hi"), &ExtractOptions::default()).unwrap();
    assert_eq!(e.text, "hi");
    let e = run("text/plain", b"\xff\xfeh\0i\0");
    assert_eq!(e.text, "hi");
}

#[test]
fn pdf_pages_become_sections() {
    let pdf = make_pdf(
        "Test PDF",
        &[
            &["Hello from page one.", "Second line."],
            &["Page two text."],
        ],
    );
    assert_eq!(detect_mime(None, &pdf), "application/pdf");
    let e = run("application/pdf", &pdf);
    assert_eq!(e.title.as_deref(), Some("Test PDF"));
    assert_eq!(paths(&e), [vec!["Page 1"], vec!["Page 2"]]);
    let p1 = &e.text[e.sections[0].start..e.sections[0].end];
    let p2 = &e.text[e.sections[1].start..e.sections[1].end];
    assert!(p1.contains("Hello from page one."), "{p1:?}");
    assert!(p1.contains("Second line."), "{p1:?}");
    assert!(p1.find("Hello").unwrap() < p1.find("Second").unwrap());
    assert!(p2.contains("Page two text."), "{p2:?}");
    assert_eq!(e.metadata["pages"], 2);
    assert_eq!(e.metadata["author"], "Nugget Tests");
}

#[test]
fn pdf_malformed_is_invalid() {
    for bad in [&b"%PDF-1.4\ngarbage"[..], b"not a pdf", b""] {
        let r = extract(
            "application/pdf",
            Source::Bytes(bad),
            &ExtractOptions::default(),
        );
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    }
}

#[test]
fn docx_headings_tables_links() {
    let docx = make_docx();
    assert_eq!(detect_mime(None, &docx), DOCX);
    let e = run(DOCX, &docx);
    assert_eq!(e.title.as_deref(), Some("Project Plan"));
    for want in [
        "Project Plan\n\nPrepared for the team & partners.",
        "Ship the beta\tsoon.",
        "Risk | Owner\nDelay | Ann",
        "Q3 – launch.",
        "Tracker",
    ] {
        assert!(e.text.contains(want), "missing {want:?} in {:?}", e.text);
    }
    assert!(!e.text.contains("deleted words"));
    assert_eq!(
        paths(&e),
        [vec!["Goals"], vec!["Goals", "Risks"], vec!["Timeline"]]
    );
    assert!(e.text[e.sections[1].start..].starts_with("Risks"));
    assert_eq!(e.links, ["https://tracker.example.com/board"]);
    assert_eq!(e.metadata["author"], "Ann Author");
}

#[test]
fn docx_core_title_and_malformed() {
    let e = run(
        DOCX,
        &make_docx_with_body("<w:p><w:r><w:t>x</w:t></w:r></w:p>"),
    );
    assert_eq!(e.title.as_deref(), Some("Core Title"));
    assert_eq!(e.text, "x");
    let broken = make_docx_with_body("<w:p><w:r><w:t>x</w:r></w:p></w:p></oops");
    let r = extract(DOCX, Source::Bytes(&broken), &ExtractOptions::default());
    assert!(r.is_ok() || matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let r = extract(
        DOCX,
        Source::Bytes(b"PK\x03\x04junk"),
        &ExtractOptions::default(),
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let not_docx = make_zip(&[("hello.txt", b"hi")]);
    let r = extract(DOCX, Source::Bytes(&not_docx), &ExtractOptions::default());
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn xlsx_sheets_and_rows() {
    let xlsx = make_xlsx();
    assert_eq!(detect_mime(None, &xlsx), XLSX);
    let e = run(XLSX, &xlsx);
    assert_eq!(e.title.as_deref(), Some("Budget 2024"));
    assert_eq!(
        e.text,
        "Budget\nItem | Cost\nLaptop | 1299.5 |  | TRUE\nDesk & chair | 300\n\n\
         Notes & Links\nSee https://example.com/budget | formula text"
    );
    assert_eq!(paths(&e), [vec!["Budget"], vec!["Notes & Links"]]);
    assert_eq!(e.links, ["https://example.com/budget"]);
    assert_eq!(
        e.metadata["sheets"][0],
        json!({"name": "Budget", "rows": 3, "columns": 4})
    );
}

#[test]
fn csv_and_tsv() {
    let e = run("text/csv", &fixture("people.csv"));
    assert_eq!(
        e.text,
        "name | email | role\nAlice Smith | alice@example.com | Lead, Platform\n\
         Bob Jones | bob@example.com | Engineer\nCarol \"CJ\" Lee | carol@example.com | Designer (contract)"
    );
    assert_eq!(e.metadata["rows"], 4);
    assert_eq!(e.metadata["columns"], 3);
    assert_eq!(e.metadata["header"], json!(["name", "email", "role"]));
    let e = run("text/tab-separated-values", b"a\tb,c\n1\t2\n");
    assert_eq!(e.text, "a | b,c\n1 | 2");
    let e = run("text/csv", b"x;y;z\n1;2;3\n");
    assert_eq!(e.text, "x | y | z\n1 | 2 | 3");
    assert_eq!(e.metadata["delimiter"], ";");
}

#[test]
fn mail_plain_with_quotes_signature_attachment() {
    let raw = fixture("reply.eml");
    assert_eq!(detect_mime(None, &raw), "message/rfc822");
    let e = run("message/rfc822", &raw);
    assert_eq!(e.title.as_deref(), Some("Re: Lunch on Friday?"));
    assert_eq!(
        e.text,
        "Friday works. Let's try the café at noon.\n\nDetails: https://example.com/menu"
    );
    let m = &e.metadata;
    assert_eq!(m["from"], "Alice Smith <alice@example.com>");
    assert_eq!(
        m["to"],
        json!(["Bob Jones <bob@example.com>", "carol@example.com"])
    );
    assert_eq!(m["cc"], json!(["Dave <dave@example.com>"]));
    assert_eq!(m["subject"], "Re: Lunch on Friday?");
    assert_eq!(m["date"], "2024-01-15T10:30:00+01:00");
    assert_eq!(m["message_id"], "reply-2@example.com");
    assert_eq!(m["in_reply_to"], json!(["orig-1@example.com"]));
    assert_eq!(
        m["references"],
        json!(["thread-0@example.com", "orig-1@example.com"])
    );
    assert_eq!(m["signature"], "Alice Smith\nPlatform Lead");
    assert!(m["quoted"].as_str().unwrap().contains("Lunch on Friday?"));
    assert_eq!(m["attachments"][0]["name"], "menu.pdf");
    assert_eq!(m["attachments"][0]["content_type"], "application/pdf");
    assert!(!e.text.contains("JVBER") && !e.text.contains("%PDF"));
    assert_eq!(e.tags, ["Important", "Friends"]);
    assert_eq!(e.links, ["https://example.com/menu"]);
}

#[test]
fn mail_html_only() {
    let e = run("message/rfc822", &fixture("html_only.eml"));
    assert_eq!(e.title.as_deref(), Some("Weekly digest"));
    assert_eq!(e.text, "Thanks for the update!\n\nRead more at the site.");
    assert_eq!(e.metadata["signature"], "Newsletter Team");
    assert!(
        e.metadata["quoted"]
            .as_str()
            .unwrap()
            .contains("Old message text")
    );
    assert_eq!(e.links, ["https://example.org/digest/7"]);
}

#[test]
fn mail_nesting_guards() {
    // Many nested message/rfc822 parts are refused up front.
    let mut m = String::new();
    for _ in 0..100 {
        m.push_str("From: a@b.c\r\nSubject: x\r\nContent-Type: message/rfc822\r\n\r\n");
    }
    m.push_str("From: a@b.c\r\n\r\nbody");
    let r = extract(
        "message/rfc822",
        Source::Bytes(m.as_bytes()),
        &ExtractOptions::default(),
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    // Deeply nested multiparts parse without recursion.
    let mut m = String::from("From: a@b.c\r\nSubject: deep\r\n");
    for i in 0..5_000 {
        m.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"b{i}\"\r\n\r\n--b{i}\r\n"
        ));
    }
    m.push_str("Content-Type: text/plain\r\n\r\ninnermost\r\n");
    let r = extract(
        "message/rfc822",
        Source::Bytes(m.as_bytes()),
        &ExtractOptions::default(),
    );
    assert!(r.is_ok() || matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let r = extract(
        "message/rfc822",
        Source::Bytes(b""),
        &ExtractOptions::default(),
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn calendar_events() {
    let raw = fixture("meeting.ics");
    assert_eq!(detect_mime(None, &raw), "text/calendar");
    let e = run("text/calendar", &raw);
    assert_eq!(e.title.as_deref(), Some("Team"));
    for want in [
        "Event: Quarterly planning, Q2",
        "Start: 2024-05-01 09:00 (Europe/Berlin)",
        "End: 2024-05-01 10:00 (Europe/Berlin)",
        "Location: Room 4; Floor 2",
        "Organizer: Alice Smith <alice@example.com>",
        "Attendees: Jones, Bob <bob@example.com> (accepted), carol@example.com (tentative)",
        "Agenda:\n1. Roadmap\n2. Hiring",
        "Event: Offsite\nStart: 2024-05-10 (all day)",
    ] {
        assert!(e.text.contains(want), "missing {want:?} in {:?}", e.text);
    }
    assert!(!e.text.contains("Reminder"), "VALARM text leaked");
    assert_eq!(e.metadata["uid"], "evt-1@nugget.test");
    assert_eq!(e.metadata["start"], "2024-05-01T09:00:00");
    assert_eq!(e.metadata["end"], "2024-05-01T10:00:00");
    assert_eq!(e.metadata["timezone"], "Europe/Berlin");
    assert_eq!(e.metadata["events"][1]["uid"], "evt-2@nugget.test");
    assert_eq!(e.metadata["events"][1]["all_day"], true);
    assert_eq!(e.tags, ["Planning", "Work"]);
    assert_eq!(e.links, ["https://example.com/agenda"]);
    assert_eq!(paths(&e), [vec!["Quarterly planning, Q2"], vec!["Offsite"]]);

    let single = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:1\nSUMMARY:Solo\nDTSTART:20240102T030405Z\nEND:VEVENT\nEND:VCALENDAR\n";
    let e = run("text/calendar", single.as_bytes());
    assert_eq!(e.title.as_deref(), Some("Solo"));
    assert!(e.text.contains("Start: 2024-01-02 03:04:05 (UTC)"));
    assert_eq!(e.metadata["start"], "2024-01-02T03:04:05Z");
    let r = extract(
        "text/calendar",
        Source::Bytes(b"hello"),
        &ExtractOptions::default(),
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn images_need_ocr_flag_and_models() {
    let png = make_png();
    assert_eq!(detect_mime(None, &png), "image/png");
    let no_ocr = ExtractOptions {
        ocr: false,
        ..Default::default()
    };
    let missing = OcrModels::from_dir("/nonexistent/nugget-ocr-models");
    for opts in [&no_ocr, &ExtractOptions::default()] {
        let r = extract_with_ocr("image/png", Source::Bytes(&png), opts, &missing);
        assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
    }
    // Model files present but unusable: decoding still validates the image first.
    let dir = temp_dir("fake-ocr");
    let fake = OcrModels::from_dir(&dir);
    std::fs::write(&fake.detection, b"not a model").unwrap();
    std::fs::write(&fake.recognition, b"not a model").unwrap();
    let opts = ExtractOptions::default();
    let r = extract_with_ocr("image/png", Source::Bytes(&png), &opts, &fake);
    assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
    let r = extract_with_ocr(
        "image/jpeg",
        Source::Bytes(b"\xff\xd8\xffjunk"),
        &opts,
        &fake,
    );
    if cfg!(feature = "ocr") {
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    } else {
        assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn routing_and_limits() {
    assert!(is_supported("text/markdown"));
    assert!(is_supported("IMAGE/WEBP"));
    assert!(is_supported("text/x-rust"));
    assert!(!is_supported("application/octet-stream"));
    for mime in ["application/zip", "application/octet-stream", "video/mp4"] {
        let r = extract(mime, Source::Bytes(b"x"), &ExtractOptions::default());
        assert!(matches!(r, Err(Error::Unsupported(_))), "{mime}: {r:?}");
    }
    let small = ExtractOptions {
        max_input_bytes: 4,
        ..Default::default()
    };
    let r = extract("text/plain", Source::Bytes(b"12345"), &small);
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let r = extract("text/plain", Source::Text("12345"), &small);
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    let r = extract(
        "text/x-rust",
        Source::File(&fixture_path("sample.rs")),
        &small,
    );
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");

    let capped = ExtractOptions {
        max_text_chars: 20,
        ..Default::default()
    };
    let e = extract("text/markdown", Source::Bytes(&fixture("note.md")), &capped).unwrap();
    assert!(e.truncated);
    assert_eq!(e.text.chars().count(), 20);
    assert!(
        e.sections
            .iter()
            .all(|s| s.end <= e.text.len() && s.start < s.end)
    );
    for (mime, data) in [
        (XLSX, make_xlsx()),
        ("text/csv", fixture("people.csv")),
        (DOCX, make_docx()),
        ("text/calendar", fixture("meeting.ics")),
    ] {
        let e = extract(mime, Source::Bytes(&data), &capped).unwrap();
        assert!(e.truncated, "{mime}");
        assert!(e.text.chars().count() <= 20, "{mime}");
    }
    let e = extract(
        "text/plain",
        Source::Text("ünïcödé"),
        &ExtractOptions {
            max_text_chars: 3,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(e.text, "ünï");
}

#[test]
fn zip_bomb_guards() {
    // A part declaring a huge inflated size is refused before inflating.
    let mut docx = make_docx();
    let name = b"word/document.xml";
    let pos = docx
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b"PK\x01\x02")
        .map(|(i, _)| i)
        .find(|&i| docx[i + 46..].starts_with(name))
        .expect("central directory entry");
    docx[pos + 24..pos + 28].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    let r = extract(DOCX, Source::Bytes(&docx), &ExtractOptions::default());
    match r {
        Err(Error::Invalid(msg)) => assert!(msg.contains("zip bomb"), "{msg}"),
        other => panic!("expected zip bomb refusal, got {other:?}"),
    }

    // Too many entries.
    let names: Vec<String> = (0..10_001).map(|i| format!("f{i}")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    let many = make_zip(&entries);
    let r = extract(XLSX, Source::Bytes(&many), &ExtractOptions::default());
    match r {
        Err(Error::Invalid(msg)) => assert!(msg.contains("entries"), "{msg}"),
        other => panic!("expected entry-count refusal, got {other:?}"),
    }
}

#[test]
fn markdown_pathological_nesting() {
    let quotes = "> ".repeat(20_000) + "x";
    let e = run("text/markdown", quotes.as_bytes());
    assert!(e.text.ends_with('x'));
    let lists = "- ".repeat(20_000) + "x";
    run("text/markdown", lists.as_bytes());
    let yaml = format!("---\n{}x\n---\nbody", "- ".repeat(20_000));
    let e = run("text/markdown", yaml.as_bytes());
    assert_eq!(e.text, "body");
    assert!(e.metadata["frontmatter_error"].is_string());
}
