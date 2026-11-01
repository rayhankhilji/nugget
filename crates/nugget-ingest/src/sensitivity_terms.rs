//! Sensitive-content terms in the UI languages besides English (QA JJ1-1): German, Spanish, French,
//! Brazilian Portuguese, Japanese, Simplified Chinese, Hindi and Arabic.

/// Match mode of a term (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum M {
    Word,
    Phrase,
    Stem,
    Sub,
}
use M::*;

/// One line per run of terms with the same match mode and kind, in match order: `<mode> <kind>:
/// term, term, ...`.
const TABLE: &str = "\
-- English additions
phrase health: mood journal, panic attack
word credentials: passphrase
phrase credentials: wifi password
-- German
stem health: befund, diagnose, arzt, ärztin
phrase health: praxis dr
stem health: arztpraxis
word health: krankgeschrieben
stem health: krankschreibung, krankmeldung, arbeitsunfähig
word health: au-bescheinigung
stem health: krankenhaus, klinik, therapie, therapeut, blutwerte, laborwerte, schilddrüse
stem health: medikament, krankenkasse
stem financial: gehalt, lohnsteuer, lohnabrechnung, kontoauszug, kontostand, girokonto
stem financial: dispositionskredit, dispokredit, lastschrift, überweisung, kreditkarte
stem financial: steuerbescheid, steuererklärung, darlehen, hypothek, schulden
stem credentials: kennwort, passwort, zugangsdaten, anmeldedaten, bestätigungscode
stem legal: klage, amtsgericht, landgericht
word legal: gericht
stem legal: rechtsanwalt, rechtsanwältin, anwalt, anwältin, gütetermin, vorladung, scheidung
stem legal: sorgerecht
stem identity: personalausweis, reisepass, sozialversicherungsnummer
word identity: steuer-id
-- Spanish
phrase health: analisis clinico
word health: analitica
phrase health: resultados de su analisis
word health: medico, medica, doctora
phrase health: centro de salud, consulta medica, receta medica
word health: diagnostico, colesterol, hospital, clinica, terapia, psicologo, psicologa
phrase health: baja medica
word financial: extracto
phrase financial: saldo anterior, saldo actual, saldo disponible, saldo de su cuenta
phrase financial: cuenta bancaria
word financial: nomina, hipoteca
phrase financial: tarjeta terminada en
word financial: prestamo, deuda
phrase financial: ha recibido una transferencia, declaracion de la renta
word credentials: contrasena
phrase credentials: clave de acceso, codigo de verificacion
word legal: demanda, juzgado
phrase legal: juicio por
word legal: abogado, abogada, tribunal, citacion, despido, divorcio, custodia
word identity: dni, pasaporte
phrase identity: seguridad social
-- French
phrase health: resultats d'analyses, analyses de sang
word health: hypothyroidie, hyperthyroidie, diabete
phrase health: compte rendu de consultation
word health: medecin, docteur, ordonnance, irm, hernie
phrase health: arret de travail, arret maladie
word health: hopital, clinique, therapie, psychologue
phrase health: analyses de sang
word health: mutuelle
phrase financial: releve de compte, releve bancaire, solde au, solde du compte
phrase financial: solde de votre compte
word financial: decouvert, prelevement, virement
phrase financial: fiche de paie, bulletin de salaire
word financial: salaire, impots
phrase financial: credit immobilier
phrase credentials: mot de passe
word credentials: identifiants
phrase credentials: code de verification
word legal: tribunal, convocation, convoquee, convoque, avocat, avocate, huissier, proces
word legal: divorce
phrase legal: depot de garantie
word identity: passeport
phrase identity: carte d'identite, numero de securite sociale
-- Portuguese (Brazil)
word health: medico
phrase health: consulta medica, resultado do exame, resultados dos exames, exame de sangue
phrase health: receita medica
word health: diagnostico
phrase health: plano de saude, atestado medico
word health: psicologo, terapia
word financial: extrato
phrase financial: saldo anterior, saldo atual, saldo disponivel, saldo em conta, conta corrente
word financial: salario, holerite, contracheque
phrase financial: fatura do cartao
word financial: emprestimo, financiamento
phrase financial: imposto de renda
word credentials: senha
phrase credentials: codigo de verificacao
phrase legal: processo judicial
word legal: advogado, advogada
phrase legal: audiencia de conciliacao
word legal: intimacao, divorcio
phrase legal: guarda dos filhos
word identity: cpf, passaporte
-- Japanese
sub health: 診察, 診断, 糖尿病, 病院, クリニック, 検査結果, 健康診断, 処方, 治療, カウンセリング
sub financial: 給与, 給料, 口座, 残高, 引き落とし, ローン, 源泉徴収, 確定申告
sub credentials: パスワード, 暗証番号, 認証コード
sub legal: 裁判, 訴訟, 弁護士, 離婚, 親権, 調停
sub identity: マイナンバー, パスポート, 保険証
-- Chinese (Simplified)
sub health: 诊断, 确诊, 糖尿病, 医院, 门诊, 体检, 检查结果, 化验, 处方, 治疗, 心理咨询, 病历
sub financial: 工资, 薪资, 银行卡, 余额, 账户对账单, 银行对账单, 信用卡对账单, 贷款, 房贷, 个税
sub credentials: 密码, 验证码
sub legal: 法院, 起诉, 诉讼, 律师, 离婚, 抚养权, 传票
sub identity: 身份证, 护照, 社保
-- Hindi
sub health: डॉक्टर, अस्पताल, जांच रिपोर्ट, दवा, इलाज, निदान, मधुमेह, मनोवैज्ञानिक, मनोचिकित्सक
sub health: परामर्श सत्र, काउंसलिंग, थेरेपी
sub financial: वेतन, सैलरी, बैंक खाता, बैलेंस, बैंक स्टेटमेंट, होम लोन, पर्सनल लोन, लोन की किस्त
sub financial: कर्ज, ऋण, खाता संख्या, खाता विवरण
sub credentials: पासवर्ड, ओटीपी, सत्यापन कोड, वन टाइम पासवर्ड
sub legal: अदालत, कोर्ट, वकील, मुकदमा, तलाक
sub identity: आधार कार्ड, आधार नंबर, पैन कार्ड, पासपोर्ट
-- Arabic
sub health: طبيب, مستشفى, عيادة, تحاليل, تشخيص, وصفة طبية, علاج, السكري, أخصائي نفسي
sub health: أخصائية نفسية, طبيب نفسي, طبيبة نفسية, معالج نفسي, معالجة نفسية, علاج نفسي
sub health: الصحة النفسية, استشارة نفسية
sub financial: الراتب, راتبك, كشف حساب, رصيد, حساب بنكي, قرض, قروض, تحويل بنكي
sub credentials: كلمة المرور, كلمة السر, رمز التحقق
sub legal: محكمة, محامي, دعوى, طلاق, حضانة
sub identity: جواز سفر, بطاقة الهوية, رقم الهوية
";

/// `(term, match mode, kind)`, in table order.
fn terms() -> &'static [(&'static str, M, &'static str)] {
    static TERMS: std::sync::LazyLock<Vec<(&str, M, &str)>> = std::sync::LazyLock::new(|| {
        let mut out = Vec::new();
        for line in TABLE.lines().filter(|l| !l.starts_with("--")) {
            let (head, list) = line.split_once(": ").expect("term line");
            let (mode, kind) = head.split_once(' ').expect("mode and kind");
            let mode = match mode {
                "word" => Word,
                "phrase" => Phrase,
                "stem" => Stem,
                "sub" => Sub,
                m => unreachable!("match mode {m}"),
            };
            out.extend(list.split(", ").map(|t| (t, mode, kind)));
        }
        out
    });
    &TERMS
}

/// Lowercase with common Latin accents removed (`Análisis` → `analisis`).
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

/// Whether folded `term` occurs in folded `text` as an Arabic word with clitics.
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

/// Han, kana and CJK punctuation.
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
    for &(term, mode, kind) in terms() {
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

    /// `language|S (sensitive) or - (ordinary)|subject|body`; `\n` is a line break.
    const CASES: &str = "\
en|S|Follow-up after Thursday's session|try the breathing exercise when the panic starts at work, and keep the mood journal
en|S|Wi-Fi for the cabin|Network: Pinecrest-Guest, passphrase: amber-otter-1182.
en|S|Your lab results are ready|Your blood test came back. Your HbA1c is 6.9.
en|S|Your payslip for September|Gross pay 4,150.00, net pay 3,130.10. Paid to sort code 20-44-91.
en|-|Lunch on Thursday?|Are you free for lunch on Thursday? The new noodle place opened.
en|-|Slides for the review|Here are the slides for Monday's design review.
en|-|Bike repair|The shop says the wheel will be ready Wednesday afternoon.
de|S|Ihr Termin in der Praxis Dr. Kaya|Ihr Befund ist da: die Schilddrüsenwerte sind leicht erhöht.
de|S|Krankmeldung bis Freitag|ich bin bis Freitag krankgeschrieben, die AU-Bescheinigung schicke ich der Personalabteilung.
de|S|Kontoauszug Oktober|Ihr Kontostand am 31.10.: 1.204,55 EUR. Dispositionskredit: 1.000 EUR.
de|S|Gehaltsabrechnung September|Brutto 4.800,00 EUR, Lohnsteuer 802,16 EUR, Netto 2.951,30 EUR.
de|S|Zugangsdaten für das Mieterportal|Benutzername: r.hale\\nKennwort: Lindenblatt#77
de|S|Ihre Klage gegen die Hausverwaltung|die Klageschrift wurde beim Amtsgericht eingereicht. Der Gütetermin ist am 3. Dezember.
de|S|Reisepass|Bitte bringen Sie Ihren Reisepass zum Termin mit.
de|-|Grillen am Samstag|Wir grillen am Samstag ab 17 Uhr im Garten. Bring gern einen Salat mit.
de|-|Protokoll der Teamrunde|Anbei das Protokoll. Nächste Runde am Dienstag um 10 Uhr.
de|-|Nur heute: 50 % auf alle Sneaker|Jetzt zuschlagen! Gutscheincode SNEAK50. Newsletter abbestellen.
es|S|Resultados de su análisis|los resultados de su analítica muestran el colesterol en 245. Centro de Salud Las Arenas
es|S|Extracto de su cuenta|Saldo a 30 de septiembre: 3.112,08 EUR. Recibo de la hipoteca cargado.
es|S|Tu contraseña del portal del colegio|Usuario: rhale_familia\\nContraseña: Gaviota*2026
es|S|Demanda laboral — citación|el juicio por despido está señalado en el Juzgado de lo Social n.º 4.
es|S|Nomina de octubre|Adjuntamos su nómina de octubre.
es|-|Cena del viernes|¿Quedamos el viernes para cenar? Reservo en el sitio de siempre.
es|-|Fotos de la excursión|Te paso las fotos de la excursión al lago. ¡Qué buen día!
es|-|Ofertas de otoño: hasta -60 %|Solo esta semana. Código OTONO60. Darse de baja.
fr|S|Compte rendu de consultation|le Dr Benali confirme une hernie discale L4-L5. Une IRM de contrôle est prescrite.
fr|S|Votre relevé de compte|Solde au 30/09 : 2 487,12 EUR. Découvert autorisé : 800 EUR.
fr|S|Vos identifiants pour l'espace client|Identifiant : r.hale\\nMot de passe provisoire : Mouette+3391
fr|S|Convocation au tribunal|vous êtes convoquée devant le tribunal judiciaire pour l'affaire du dépôt de garantie.
fr|S|Fiche de paie|Votre fiche de paie d'octobre est disponible.
fr|-|Pique-nique dimanche|On fait un pique-nique au parc dimanche midi, tu viens ?
fr|-|Notes de la réunion|Voici les notes de la réunion de lundi. Prochaine étape : le devis.
fr|-|Soldes privées : -40 % sur la maison|Offre réservée aux membres. Code MAISON40. Se désabonner.
pt-BR|S|Resultado do exame|O resultado do exame de sangue já está disponível. Consulta médica na terça.
pt-BR|S|Seu holerite de outubro|Salário líquido: R$ 6.240,00.
pt-BR|S|Extrato da conta corrente|Saldo disponível: R$ 1.932,10.
pt-BR|S|Sua nova senha|Usuário: rhale. Senha temporária: Sabiá#2026.
pt-BR|S|Intimação|O advogado informa que a audiência de conciliação foi marcada.
pt-BR|S|CPF|Envie uma cópia do seu CPF e do passaporte.
pt-BR|-|Churrasco no sábado|Vamos fazer um churrasco no sábado à tarde, traz a salada?
pt-BR|-|Fotos da viagem|Mandei as fotos da viagem para a praia. Que dia lindo!
pt-BR|-|Reunião de equipe|A pauta da reunião de segunda está anexa.
ja|S|検査結果のお知らせ|健康診断の検査結果が出ました。再診察をご予約ください。
ja|S|10月分給与明細|10月分の給与明細をお送りします。
ja|S|口座残高のお知らせ|普通預金口座の残高は 152,300 円です。
ja|S|パスワードの再設定|仮パスワード: Sakura#88 を使ってログインしてください。
ja|S|調停の日程|弁護士より、離婚調停の期日についてご連絡します。
ja|-|土曜日のバーベキュー|土曜日の午後にバーベキューをします。サラダを持ってきてね。
ja|-|会議の議事録|月曜日の会議の議事録を添付します。
ja|-|写真を送ります|旅行の写真を送ります。楽しかったね。
zh-CN|S|体检报告|您的体检检查结果已出，请预约门诊复诊。
zh-CN|S|十月工资条|您十月的工资条已发送，实发工资 12,400 元。
zh-CN|S|账户对账单|您的银行卡余额为 3,210.55 元。
zh-CN|S|验证码|您的验证码是 483920，请勿告诉他人。
zh-CN|S|律师函|律师通知您，起诉材料已提交法院。
zh-CN|S|身份证|请带上身份证和护照原件。
zh-CN|-|周六烧烤|周六下午在花园烧烤，记得带沙拉。
zh-CN|-|会议纪要|附上周一会议的纪要，下一步是报价。
zh-CN|-|旅行照片|给你发了湖边旅行的照片，那天真好。
hi|S|जांच रिपोर्ट|आपकी जांच रिपोर्ट आ गई है, कृपया डॉक्टर से मिलें।
hi|S|अक्टूबर का वेतन|आपका अक्टूबर का वेतन आपके बैंक खाता में जमा कर दिया गया है।
hi|S|पासवर्ड बदलें|आपका अस्थायी पासवर्ड Neem#42 है।
hi|S|अदालत की तारीख|वकील ने बताया कि मुकदमा की अगली सुनवाई अगले महीने है।
hi|S|होम लोन|आपकी होम लोन की किस्त कल कटेगी।
hi|-|शनिवार को पिकनिक|शनिवार को पार्क में पिकनिक है, सलाद ले आना।
hi|-|बैठक के नोट्स|सोमवार की बैठक के नोट्स संलग्न हैं। इसके आधार पर अगला कदम तय करेंगे।
hi|-|नई कॉलोनी|हमारी कॉलोनी में नया पार्क खुला है।
ar|S|نتائج التحاليل|نتائج تحاليل الدم جاهزة، يرجى مراجعة الطبيب في العيادة.
ar|S|كشف حساب أكتوبر|رصيد حسابك البنكي في نهاية الشهر هو 4,120 درهم.
ar|S|الراتب|تم تحويل الراتب إلى حسابك.
ar|S|كلمة المرور الجديدة|كلمة المرور المؤقتة هي Nakhla#7.
ar|S|موعد المحكمة|أبلغنا المحامي أن جلسة المحكمة في الأسبوع القادم.
ar|S|جواز سفر|يرجى إحضار جواز سفر ساري المفعول.
ar|-|شواء يوم السبت|سنقيم حفلة شواء يوم السبت في الحديقة، أحضر السلطة.
ar|-|محضر الاجتماع|مرفق محضر اجتماع يوم الاثنين.
ar|-|صور الرحلة|أرسلت لك صور الرحلة إلى البحيرة.";

    #[test]
    fn every_ui_language() {
        for line in CASES.lines() {
            let [lang, flag, subject, body] = line.splitn(4, '|').collect::<Vec<_>>()[..] else {
                panic!("bad case {line:?}");
            };
            let hit = mail(subject, &body.replace("\\n", "\n"));
            assert_eq!(hit.is_some(), flag == "S", "{lang}: {subject:?} -> {hit:?}");
        }
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
}
