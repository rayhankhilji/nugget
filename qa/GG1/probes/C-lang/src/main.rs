//! GG1 area C (languages) probe: feeds German, Spanish, Japanese and Arabic text through the
//! product's post-processor, length band, streaming grounding check and voice normalisation, and
//! prints what comes out. Deterministic; no model. Run from the repo root:
//!
//! ```sh
//! cd qa/GG1/probes/C-lang && CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/user/gg1-target \
//!   cargo run --release -q
//! ```

use nugget_core::ipc::{AppInfo, DraftContext, FieldInfo};
use nugget_draft::grounding::Corpus;
use nugget_draft::post::{PostOptions, PostProcessor};
use nugget_draft::{FieldKind, Grounder, GroundingCheck, LengthBand, post_options};
use nugget_voice::{StyleStats, VoiceHints, normalize_greeting, normalize_signoff};

/// Stream `text` through a post-processor in 3-character pieces (as the scripted provider does).
fn post(text: &str, opts: PostOptions) -> String {
    let mut p = PostProcessor::new(opts);
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    for piece in chars.chunks(3) {
        let s: String = piece.iter().collect();
        let e = p.push(&s);
        out.push_str(&e.text);
        if e.stop {
            return out;
        }
    }
    out.push_str(&p.finish());
    out
}

fn ctx_with_value(value: &str, caret: Option<u32>) -> DraftContext {
    DraftContext {
        app: AppInfo {
            id: Some("com.apple.mail".into()),
            name: "Mail".into(),
            window_title: None,
            url: None,
        },
        field: FieldInfo {
            role: "text_area".into(),
            label: Some("Message body".into()),
            value: value.into(),
            selection: caret.map(|c| (c, c)),
            ..Default::default()
        },
        subject: Some("Re: x".into()),
        ..Default::default()
    }
}

fn band(kind: FieldKind, median: f32, samples: u32) -> LengthBand {
    let voice = VoiceHints {
        stats: StyleStats {
            samples,
            median_words: median,
            ..Default::default()
        },
        ..Default::default()
    };
    LengthBand::for_draft(kind, &voice)
}

/// Stream a draft through the sentence-level grounding check with `corpus_text` as the only
/// supported source; returns (output, report, bytes pushed before the first output).
fn ground(corpus_text: &str, draft: &str) -> (String, nugget_draft::GroundingReport, Option<usize>) {
    let mut c = Corpus::default();
    c.add(corpus_text);
    let mut g = Grounder::new(c).with_check(GroundingCheck::Sentences);
    let mut out = String::new();
    let mut pushed = 0usize;
    let mut first = None;
    let chars: Vec<char> = draft.chars().collect();
    for piece in chars.chunks(3) {
        let s: String = piece.iter().collect();
        pushed += s.len();
        let o = g.push(&s);
        if first.is_none() && !o.trim().is_empty() {
            first = Some(pushed);
        }
        out.push_str(&o);
    }
    let o = g.finish();
    if first.is_none() && !o.trim().is_empty() {
        first = Some(pushed);
    }
    out.push_str(&o);
    (out, g.report().clone(), first)
}

fn main() {
    println!("## 1. post_options: does text already in the field switch on duplicate stripping?");
    let greet_cases = [
        "Hi Sabine,\n\n",
        "Hallo Frau Weber,\n\n",
        "Sehr geehrte Frau Weber,\n\n",
        "Guten Tag Herr Albrecht,\n\n",
        "Liebe Lena,\n\n",
        "Estimado Sr. Méndez:\n\n",
        "Buenos días, Ramón:\n\n",
        "山田様\n\n",
        "السيد كريم منصور المحترم،\n\n",
        "مرحبًا رامي،\n\n",
    ];
    for v in greet_cases {
        let o = post_options(FieldKind::EmailBody, &ctx_with_value(v, None), band(FieldKind::EmailBody, 0.0, 0));
        println!("  before caret {:<40} strip_greeting={}", format!("{v:?}"), o.strip_greeting);
    }
    let sign_cases = [
        "Best,\nRobin",
        "Mit freundlichen Grüßen\nKlara Brandt",
        "Viele Grüße\nKlara",
        "Beste Grüße\nKlara",
        "LG\nKlara",
        "Un saludo,\nLucía Ortega",
        "Atentamente,\nLucía Ortega",
        "Saludos,\nLucía",
        "何卒よろしくお願いいたします。\n水野",
        "مع خالص التحية،\nليلى حداد",
    ];
    for s in sign_cases {
        let v = format!("\n\n{s}");
        let o = post_options(FieldKind::EmailBody, &ctx_with_value(&v, Some(0)), band(FieldKind::EmailBody, 0.0, 0));
        println!("  after caret  {:<45} strip_signoff={}", format!("{s:?}"), o.strip_signoff);
    }

    println!("\n## 2. Duplicate greeting removal (strip_greeting forced on), model output streamed");
    let on = PostOptions { plain_text: true, strip_greeting: true, ..Default::default() };
    for d in [
        "Hallo Frau Weber,\n\ndie Lieferung geht an die Hafenstraße 12.",
        "Sehr geehrte Frau Weber,\n\ndie Lieferung geht an die Hafenstraße 12.",
        "Estimado Sr. Méndez:\n\nLe confirmo la fecha.",
        "Hola, Ana:\n\nYa he hablado con el fontanero.",
        "Hallo, Jonas,\n\ndanke für die Folien.",
        "Hi, Sam,\n\nThursday works.",
        "山田様\n\nいつもお世話になっております。",
        "السيد كريم منصور المحترم،\n\nشكرًا لرسالتكم.",
    ] {
        println!("  in : {d:?}\n  out: {:?}", post(d, on.clone()));
    }

    println!("\n## 3. Duplicate sign-off removal (strip_signoff forced on)");
    let on = PostOptions { plain_text: true, strip_signoff: true, ..Default::default() };
    for d in [
        "Es sind 14 Paletten.\n\nBest,\nKlara",
        "Es sind 14 Paletten.\n\nMit freundlichen Grüßen\nKlara Brandt",
        "Es sind 14 Paletten.\n\nViele Grüße\nKlara",
        "Le confirmo la fecha.\n\nUn saludo,\nLucía Ortega",
        "Le confirmo la fecha.\n\nAtentamente,\nLucía Ortega",
        "承知いたしました。\n\n何卒よろしくお願いいたします。\n水野",
        "شكرًا لرسالتكم.\n\nمع خالص التحية،\nليلى حداد",
    ] {
        println!("  in : {d:?}\n  out: {:?}", post(d, on.clone()));
    }

    println!("\n## 4. Preambles, postambles and wrapping quotes in other languages");
    let plain = PostOptions { plain_text: true, ..Default::default() };
    for d in [
        "Sure, here's a draft:\n\nMonday at 10 works for me.",
        "Hier ist ein Entwurf für Ihre Antwort:\n\nMontag um 10 Uhr passt mir gut.",
        "Gerne, hier ist eine mögliche Antwort:\n\nMontag um 10 Uhr passt mir gut.",
        "Claro, aquí tienes un borrador:\n\nSí, viene el jueves.",
        "以下は返信の下書きです:\n「うん、さっき入れておいたよ!」",
        "「うん、さっき入れておいたよ!」",
        "إليك مسودة الرد:\n\nتمام، بشوفها بعد الاجتماع.",
        "Montag passt.\n\nLet me know if you need anything else.",
        "Montag passt.\n\nLassen Sie mich wissen, ob ich etwas anpassen soll.",
        "\"Montag passt.\"",
        "„Montag passt.“",
        "«Sí, viene el jueves.»",
    ] {
        println!("  in : {d:?}\n  out: {:?}", post(d, plain.clone()));
    }

    println!("\n## 5. Length band: hard_max cut, English vs Japanese of the same length");
    let b = band(FieldKind::EmailBody, 0.0, 0);
    println!("  email default band: {b:?} max_tokens={}", b.max_tokens());
    let opts = PostOptions { plain_text: true, max_words: Some(b.hard_max), ..Default::default() };
    let en = "We reviewed the three colour options and can deliver all of them with print and web checks. ".repeat(20);
    let ja = "社内で検討を重ねた結果、ご提案の三案すべてを作成することが可能でございます。それぞれの案について、印刷時の再現性やウェブ上での見え方を確認したうえでお送りいたします。".repeat(8);
    let en_out = post(&en, opts.clone());
    let ja_out = post(&ja, opts.clone());
    println!(
        "  en: in {} words / {} chars -> out {} words / {} chars",
        en.split_whitespace().count(), en.chars().count(),
        en_out.split_whitespace().count(), en_out.chars().count()
    );
    println!(
        "  ja: in {} whitespace-words / {} chars -> out {} chars (cut: {})",
        ja.split_whitespace().count(), ja.chars().count(), ja_out.chars().count(),
        ja_out.chars().count() < ja.chars().count()
    );
    println!("  bands from voice stats (median of whitespace words over >=5 samples):");
    for (label, kind, median) in [
        ("ja chat (median 1 'word')", FieldKind::ChatMessage, 1.0),
        ("ja keigo email (median 6 'words': one per line)", FieldKind::EmailBody, 6.0),
        ("de chat (median 12 words)", FieldKind::ChatMessage, 12.0),
    ] {
        let b = band(kind, median, 9);
        println!("    {label:<50} target={} hard_max={} max_tokens={}", b.target, b.hard_max, b.max_tokens());
    }

    println!("\n## 6. Grounding (GroundingCheck::Sentences): local number and date formats");
    let cases: &[(&str, &str, &str)] = &[
        ("de correct", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Der Betrag ist 1.234,50 € und fällig am 18.11.2026. Bei Fragen melden Sie sich gern."),
        ("de wrong amount", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Der Betrag ist 1.243,50 € brutto. Bei Fragen melden Sie sich gern."),
        ("de wrong date", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Das Zahlungsziel ist der 28.11.2026. Bei Fragen melden Sie sich gern."),
        ("de wrong date, short form", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Das Zahlungsziel ist der 28.11. Bei Fragen melden Sie sich gern."),
        ("de correct, no thousands dot", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Der Betrag ist 1234,50 € brutto. Bei Fragen melden Sie sich gern."),
        ("de correct, written English-style", "Rechnungsbetrag: 1.234,50 €. Zahlungsziel: 18.11.2026.",
         "Der Betrag ist €1,234.50 brutto. Bei Fragen melden Sie sich gern."),
        ("de wrong amount 1.234,59 vs 1.234,50", "Rechnungsbetrag: 1.234,50 €.",
         "Der Betrag ist 1.234,59 € brutto. Bei Fragen melden Sie sich gern."),
        ("es correct", "Presupuesto: 4.850,00 €. Entrega: 18 de noviembre de 2026.",
         "El presupuesto es de 4.850,00 € y la entrega es el 18 de noviembre de 2026. Un saludo."),
        ("es wrong day", "Presupuesto: 4.850,00 €. Entrega: 18 de noviembre de 2026.",
         "La entrega es el 28 de noviembre. Un saludo."),
        ("es wrong month", "Presupuesto: 4.850,00 €. Entrega: 18 de noviembre de 2026.",
         "La entrega es el 18 de diciembre. Un saludo."),
        ("ja correct", "合計 ¥120,000(税込)。納品日:2026年11月18日(水)。",
         "お見積りは合計¥120,000(税込)です。納品日は2026年11月18日です。よろしくお願いいたします。"),
        ("ja wrong yen symbol amount", "合計 ¥120,000(税込)。納品日:2026年11月18日(水)。",
         "お見積りは合計¥130,000(税込)です。よろしくお願いいたします。"),
        ("ja wrong amount with 円", "合計 ¥120,000(税込)。納品日:2026年11月18日(水)。",
         "お見積りは合計130,000円(税込)です。よろしくお願いいたします。"),
        ("ja wrong date", "合計 ¥120,000(税込)。納品日:2026年11月18日(水)。",
         "納品日は2026年11月28日です。よろしくお願いいたします。"),
        ("ar correct, Arabic-Indic", "المبلغ الإجمالي: ١٢٬٣٤٥ درهم. موعد التسليم: ١٨/١١/٢٠٢٦.",
         "المبلغ الإجمالي هو ١٢٬٣٤٥ درهم. موعد التسليم ١٨/١١/٢٠٢٦. مع خالص التحية."),
        ("ar wrong, Arabic-Indic", "المبلغ الإجمالي: ١٢٬٣٤٥ درهم. موعد التسليم: ١٨/١١/٢٠٢٦.",
         "المبلغ الإجمالي هو ١٣٬٣٤٥ درهم. موعد التسليم ٢٨/١١/٢٠٢٦. مع خالص التحية."),
        ("ar correct, converted to Western digits", "المبلغ الإجمالي: ١٢٬٣٤٥ درهم. موعد التسليم: ١٨/١١/٢٠٢٦.",
         "المبلغ الإجمالي هو 12,345 درهم. موعد التسليم 18/11/2026. مع خالص التحية."),
    ];
    for (label, corpus, draft) in cases {
        let (out, r, _) = ground(corpus, draft);
        println!(
            "  [{label}]\n    draft: {draft:?}\n    out  : {out:?}\n    values_checked={} unsupported={:?} dropped={}",
            r.values_checked, r.unsupported, r.sentences_dropped
        );
    }

    println!("\n## 7. Grounding hold: when does the first character leave the sentence check?");
    for (label, draft) in [
        ("en", "Thanks for the note. Monday at 10 works for me. I will bring the samples. See you then."),
        ("de", "Vielen Dank für Ihre Nachricht. Montag um 10 Uhr passt mir gut. Ich bringe die Muster mit."),
        ("ja", "いつもお世話になっております。月曜日の10時で問題ございません。サンプルを持参いたします。当日はよろしくお願いいたします。"),
        ("ar", "شكرًا لرسالتكم. يناسبني يوم الاثنين الساعة العاشرة. سأحضر العينات معي."),
        ("ja-long", "いつもお世話になっております。月曜日の10時で問題ございません。サンプルを持参いたします。当日はよろしくお願いいたします。なお、資料は事前にお送りいたしますので、ご確認いただけますと幸いです。ご不明な点がございましたら、お気軽にお申し付けください。"),
    ] {
        let (out, r, first) = ground("Monday 10", draft);
        println!(
            "  {label}: {} bytes / {} chars; first output after {:?} bytes; sentences_checked={} out={:?}",
            draft.len(), draft.chars().count(), first, r.sentences_checked,
            out.chars().take(40).collect::<String>()
        );
    }

    println!("\n## 8. Voice: greeting and sign-off normalisation (nugget_voice)");
    for t in [
        "Hi Ana,\n\nThursday works.\n\nCheers,\nRobin",
        "Sehr geehrte Frau Weber,\n\nvielen Dank.\n\nMit freundlichen Grüßen\nKlara Brandt",
        "Guten Tag Herr Albrecht,\n\nes sind 14 Paletten.\n\nMit freundlichen Grüßen\nKlara Brandt",
        "Hallo Jonas,\n\ndanke.\n\nViele Grüße\nKlara",
        "Estimado Sr. Méndez:\n\nGracias.\n\nUn saludo,\nLucía Ortega",
        "Buenos días:\n\nGracias.\n\nAtentamente,\nLucía Ortega",
        "星商事株式会社\n山田様\n\nいつもお世話になっております。\n\n何卒よろしくお願いいたします。\n水野",
        "السيد كريم منصور المحترم،\n\nشكرًا.\n\nمع خالص التحية،\nليلى حداد",
    ] {
        println!(
            "  {:<60} greeting={:?} signoff={:?}",
            format!("{:?}", t.lines().next().unwrap_or("")),
            normalize_greeting(t, &[]),
            normalize_signoff(t, &[])
        );
    }
}
