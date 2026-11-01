//! Sensitive-content terms in the UI languages besides English (QA JJ1-1): German, Spanish,
//! French, Brazilian Portuguese, Japanese, Simplified Chinese, Hindi and Arabic.
//!
//! Each term has a match mode:
//! - [`M::Word`]: a whole token (also with an `s` / `es` plural);
//! - [`M::Phrase`]: consecutive tokens;
//! - [`M::Stem`]: a token that contains it (German compounds: `Gehaltsabrechnung`,
//!   `Girokonto`, `Schilddrüsenwerte`);
//! - [`M::Sub`]: anywhere in the text, for scripts without spaces between words (Japanese,
//!   Chinese) and Devanagari (whose vowel signs and virama split tokens). Arabic terms are
//!   matched as words that may carry clitics: a prefix (`ال`, `و`, `ب`, `ل`, `ف`, `ك` and their
//!   combinations) and a suffix (pronouns, plural endings; `ة` becomes `ت` before one), so
//!   `القرض` and `عيادتك` match while `طلاق` does not match inside `إطلاق` (see [`arabic_match`]).
//!
//! Latin text and terms are compared lowercase and with accents folded (`análisis` =
//! `analisis`), so a missing accent still matches. Arabic is compared without short vowels and
//! tatweel, with `أ إ آ ٱ` as `ا`, `ى` as `ي` and `ة` as `ه`; Devanagari with chandrabindu as
//! anusvara and without nukta (`जाँच` = `जांच`). Terms favour recall: a false positive only
//! keeps an item from cloud models; it stays searchable locally.

/// Match mode of a term (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum M {
    Word,
    Phrase,
    Stem,
    Sub,
}
use M::*;

const H: &str = "health";
const F: &str = "financial";
const C: &str = "credentials";
const L: &str = "legal";
const I: &str = "identity";

/// `(term, match mode, kind)`.
pub(crate) const TERMS: &[(&str, M, &str)] = &[
    // ---- English additions (JJ1 misses) ----
    ("mood journal", Phrase, H),
    ("panic attack", Phrase, H),
    ("passphrase", Word, C),
    ("wifi password", Phrase, C),
    // ---- German ----
    ("befund", Stem, H),
    ("diagnose", Stem, H),
    ("arzt", Stem, H),
    ("ärztin", Stem, H),
    ("praxis dr", Phrase, H),
    ("arztpraxis", Stem, H),
    ("krankgeschrieben", Word, H),
    ("krankschreibung", Stem, H),
    ("krankmeldung", Stem, H),
    ("arbeitsunfähig", Stem, H),
    ("au-bescheinigung", Word, H),
    ("krankenhaus", Stem, H),
    ("klinik", Stem, H),
    ("therapie", Stem, H),
    ("therapeut", Stem, H),
    ("blutwerte", Stem, H),
    ("laborwerte", Stem, H),
    ("schilddrüse", Stem, H),
    ("medikament", Stem, H),
    ("krankenkasse", Stem, H),
    ("gehalt", Stem, F),
    ("lohnsteuer", Stem, F),
    ("lohnabrechnung", Stem, F),
    ("kontoauszug", Stem, F),
    ("kontostand", Stem, F),
    ("girokonto", Stem, F),
    ("dispositionskredit", Stem, F),
    ("dispokredit", Stem, F),
    ("lastschrift", Stem, F),
    ("überweisung", Stem, F),
    ("kreditkarte", Stem, F),
    ("steuerbescheid", Stem, F),
    ("steuererklärung", Stem, F),
    ("darlehen", Stem, F),
    ("hypothek", Stem, F),
    ("schulden", Stem, F),
    ("kennwort", Stem, C),
    ("passwort", Stem, C),
    ("zugangsdaten", Stem, C),
    ("anmeldedaten", Stem, C),
    ("bestätigungscode", Stem, C),
    ("klage", Stem, L),
    ("amtsgericht", Stem, L),
    ("landgericht", Stem, L),
    ("gericht", Word, L),
    ("rechtsanwalt", Stem, L),
    ("rechtsanwältin", Stem, L),
    ("anwalt", Stem, L),
    ("anwältin", Stem, L),
    ("gütetermin", Stem, L),
    ("vorladung", Stem, L),
    ("scheidung", Stem, L),
    ("sorgerecht", Stem, L),
    ("personalausweis", Stem, I),
    ("reisepass", Stem, I),
    ("sozialversicherungsnummer", Stem, I),
    ("steuer-id", Word, I),
    // ---- Spanish ----
    ("analisis clinico", Phrase, H),
    ("analitica", Word, H),
    ("resultados de su analisis", Phrase, H),
    ("medico", Word, H),
    ("medica", Word, H),
    ("doctora", Word, H),
    ("centro de salud", Phrase, H),
    ("consulta medica", Phrase, H),
    ("receta medica", Phrase, H),
    ("diagnostico", Word, H),
    ("colesterol", Word, H),
    ("hospital", Word, H),
    ("clinica", Word, H),
    ("terapia", Word, H),
    ("psicologo", Word, H),
    ("psicologa", Word, H),
    ("baja medica", Phrase, H),
    ("extracto", Word, F),
    ("saldo anterior", Phrase, F),
    ("saldo actual", Phrase, F),
    ("saldo disponible", Phrase, F),
    ("saldo de su cuenta", Phrase, F),
    ("cuenta bancaria", Phrase, F),
    ("nomina", Word, F),
    ("hipoteca", Word, F),
    ("tarjeta terminada en", Phrase, F),
    ("prestamo", Word, F),
    ("deuda", Word, F),
    ("ha recibido una transferencia", Phrase, F),
    ("declaracion de la renta", Phrase, F),
    ("contrasena", Word, C),
    ("clave de acceso", Phrase, C),
    ("codigo de verificacion", Phrase, C),
    ("demanda", Word, L),
    ("juzgado", Word, L),
    ("juicio por", Phrase, L),
    ("abogado", Word, L),
    ("abogada", Word, L),
    ("tribunal", Word, L),
    ("citacion", Word, L),
    ("despido", Word, L),
    ("divorcio", Word, L),
    ("custodia", Word, L),
    ("dni", Word, I),
    ("pasaporte", Word, I),
    ("seguridad social", Phrase, I),
    // ---- French ----
    ("resultats d'analyses", Phrase, H),
    ("analyses de sang", Phrase, H),
    ("hypothyroidie", Word, H),
    ("hyperthyroidie", Word, H),
    ("diabete", Word, H),
    ("compte rendu de consultation", Phrase, H),
    ("medecin", Word, H),
    ("docteur", Word, H),
    ("ordonnance", Word, H),
    ("irm", Word, H),
    ("hernie", Word, H),
    ("arret de travail", Phrase, H),
    ("arret maladie", Phrase, H),
    ("hopital", Word, H),
    ("clinique", Word, H),
    ("therapie", Word, H),
    ("psychologue", Word, H),
    ("analyses de sang", Phrase, H),
    ("mutuelle", Word, H),
    ("releve de compte", Phrase, F),
    ("releve bancaire", Phrase, F),
    ("solde au", Phrase, F),
    ("solde du compte", Phrase, F),
    ("solde de votre compte", Phrase, F),
    ("decouvert", Word, F),
    ("prelevement", Word, F),
    ("virement", Word, F),
    ("fiche de paie", Phrase, F),
    ("bulletin de salaire", Phrase, F),
    ("salaire", Word, F),
    ("impots", Word, F),
    ("credit immobilier", Phrase, F),
    ("mot de passe", Phrase, C),
    ("identifiants", Word, C),
    ("code de verification", Phrase, C),
    ("tribunal", Word, L),
    ("convocation", Word, L),
    ("convoquee", Word, L),
    ("convoque", Word, L),
    ("avocat", Word, L),
    ("avocate", Word, L),
    ("huissier", Word, L),
    ("proces", Word, L),
    ("divorce", Word, L),
    ("depot de garantie", Phrase, L),
    ("passeport", Word, I),
    ("carte d'identite", Phrase, I),
    ("numero de securite sociale", Phrase, I),
    // ---- Portuguese (Brazil) ----
    ("medico", Word, H),
    ("consulta medica", Phrase, H),
    ("resultado do exame", Phrase, H),
    ("resultados dos exames", Phrase, H),
    ("exame de sangue", Phrase, H),
    ("receita medica", Phrase, H),
    ("diagnostico", Word, H),
    ("plano de saude", Phrase, H),
    ("atestado medico", Phrase, H),
    ("psicologo", Word, H),
    ("terapia", Word, H),
    ("extrato", Word, F),
    ("saldo anterior", Phrase, F),
    ("saldo atual", Phrase, F),
    ("saldo disponivel", Phrase, F),
    ("saldo em conta", Phrase, F),
    ("conta corrente", Phrase, F),
    ("salario", Word, F),
    ("holerite", Word, F),
    ("contracheque", Word, F),
    ("fatura do cartao", Phrase, F),
    ("emprestimo", Word, F),
    ("financiamento", Word, F),
    ("imposto de renda", Phrase, F),
    ("senha", Word, C),
    ("codigo de verificacao", Phrase, C),
    ("processo judicial", Phrase, L),
    ("advogado", Word, L),
    ("advogada", Word, L),
    ("audiencia de conciliacao", Phrase, L),
    ("intimacao", Word, L),
    ("divorcio", Word, L),
    ("guarda dos filhos", Phrase, L),
    ("cpf", Word, I),
    ("passaporte", Word, I),
    // ---- Japanese ----
    ("診察", Sub, H),
    ("診断", Sub, H),
    ("糖尿病", Sub, H),
    ("病院", Sub, H),
    ("クリニック", Sub, H),
    ("検査結果", Sub, H),
    ("健康診断", Sub, H),
    ("処方", Sub, H),
    ("治療", Sub, H),
    ("カウンセリング", Sub, H),
    ("給与", Sub, F),
    ("給料", Sub, F),
    ("口座", Sub, F),
    ("残高", Sub, F),
    ("引き落とし", Sub, F),
    ("ローン", Sub, F),
    ("源泉徴収", Sub, F),
    ("確定申告", Sub, F),
    ("パスワード", Sub, C),
    ("暗証番号", Sub, C),
    ("認証コード", Sub, C),
    ("裁判", Sub, L),
    ("訴訟", Sub, L),
    ("弁護士", Sub, L),
    ("離婚", Sub, L),
    ("親権", Sub, L),
    ("調停", Sub, L),
    ("マイナンバー", Sub, I),
    ("パスポート", Sub, I),
    ("保険証", Sub, I),
    // ---- Chinese (Simplified) ----
    ("诊断", Sub, H),
    ("确诊", Sub, H),
    ("糖尿病", Sub, H),
    ("医院", Sub, H),
    ("门诊", Sub, H),
    ("体检", Sub, H),
    ("检查结果", Sub, H),
    ("化验", Sub, H),
    ("处方", Sub, H),
    ("治疗", Sub, H),
    ("心理咨询", Sub, H),
    ("病历", Sub, H),
    ("工资", Sub, F),
    ("薪资", Sub, F),
    ("银行卡", Sub, F),
    ("余额", Sub, F),
    ("账户对账单", Sub, F),
    ("银行对账单", Sub, F),
    ("信用卡对账单", Sub, F),
    ("贷款", Sub, F),
    ("房贷", Sub, F),
    ("个税", Sub, F),
    ("密码", Sub, C),
    ("验证码", Sub, C),
    ("法院", Sub, L),
    ("起诉", Sub, L),
    ("诉讼", Sub, L),
    ("律师", Sub, L),
    ("离婚", Sub, L),
    ("抚养权", Sub, L),
    ("传票", Sub, L),
    ("身份证", Sub, I),
    ("护照", Sub, I),
    ("社保", Sub, I),
    // ---- Hindi ----
    ("डॉक्टर", Sub, H),
    ("अस्पताल", Sub, H),
    ("जांच रिपोर्ट", Sub, H),
    ("दवा", Sub, H),
    ("इलाज", Sub, H),
    ("निदान", Sub, H),
    ("मधुमेह", Sub, H),
    ("मनोवैज्ञानिक", Sub, H),
    ("मनोचिकित्सक", Sub, H),
    ("परामर्श सत्र", Sub, H),
    ("काउंसलिंग", Sub, H),
    ("थेरेपी", Sub, H),
    ("वेतन", Sub, F),
    ("सैलरी", Sub, F),
    ("बैंक खाता", Sub, F),
    ("बैलेंस", Sub, F),
    ("बैंक स्टेटमेंट", Sub, F),
    ("होम लोन", Sub, F),
    ("पर्सनल लोन", Sub, F),
    ("लोन की किस्त", Sub, F),
    ("कर्ज", Sub, F),
    ("ऋण", Sub, F),
    ("खाता संख्या", Sub, F),
    ("खाता विवरण", Sub, F),
    ("पासवर्ड", Sub, C),
    ("ओटीपी", Sub, C),
    ("सत्यापन कोड", Sub, C),
    ("वन टाइम पासवर्ड", Sub, C),
    ("अदालत", Sub, L),
    ("कोर्ट", Sub, L),
    ("वकील", Sub, L),
    ("मुकदमा", Sub, L),
    ("तलाक", Sub, L),
    ("आधार कार्ड", Sub, I),
    ("आधार नंबर", Sub, I),
    ("पैन कार्ड", Sub, I),
    ("पासपोर्ट", Sub, I),
    // ---- Arabic ----
    ("طبيب", Sub, H),
    ("مستشفى", Sub, H),
    ("عيادة", Sub, H),
    ("تحاليل", Sub, H),
    ("تشخيص", Sub, H),
    ("وصفة طبية", Sub, H),
    ("علاج", Sub, H),
    ("السكري", Sub, H),
    ("أخصائي نفسي", Sub, H),
    ("أخصائية نفسية", Sub, H),
    ("طبيب نفسي", Sub, H),
    ("طبيبة نفسية", Sub, H),
    ("معالج نفسي", Sub, H),
    ("معالجة نفسية", Sub, H),
    ("علاج نفسي", Sub, H),
    ("الصحة النفسية", Sub, H),
    ("استشارة نفسية", Sub, H),
    ("الراتب", Sub, F),
    ("راتبك", Sub, F),
    ("كشف حساب", Sub, F),
    ("رصيد", Sub, F),
    ("حساب بنكي", Sub, F),
    ("قرض", Sub, F),
    ("قروض", Sub, F),
    ("تحويل بنكي", Sub, F),
    ("كلمة المرور", Sub, C),
    ("كلمة السر", Sub, C),
    ("رمز التحقق", Sub, C),
    ("محكمة", Sub, L),
    ("محامي", Sub, L),
    ("دعوى", Sub, L),
    ("طلاق", Sub, L),
    ("حضانة", Sub, L),
    ("جواز سفر", Sub, I),
    ("بطاقة الهوية", Sub, I),
    ("رقم الهوية", Sub, I),
];

/// Lowercase with common Latin accents removed (`Análisis` → `analisis`). Non-Latin text is
/// returned lowercase and otherwise unchanged.
pub(crate) fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            '\u{2019}' | '\u{2018}' => '\'',
            // Arabic letter variants.
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            // Devanagari: chandrabindu as anusvara.
            '\u{0901}' => '\u{0902}',
            c => c,
        })
        // Arabic short vowels, shadda, sukun, superscript alef and tatweel; Devanagari nukta.
        .filter(|c| {
            !matches!(
                c,
                '\u{064B}'..='\u{0652}' | '\u{0670}' | '\u{0640}' | '\u{093C}'
            )
        })
        .collect()
}

fn is_arabic(c: char) -> bool {
    matches!(c, '\u{0600}'..='\u{06FF}')
}

/// Prefix clitics an Arabic word may carry (folded): conjunctions, prepositions, the article.
const AR_PREFIXES: &[&str] = &[
    "", "و", "ف", "ب", "ك", "ل", "ال", "وال", "فال", "بال", "كال", "لل", "ولل", "فلل", "وب", "ول",
    "فب", "فل", "وك",
];
/// Suffixes an Arabic word may carry (folded): pronouns and plural/dual endings.
const AR_SUFFIXES: &[&str] = &[
    "", "ه", "ها", "هم", "هن", "هما", "ك", "كم", "كن", "كما", "ي", "نا", "ات", "ان", "ين", "ون",
    "يه", "يها", "يهم",
];

/// Whether folded `term` occurs in folded `text` as an Arabic word with clitics. The parts of
/// the word before and after the match must be a known prefix and suffix.
fn arabic_match(text: &str, term: &str) -> bool {
    // `ة` (folded to `ه`) is written `ت` before a suffix: `عيادة` + `ك` = `عيادتك`.
    let stem_t = term.strip_suffix('ه').map(|b| format!("{b}ت"));
    let forms = std::iter::once((term, false)).chain(stem_t.as_deref().map(|t| (t, true)));
    for (form, needs_suffix) in forms {
        for (at, _) in text.match_indices(form) {
            let start = text[..at]
                .char_indices()
                .rev()
                .find(|(_, c)| !is_arabic(*c))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let end = at + form.len();
            let stop = text[end..]
                .char_indices()
                .find(|(_, c)| !is_arabic(*c))
                .map_or(text.len(), |(i, _)| end + i);
            let (pre, suf) = (&text[start..at], &text[end..stop]);
            if AR_PREFIXES.contains(&pre)
                && AR_SUFFIXES.contains(&suf)
                && !(needs_suffix && suf.is_empty())
            {
                return true;
            }
        }
    }
    false
}

/// Han, kana and CJK punctuation. Text in these scripts has no spaces between words, so a
/// CJK character also ends a Latin word glued to it (`HbA1cは7.2%`, `您的OTP为482913`; QA
/// GG7-3); CJK terms themselves are matched as substrings ([`M::Sub`]).
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}')
}

/// Whether `c` belongs to a word token (Latin and other spaced scripts).
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() && !is_cjk(c)
}

/// Folded tokens: letters, digits, `'` and `-` inside a word.
pub(crate) fn tokens(s: &str) -> Vec<String> {
    fold(s)
        .split(|c: char| !(is_word_char(c) || c == '\'' || c == '-'))
        .map(|t| t.trim_matches(['\'', '-']).to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The kind of the first term found in `texts` (title, subject, body), if any.
pub(crate) fn find(texts: &[&str]) -> Option<&'static str> {
    let folded: Vec<String> = texts.iter().map(|t| fold(t)).collect();
    let toks: Vec<Vec<String>> = texts.iter().map(|t| tokens(t)).collect();
    for (term, mode, kind) in TERMS {
        let hit = match mode {
            Sub => {
                let t = fold(term);
                if t.chars().next().is_some_and(is_arabic) {
                    folded.iter().any(|x| arabic_match(x, &t))
                } else {
                    folded.iter().any(|x| x.contains(&t))
                }
            }
            Word => {
                let t = fold(term);
                toks.iter().flatten().any(|w| {
                    *w == t
                        || w.strip_suffix('s').is_some_and(|b| b == t)
                        || w.strip_suffix("es").is_some_and(|b| b == t)
                })
            }
            Stem => {
                let t = fold(term);
                toks.iter().flatten().any(|w| w.contains(&t))
            }
            Phrase => {
                let want = tokens(term);
                toks.iter()
                    .any(|ts| ts.windows(want.len()).any(|w| w == want.as_slice()))
            }
        };
        if hit {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::sensitivity::classify;
    use serde_json::json;

    fn mail(subject: &str, body: &str) -> Option<&'static str> {
        classify(
            "0:Inbox/m.eml",
            &[],
            Some(subject),
            body,
            &json!({"from": "Ada Quill <ada@quillworks.example>", "subject": subject}),
        )
    }

    /// `(language, subject, body)` that must be sensitive, then ones that must not.
    fn check(lang: &str, sensitive: &[(&str, &str)], ordinary: &[(&str, &str)]) {
        for (s, b) in sensitive {
            assert!(mail(s, b).is_some(), "{lang}: missed {s:?}");
        }
        for (s, b) in ordinary {
            assert_eq!(mail(s, b), None, "{lang}: flagged {s:?}");
        }
    }

    #[test]
    fn english() {
        check(
            "en",
            &[
                (
                    "Follow-up after Thursday's session",
                    "try the breathing exercise when the panic starts at work, and keep the mood journal",
                ),
                (
                    "Wi-Fi for the cabin",
                    "Network: Pinecrest-Guest, passphrase: amber-otter-1182.",
                ),
                (
                    "Your lab results are ready",
                    "Your blood test came back. Your HbA1c is 6.9.",
                ),
                (
                    "Your payslip for September",
                    "Gross pay 4,150.00, net pay 3,130.10. Paid to sort code 20-44-91.",
                ),
            ],
            &[
                (
                    "Lunch on Thursday?",
                    "Are you free for lunch on Thursday? The new noodle place opened.",
                ),
                (
                    "Slides for the review",
                    "Here are the slides for Monday's design review.",
                ),
                (
                    "Bike repair",
                    "The shop says the wheel will be ready Wednesday afternoon.",
                ),
            ],
        );
    }

    #[test]
    fn german() {
        check(
            "de",
            &[
                (
                    "Ihr Termin in der Praxis Dr. Kaya",
                    "Ihr Befund ist da: die Schilddrüsenwerte sind leicht erhöht.",
                ),
                (
                    "Krankmeldung bis Freitag",
                    "ich bin bis Freitag krankgeschrieben, die AU-Bescheinigung schicke ich der Personalabteilung.",
                ),
                (
                    "Kontoauszug Oktober",
                    "Ihr Kontostand am 31.10.: 1.204,55 EUR. Dispositionskredit: 1.000 EUR.",
                ),
                (
                    "Gehaltsabrechnung September",
                    "Brutto 4.800,00 EUR, Lohnsteuer 802,16 EUR, Netto 2.951,30 EUR.",
                ),
                (
                    "Zugangsdaten für das Mieterportal",
                    "Benutzername: r.hale\nKennwort: Lindenblatt#77",
                ),
                (
                    "Ihre Klage gegen die Hausverwaltung",
                    "die Klageschrift wurde beim Amtsgericht eingereicht. Der Gütetermin ist am 3. Dezember.",
                ),
                (
                    "Reisepass",
                    "Bitte bringen Sie Ihren Reisepass zum Termin mit.",
                ),
            ],
            &[
                (
                    "Grillen am Samstag",
                    "Wir grillen am Samstag ab 17 Uhr im Garten. Bring gern einen Salat mit.",
                ),
                (
                    "Protokoll der Teamrunde",
                    "Anbei das Protokoll. Nächste Runde am Dienstag um 10 Uhr.",
                ),
                (
                    "Nur heute: 50 % auf alle Sneaker",
                    "Jetzt zuschlagen! Gutscheincode SNEAK50. Newsletter abbestellen.",
                ),
            ],
        );
    }

    #[test]
    fn spanish() {
        check(
            "es",
            &[
                (
                    "Resultados de su análisis",
                    "los resultados de su analítica muestran el colesterol en 245. Centro de Salud Las Arenas",
                ),
                (
                    "Extracto de su cuenta",
                    "Saldo a 30 de septiembre: 3.112,08 EUR. Recibo de la hipoteca cargado.",
                ),
                (
                    "Tu contraseña del portal del colegio",
                    "Usuario: rhale_familia\nContraseña: Gaviota*2026",
                ),
                (
                    "Demanda laboral — citación",
                    "el juicio por despido está señalado en el Juzgado de lo Social n.º 4.",
                ),
                ("Nomina de octubre", "Adjuntamos su nómina de octubre."),
            ],
            &[
                (
                    "Cena del viernes",
                    "¿Quedamos el viernes para cenar? Reservo en el sitio de siempre.",
                ),
                (
                    "Fotos de la excursión",
                    "Te paso las fotos de la excursión al lago. ¡Qué buen día!",
                ),
                (
                    "Ofertas de otoño: hasta -60 %",
                    "Solo esta semana. Código OTONO60. Darse de baja.",
                ),
            ],
        );
    }

    #[test]
    fn french() {
        check(
            "fr",
            &[
                (
                    "Compte rendu de consultation",
                    "le Dr Benali confirme une hernie discale L4-L5. Une IRM de contrôle est prescrite.",
                ),
                (
                    "Votre relevé de compte",
                    "Solde au 30/09 : 2 487,12 EUR. Découvert autorisé : 800 EUR.",
                ),
                (
                    "Vos identifiants pour l'espace client",
                    "Identifiant : r.hale\nMot de passe provisoire : Mouette+3391",
                ),
                (
                    "Convocation au tribunal",
                    "vous êtes convoquée devant le tribunal judiciaire pour l'affaire du dépôt de garantie.",
                ),
                (
                    "Fiche de paie",
                    "Votre fiche de paie d'octobre est disponible.",
                ),
            ],
            &[
                (
                    "Pique-nique dimanche",
                    "On fait un pique-nique au parc dimanche midi, tu viens ?",
                ),
                (
                    "Notes de la réunion",
                    "Voici les notes de la réunion de lundi. Prochaine étape : le devis.",
                ),
                (
                    "Soldes privées : -40 % sur la maison",
                    "Offre réservée aux membres. Code MAISON40. Se désabonner.",
                ),
            ],
        );
    }

    #[test]
    fn portuguese_brazil() {
        check(
            "pt-BR",
            &[
                (
                    "Resultado do exame",
                    "O resultado do exame de sangue já está disponível. Consulta médica na terça.",
                ),
                ("Seu holerite de outubro", "Salário líquido: R$ 6.240,00."),
                (
                    "Extrato da conta corrente",
                    "Saldo disponível: R$ 1.932,10.",
                ),
                (
                    "Sua nova senha",
                    "Usuário: rhale. Senha temporária: Sabiá#2026.",
                ),
                (
                    "Intimação",
                    "O advogado informa que a audiência de conciliação foi marcada.",
                ),
                ("CPF", "Envie uma cópia do seu CPF e do passaporte."),
            ],
            &[
                (
                    "Churrasco no sábado",
                    "Vamos fazer um churrasco no sábado à tarde, traz a salada?",
                ),
                (
                    "Fotos da viagem",
                    "Mandei as fotos da viagem para a praia. Que dia lindo!",
                ),
                (
                    "Reunião de equipe",
                    "A pauta da reunião de segunda está anexa.",
                ),
            ],
        );
    }

    #[test]
    fn japanese() {
        check(
            "ja",
            &[
                (
                    "検査結果のお知らせ",
                    "健康診断の検査結果が出ました。再診察をご予約ください。",
                ),
                ("10月分給与明細", "10月分の給与明細をお送りします。"),
                (
                    "口座残高のお知らせ",
                    "普通預金口座の残高は 152,300 円です。",
                ),
                (
                    "パスワードの再設定",
                    "仮パスワード: Sakura#88 を使ってログインしてください。",
                ),
                (
                    "調停の日程",
                    "弁護士より、離婚調停の期日についてご連絡します。",
                ),
            ],
            &[
                (
                    "土曜日のバーベキュー",
                    "土曜日の午後にバーベキューをします。サラダを持ってきてね。",
                ),
                ("会議の議事録", "月曜日の会議の議事録を添付します。"),
                ("写真を送ります", "旅行の写真を送ります。楽しかったね。"),
            ],
        );
    }

    #[test]
    fn chinese_simplified() {
        check(
            "zh-CN",
            &[
                ("体检报告", "您的体检检查结果已出，请预约门诊复诊。"),
                ("十月工资条", "您十月的工资条已发送，实发工资 12,400 元。"),
                ("账户对账单", "您的银行卡余额为 3,210.55 元。"),
                ("验证码", "您的验证码是 483920，请勿告诉他人。"),
                ("律师函", "律师通知您，起诉材料已提交法院。"),
                ("身份证", "请带上身份证和护照原件。"),
            ],
            &[
                ("周六烧烤", "周六下午在花园烧烤，记得带沙拉。"),
                ("会议纪要", "附上周一会议的纪要，下一步是报价。"),
                ("旅行照片", "给你发了湖边旅行的照片，那天真好。"),
            ],
        );
    }

    #[test]
    fn hindi() {
        check(
            "hi",
            &[
                ("जांच रिपोर्ट", "आपकी जांच रिपोर्ट आ गई है, कृपया डॉक्टर से मिलें।"),
                (
                    "अक्टूबर का वेतन",
                    "आपका अक्टूबर का वेतन आपके बैंक खाता में जमा कर दिया गया है।",
                ),
                ("पासवर्ड बदलें", "आपका अस्थायी पासवर्ड Neem#42 है।"),
                (
                    "अदालत की तारीख",
                    "वकील ने बताया कि मुकदमा की अगली सुनवाई अगले महीने है।",
                ),
                ("होम लोन", "आपकी होम लोन की किस्त कल कटेगी।"),
            ],
            &[
                ("शनिवार को पिकनिक", "शनिवार को पार्क में पिकनिक है, सलाद ले आना।"),
                (
                    "बैठक के नोट्स",
                    "सोमवार की बैठक के नोट्स संलग्न हैं। इसके आधार पर अगला कदम तय करेंगे।",
                ),
                ("नई कॉलोनी", "हमारी कॉलोनी में नया पार्क खुला है।"),
            ],
        );
    }

    #[test]
    fn arabic_words_carry_clitics_but_do_not_match_inside_other_words() {
        // Prefixes, suffixes, and ة written ت before a suffix.
        assert!(mail("موعد", "سيتصل بك الطبيب غدا.").is_some());
        assert!(mail("القرض", "تم خصم قسط القرض.").is_some());
        assert!(mail("موعد", "نذكرك بموعدك في عيادتك يوم الأحد.").is_some());
        assert!(mail("جلسة", "د. ليلى منصور، أخصائية نفسية").is_some());
        // Short vowels and hamza forms do not matter.
        assert!(mail("موعد", "أخصّائيّة نفسيّة").is_some());
        assert!(mail("موعد", "اخصائية نفسية").is_some());
        // `طلاق` (divorce) inside `إطلاق` (launch), `قرض` inside `يقرضون`: no.
        assert_eq!(mail("إطلاق المرحلة الثانية", "نستعد لإطلاق المشروع."), None);
        assert_eq!(mail("مشروع", "هم يقرضون الأدوات للجيران."), None);
    }

    #[test]
    fn latin_terms_glued_to_cjk_text_still_match() {
        // QA GG7-3: a CJK run used to swallow the Latin word next to it.
        assert!(
            mail(
                "ご本人確認",
                "ご本人確認のためverification code：482913をご入力ください"
            )
            .is_some()
        );
        assert!(mail("検査", "HbA1cは7.2%でした").is_some());
        assert!(mail("登录", "您的OTP为482913").is_some());
        assert!(mail("检查", "您的HbA1c为6.9%").is_some());
    }

    #[test]
    fn arabic() {
        check(
            "ar",
            &[
                (
                    "نتائج التحاليل",
                    "نتائج تحاليل الدم جاهزة، يرجى مراجعة الطبيب في العيادة.",
                ),
                (
                    "كشف حساب أكتوبر",
                    "رصيد حسابك البنكي في نهاية الشهر هو 4,120 درهم.",
                ),
                ("الراتب", "تم تحويل الراتب إلى حسابك."),
                ("كلمة المرور الجديدة", "كلمة المرور المؤقتة هي Nakhla#7."),
                (
                    "موعد المحكمة",
                    "أبلغنا المحامي أن جلسة المحكمة في الأسبوع القادم.",
                ),
                ("جواز سفر", "يرجى إحضار جواز سفر ساري المفعول."),
            ],
            &[
                (
                    "شواء يوم السبت",
                    "سنقيم حفلة شواء يوم السبت في الحديقة، أحضر السلطة.",
                ),
                ("محضر الاجتماع", "مرفق محضر اجتماع يوم الاثنين."),
                ("صور الرحلة", "أرسلت لك صور الرحلة إلى البحيرة."),
            ],
        );
    }
}
