//! Full-text search through [`Store::search_text`]: query syntax (phrases, prefixes, exclusions,
//! inert FTS5 operators), scripts `unicode61` does not segment (CJK, Thai), and everyday
//! spelling variants (QA GG7-7).

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

/// Keys of the hits in rank order.
fn ranked(s: &Store, q: &str) -> Vec<String> {
    s.search_text(q, 10)
        .unwrap()
        .iter()
        .map(|h| h.item.key.as_str().to_owned())
        .collect()
}

/// Keys of the hits, sorted.
fn keys(s: &Store, q: &str) -> Vec<String> {
    let mut k = ranked(s, q);
    k.sort_unstable();
    k.dedup();
    k
}

fn hit(s: &Store, q: &str, key: &str) -> Hit {
    s.search_text(q, 10)
        .unwrap()
        .into_iter()
        .find(|h| h.item.key.as_str() == key)
        .unwrap_or_else(|| panic!("{q:?} did not find {key}"))
}

fn syntax_corpus() -> Store {
    store_with(&[
        (
            "phrase",
            "Notes",
            "the boiler pressure valve needs replacing soon",
        ),
        (
            "scattered",
            "Misc",
            "pressure at work; the valve on the bike; a boiler",
        ),
        (
            "nuggets",
            "Prospecting",
            "we found three gold nuggets in the creek",
        ),
        ("other", "Groceries", "milk, eggs and bread"),
    ])
}

#[test]
fn phrases_prefixes_and_exclusions() {
    let s = syntax_corpus();
    assert_eq!(keys(&s, "\"pressure valve\""), ["phrase"]);
    assert_eq!(keys(&s, "“pressure valve”"), ["phrase"]);
    assert!(keys(&s, "\"valve pressure\"").is_empty(), "word order");
    assert_eq!(keys(&s, "pressure valve"), ["phrase", "scattered"]);
    assert_eq!(
        ranked(&s, "\"pressure valve\" boiler"),
        ["phrase", "scattered"]
    );
    assert_eq!(keys(&s, "\"pressure valve\" milk"), ["other", "phrase"]);
    assert_eq!(keys(&s, "\"found three gold\""), ["nuggets"]);

    assert_eq!(keys(&s, "nug*"), ["nuggets"]);
    assert_eq!(keys(&s, "prospect*"), ["nuggets"], "title prefix");
    assert!(keys(&s, "nug").is_empty());
    assert!(
        keys(&s, "b*").is_empty(),
        "one-character prefixes are words"
    );

    assert_eq!(keys(&s, "pressure -bike"), ["phrase"]);
    assert_eq!(keys(&s, "valve -\"on the bike\""), ["phrase"]);
    assert!(keys(&s, "-bike").is_empty());
    assert_eq!(keys(&s, "three-gold"), ["nuggets"]);

    let hybrid = |q: &str| -> Vec<String> {
        let hits = s.search_hybrid(q, None, 10).unwrap();
        hits.iter()
            .map(|h| h.item.key.as_str().to_owned())
            .collect()
    };
    assert_eq!(hybrid("\"pressure valve\""), ["phrase"]);
    assert_eq!(hybrid("nugg*"), ["nuggets"]);
}

#[test]
fn operators_stay_inert_end_to_end() {
    let s = syntax_corpus();
    for q in [
        "milk AND NOT",
        "NEAR(milk eggs",
        "text:milk",
        "^milk",
        "milk\"",
        "\"milk",
        "milk) OR (",
        "{text}: milk",
        "milk + * -",
    ] {
        assert_eq!(keys(&s, q), ["other"], "query {q:?}");
    }
    for q in ["", "\"\"", "*", "-", "\"(*)-^", "-\"a b\""] {
        assert!(keys(&s, q).is_empty(), "query {q:?}");
    }
}

const JA: &str = "星商事 ロゴ制作 お見積り(確定) 金額 ¥120,000 納期 2026年11月18日";

fn cjk_corpus() -> Store {
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
fn unsegmented_and_spaced_scripts_are_found() {
    let s = cjk_corpus();
    for (q, key) in [
        ("お見積り", "ja"),
        ("星商事", "ja"),
        ("納期", "ja"),
        ("定例会議", "ja-chat"),
        ("水曜日", "ja-chat"),
        ("上海", "zh"),
        ("预算", "zh"),
        ("회의", "ko"),
        ("ประชุม", "th"),
        ("ลูกค้า", "th"),
        ("invoice", "en"),
        ("uber", "de"),
        ("proximo", "es"),
        ("الفاتورة", "ar"),
        ("見積書", "title"),
        ("送付", "title"),
        ("ご相談", "ja"),
    ] {
        assert_eq!(keys(&s, q), [key], "{q}");
    }
    assert!(keys(&s, "金").contains(&"ja".to_owned()));
    assert!(keys(&s, "積見").is_empty());
    assert_eq!(keys(&s, "Hoshi 見積り"), ["en", "ja"]);
    assert_eq!(keys(&s, "\"Hoshi Shoji\" -見積り"), ["en"]);
    assert!(keys(&s, "見積り -金額").is_empty());
}

#[test]
fn snippets_show_the_original_text() {
    let s = cjk_corpus();
    assert_eq!(hit(&s, "お見積り", "ja").snippet, JA);
    assert_eq!(hit(&s, "Rechnung", "de").snippet, "Rechnung über 120 Euro");

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
    assert_eq!(
        ranked(
            &s,
            "先日お送りいただいたロゴ制作のお見積りについて確認させてください"
        )[0],
        "quote"
    );
    assert_eq!(
        ranked(&s, "来週の定例会議の時間って変更になったんだっけ")[0],
        "meeting"
    );
}

const AR_CHAIRS: &str = "أحضروا الخيمة والكراسي معكم يوم الجمعة";
const DE_ADDRESS: &str = "Unser Büro ist in der Königstraße 14, 70173 Stuttgart.";
const DE_COMMITTEE: &str = "Die Präsentation für den Lenkungsausschuss ist fertig.";

fn variants_corpus() -> Store {
    store_with(&[
        (
            "ja-led",
            "打ち合わせ",
            "来春オープン予定の新店舗へのＬＥＤ照明導入の件につきまして",
        ),
        (
            "ja-campaign",
            "企画書",
            "本日の定例でお話しした年末キャンペーンの件",
        ),
        ("ar-chairs", "كشتة", AR_CHAIRS),
        ("ar-invoice", "طباعة", "مرفق لكم فاتورة الطباعة رقم ٢٠٤"),
        ("ar-confirm", "اجتماع", "نرجو منكم التكرّم بتأكيد الحضور"),
        ("de-address", "Terminanfrage", DE_ADDRESS),
        ("de-committee", "Präsentation", DE_COMMITTEE),
        ("hi-train", "यात्रा", "हम रेलगाड़ी से ऋषिकेश जाएंगे"),
        ("hi-floor", "बैठक", "बैठक तीसरी मंज़िल पर होगी"),
        (
            "es-recipe",
            "Paella",
            "Añade el tomate rallado y sofreírlo cinco minutos.",
        ),
        ("en", "Offsite", "We booked the venue for the offsite"),
    ])
}

#[test]
fn spelling_variants_match() {
    let s = variants_corpus();
    for (q, key) in [
        ("LED照明", "ja-led"),
        ("ＬＥＤ照明", "ja-led"),
        ("led", "ja-led"),
        ("ｷｬﾝﾍﾟｰﾝ", "ja-campaign"),
        ("キャンペーン", "ja-campaign"),
        ("والكراسي", "ar-chairs"),
        ("كراسي", "ar-chairs"),
        ("فاتوره", "ar-invoice"),
        ("تاكيد", "ar-confirm"),
        ("التكرم", "ar-confirm"),
        ("Königstraße", "de-address"),
        ("Konigstrasse", "de-address"),
        ("Koenigstrasse", "de-address"),
        ("Buero", "de-address"),
        ("\"Konigstrasse 14\"", "de-address"),
        ("Ausschuss", "de-committee"),
        ("lenkungs*", "de-committee"),
        ("गाड़ी", "hi-train"),
        ("गा\u{0921}\u{093C}ी", "hi-train"),
        ("sofreir", "es-recipe"),
        ("मंजिल", "hi-floor"),
        ("booking", "en"),
        ("venues", "en"),
    ] {
        assert_eq!(keys(&s, q), [key], "{q}");
    }
    assert_eq!(hit(&s, "كراسي", "ar-chairs").snippet, AR_CHAIRS);
    assert_eq!(hit(&s, "Koenigstrasse", "de-address").snippet, DE_ADDRESS);
    assert_eq!(hit(&s, "Ausschuss", "de-committee").snippet, DE_COMMITTEE);
    assert!(keys(&s, "Ausschuss -Präsentation").is_empty());
    assert!(keys(&s, "aus").is_empty());
}

#[test]
fn word_parts_rank_below_whole_words() {
    let s = store_with(&[
        ("compound", "a", "Der Lenkungsausschuss tagt morgen"),
        ("word", "b", "Der Ausschuss tagt morgen"),
        ("x1", "c", "Die Sitzung beginnt pünktlich"),
        ("x2", "d", "Bitte Unterlagen mitbringen"),
        ("x3", "e", "Rechnung anbei gesendet"),
        ("x4", "f", "Termine stehen fest"),
    ]);
    assert_eq!(ranked(&s, "Ausschuss"), ["word", "compound"]);
}

#[test]
fn edits_and_deletes_keep_the_indexes_in_step() {
    let s = cjk_corpus();
    let it = NewItem::new("files:test", "zh", content_hash("x")).with_title("预算表");
    s.upsert_item(&it).unwrap();
    s.replace_chunks(
        &it.item,
        &[NewChunk::text("北京的天气 Der Prüfungsausschuss")],
    )
    .unwrap();
    assert!(keys(&s, "上海").is_empty());
    assert_eq!(keys(&s, "北京"), ["zh"]);
    assert_eq!(keys(&s, "预算"), ["zh"], "title");
    assert_eq!(keys(&s, "Ausschuss"), ["zh"]);
    assert_eq!(keys(&s, "Pruefungsausschuss"), ["zh"]);
    s.delete_item(&it.item).unwrap();
    for q in ["北京", "预算", "Ausschuss"] {
        assert!(keys(&s, q).is_empty(), "{q}");
    }
    assert!(s.check_consistency().unwrap().ok);
}
