//! Full-text search over scripts `unicode61` does not segment (Japanese, Chinese, Korean, Thai),
//! next to languages it already handles, through [`Store::search_text`].

use nugget_store::{Hit, NewChunk, NewItem, Store, content_hash};

fn store_with(docs: &[(&str, &str, &str)]) -> Store {
    let s = Store::open_in_memory().expect("open in-memory store");
    for (key, title, body) in docs {
        let it = NewItem::new("files:test", *key, content_hash(body))
            .with_title(*title)
            .with_text(*body);
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::text(*body)])
            .unwrap();
    }
    s
}

fn keys(s: &Store, q: &str) -> Vec<String> {
    let mut k: Vec<String> = s
        .search_text(q, 10)
        .unwrap()
        .iter()
        .map(|h| h.item.key.as_str().to_owned())
        .collect();
    k.sort_unstable();
    k
}

fn hit(s: &Store, q: &str, key: &str) -> Hit {
    s.search_text(q, 10)
        .unwrap()
        .into_iter()
        .find(|h| h.item.key.as_str() == key)
        .unwrap_or_else(|| panic!("{q:?} did not find {key}"))
}

const JA: &str = "星商事 ロゴ制作 お見積り(確定) 金額 ¥120,000 納期 2026年11月18日";

fn corpus() -> Store {
    store_with(&[
        ("ja", "Re: ロゴ制作のご相談", JA),
        (
            "ja-chat",
            "連絡",
            "来週の定例会議は水曜日に変更になりました",
        ),
        ("zh", "会议", "我们下周二在上海开会讨论预算"),
        ("ko", "메모", "다음 주 회의 일정"),
        ("th", "บันทึก", "พรุ่งนี้มีประชุมกับลูกค้า"),
        ("en", "Invoice", "The invoice from Hoshi Shoji is attached"),
        ("de", "Abrechnung", "Rechnung über 120 Euro"),
        ("es", "Factura", "La factura vence el próximo martes"),
        ("ar", "فاتورة", "الفاتورة مستحقة يوم الثلاثاء"),
        (
            "title",
            "見積書の送付について",
            "please see the attached document",
        ),
    ])
}

#[test]
fn japanese_words_are_found() {
    let s = corpus();
    for q in ["お見積り", "見積り", "星商事", "ロゴ制作", "納期", "金額"] {
        assert_eq!(keys(&s, q), ["ja"], "{q}");
    }
    // Words inside an unspaced sentence.
    for q in ["定例会議", "水曜日", "変更"] {
        assert_eq!(keys(&s, q), ["ja-chat"], "{q}");
    }
    // A lone character matches the bigrams it starts.
    assert!(keys(&s, "金").contains(&"ja".to_owned()));
    // Character order matters.
    assert!(keys(&s, "積見").is_empty());
}

#[test]
fn chinese_korean_and_thai_words_are_found() {
    let s = corpus();
    assert_eq!(keys(&s, "上海"), ["zh"]);
    assert_eq!(keys(&s, "预算"), ["zh"]);
    assert_eq!(keys(&s, "회의"), ["ko"]);
    assert_eq!(keys(&s, "ประชุม"), ["th"]);
    assert_eq!(keys(&s, "ลูกค้า"), ["th"]);
}

#[test]
fn spaced_languages_still_work() {
    let s = corpus();
    assert_eq!(keys(&s, "invoice"), ["en"]);
    assert_eq!(keys(&s, "Rechnung"), ["de"]);
    assert_eq!(keys(&s, "uber"), ["de"], "diacritics folded");
    assert_eq!(keys(&s, "factura"), ["es"]);
    assert_eq!(keys(&s, "proximo"), ["es"]);
    assert_eq!(keys(&s, "الفاتورة"), ["ar"]);
    assert_eq!(hit(&s, "Rechnung", "de").snippet, "Rechnung über 120 Euro");
}

#[test]
fn mixed_query_finds_both_scripts() {
    let s = corpus();
    assert_eq!(keys(&s, "Hoshi 見積り"), ["en", "ja"]);
    assert_eq!(keys(&s, "\"Hoshi Shoji\" -見積り"), ["en"]);
    assert!(keys(&s, "見積り -金額").is_empty());
}

#[test]
fn titles_are_found() {
    let s = corpus();
    assert_eq!(keys(&s, "見積書"), ["title"]);
    assert_eq!(keys(&s, "送付"), ["title"]);
    // Title and body both match: still one hit.
    assert_eq!(keys(&s, "ご相談"), ["ja"]);
}

#[test]
fn snippets_show_the_original_text() {
    let s = corpus();
    assert_eq!(hit(&s, "お見積り", "ja").snippet, JA);
    assert_eq!(
        hit(&s, "上海", "zh").snippet,
        "我们下周二在上海开会讨论预算"
    );

    // A match deep in a long chunk: the snippet is a window around it.
    let long = format!(
        "{}。担当者は星商事の田中様です。{}",
        "前置きの文章です。".repeat(40),
        "後書きの文章です。".repeat(40)
    );
    let s = store_with(&[("long", "メモ", &long)]);
    let snip = hit(&s, "田中", "long").snippet;
    assert!(snip.contains("田中様"), "{snip}");
    assert!(snip.starts_with('…') && snip.ends_with('…'), "{snip}");
    assert!(!snip.contains("田中 中様"), "no bigrams: {snip}");
}

#[test]
fn edits_and_deletes_keep_the_index_in_step() {
    let s = corpus();
    let it = NewItem::new("files:test", "zh", content_hash("x"))
        .with_title("预算表")
        .with_text("北京的天气");
    s.upsert_item(&it).unwrap();
    s.replace_chunks(&it.item, &[NewChunk::text("北京的天气")])
        .unwrap();
    assert!(keys(&s, "上海").is_empty());
    assert_eq!(keys(&s, "北京"), ["zh"]);
    assert_eq!(keys(&s, "预算"), ["zh"], "title");
    s.delete_item(&it.item).unwrap();
    assert!(keys(&s, "北京").is_empty());
    assert!(keys(&s, "预算").is_empty());
}

/// A long unspaced sentence (a thread a draft replies to) finds the chunks sharing part of it,
/// best first, not only a verbatim copy (GG1-9).
#[test]
fn sentences_find_partly_matching_chunks() {
    let s = store_with(&[
        (
            "quote",
            "お見積り",
            "星商事様 ロゴ制作のお見積り(確定)は¥120,000です",
        ),
        (
            "meeting",
            "定例会議",
            "定例会議は水曜日10時からに変更します",
        ),
        ("other", "メモ", "週末は京都へ行きました"),
    ]);
    let top = |q: &str| -> String {
        s.search_text(q, 10).unwrap()[0]
            .item
            .key
            .as_str()
            .to_owned()
    };
    assert_eq!(
        top("先日お送りいただいたロゴ制作のお見積りについて確認させてください"),
        "quote"
    );
    assert_eq!(
        top("来週の定例会議の時間って変更になったんだっけ"),
        "meeting"
    );
    // Short words stay phrases: character order still matters.
    assert!(keys(&s, "積見").is_empty());
}
