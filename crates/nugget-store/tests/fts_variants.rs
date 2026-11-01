//! Full-text search finds everyday spelling variants (QA GG7-7): width, Arabic letter forms and
//! proclitics, German ß and umlauts, and words inside compounds or with attached clitics.

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

// Sentences after the QA corpus (qa/GG7/corpus).
const JA_LED: &str = "来春オープン予定の新店舗へのＬＥＤ照明導入の件につきまして";
const JA_CAMPAIGN: &str = "本日の定例でお話しした年末キャンペーンの件";
const AR_CHAIRS: &str = "أحضروا الخيمة والكراسي معكم يوم الجمعة";
const AR_INVOICE: &str = "مرفق لكم فاتورة الطباعة رقم ٢٠٤";
const AR_CONFIRM: &str = "نرجو منكم التكرّم بتأكيد الحضور";
const DE_ADDRESS: &str = "Unser Büro ist in der Königstraße 14, 70173 Stuttgart.";
const DE_COMMITTEE: &str = "Die Präsentation für den Lenkungsausschuss ist fertig.";
const HI_TRAIN: &str = "हम रेलगाड़ी से ऋषिकेश जाएंगे";
const HI_FLOOR: &str = "बैठक तीसरी मंज़िल पर होगी";
const ES_RECIPE: &str = "Añade el tomate rallado y sofreírlo cinco minutos.";
const EN: &str = "We booked the venue for the offsite";

fn corpus() -> Store {
    store_with(&[
        ("ja-led", "打ち合わせ", JA_LED),
        ("ja-campaign", "企画書", JA_CAMPAIGN),
        ("ar-chairs", "كشتة", AR_CHAIRS),
        ("ar-invoice", "طباعة", AR_INVOICE),
        ("ar-confirm", "اجتماع", AR_CONFIRM),
        ("de-address", "Terminanfrage", DE_ADDRESS),
        ("de-committee", "Präsentation", DE_COMMITTEE),
        ("hi-train", "यात्रा", HI_TRAIN),
        ("hi-floor", "बैठक", HI_FLOOR),
        ("es-recipe", "Paella", ES_RECIPE),
        ("en", "Offsite", EN),
    ])
}

#[test]
fn width_variants_match() {
    let s = corpus();
    for q in ["LED照明", "ＬＥＤ照明", "led", "照明"] {
        assert_eq!(keys(&s, q), ["ja-led"], "{q}");
    }
    for q in ["ｷｬﾝﾍﾟｰﾝ", "キャンペーン"] {
        assert_eq!(keys(&s, q), ["ja-campaign"], "{q}");
    }
    assert_eq!(hit(&s, "LED照明", "ja-led").snippet, JA_LED);
}

#[test]
fn arabic_proclitics_and_letter_forms_match() {
    let s = corpus();
    for q in ["والكراسي", "الكراسي", "كراسي"] {
        assert_eq!(keys(&s, q), ["ar-chairs"], "{q}");
    }
    for q in ["فاتورة", "فاتوره"] {
        assert_eq!(keys(&s, q), ["ar-invoice"], "{q}");
    }
    // Hamza forms and tashkeel: تأكيد written without hamza, التكرّم without shadda.
    assert_eq!(keys(&s, "تاكيد"), ["ar-confirm"]);
    assert_eq!(keys(&s, "التكرم"), ["ar-confirm"]);
    assert_eq!(keys(&s, "الحضور"), ["ar-confirm"]);
    assert_eq!(hit(&s, "كراسي", "ar-chairs").snippet, AR_CHAIRS);
    assert_eq!(hit(&s, "فاتوره", "ar-invoice").snippet, AR_INVOICE);
}

#[test]
fn german_sharp_s_and_umlauts_match_both_ways() {
    let s = corpus();
    for q in [
        "Königstraße",
        "Konigstrasse",
        "Koenigstrasse",
        "königstrasse",
    ] {
        assert_eq!(keys(&s, q), ["de-address"], "{q}");
    }
    for q in ["Büro", "Buro", "Buero"] {
        assert_eq!(keys(&s, q), ["de-address"], "{q}");
    }
    // A phrase across a word with variants still matches.
    assert_eq!(keys(&s, "\"Königstraße 14\""), ["de-address"]);
    assert_eq!(keys(&s, "\"Konigstrasse 14\""), ["de-address"]);
    let snip = hit(&s, "Koenigstrasse", "de-address").snippet;
    assert_eq!(snip, DE_ADDRESS);
}

#[test]
fn compound_parts_and_attached_clitics_match() {
    let s = corpus();
    // German compound tail.
    assert_eq!(keys(&s, "Ausschuss"), ["de-committee"]);
    assert_eq!(keys(&s, "lenkungs*"), ["de-committee"]);
    // Hindi compound tail (रेल + गाड़ी), with the nukta precomposed or not.
    assert_eq!(keys(&s, "गाड़ी"), ["hi-train"]);
    assert_eq!(keys(&s, "गा\u{0921}\u{093C}ी"), ["hi-train"]);
    // Spanish verb with an attached pronoun.
    assert_eq!(keys(&s, "sofreír"), ["es-recipe"]);
    assert_eq!(keys(&s, "sofreir"), ["es-recipe"]);
    let snip = hit(&s, "Ausschuss", "de-committee").snippet;
    assert_eq!(snip, DE_COMMITTEE);
    // Exclusions still apply to word-part matches.
    assert!(keys(&s, "Ausschuss -Präsentation").is_empty());
    // Short words are not looked up inside others.
    assert!(keys(&s, "aus").is_empty());
}

#[test]
fn hindi_nukta_spellings_match() {
    let s = corpus();
    assert_eq!(keys(&s, "मंज़िल"), ["hi-floor"]);
    assert_eq!(keys(&s, "मंजिल"), ["hi-floor"], "without the nukta");
}

#[test]
fn english_stemming_still_works() {
    let s = corpus();
    for q in ["book", "booked", "booking", "venues"] {
        assert_eq!(keys(&s, q), ["en"], "{q}");
    }
    assert_eq!(hit(&s, "book", "en").snippet, EN);
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
    let found: Vec<String> = s
        .search_text("Ausschuss", 10)
        .unwrap()
        .iter()
        .map(|h| h.item.key.as_str().to_owned())
        .collect();
    assert_eq!(found, ["word", "compound"]);
}

#[test]
fn edits_and_deletes_keep_the_suffix_index_in_step() {
    let s = corpus();
    let it = NewItem::new("files:test", "de-committee", content_hash("x"))
        .with_title("Protokoll")
        .with_text("Der Prüfungsausschuss");
    s.upsert_item(&it).unwrap();
    s.replace_chunks(&it.item, &[NewChunk::text("Der Prüfungsausschuss tagt")])
        .unwrap();
    assert_eq!(keys(&s, "Ausschuss"), ["de-committee"]);
    assert!(keys(&s, "Lenkungsausschuss").is_empty());
    assert_eq!(keys(&s, "Pruefungsausschuss"), ["de-committee"]);
    s.delete_item(&it.item).unwrap();
    assert!(keys(&s, "Ausschuss").is_empty());
    assert!(s.check_consistency().unwrap().ok);
}
