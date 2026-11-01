//! Ask, don't guess: spot a goal that names a target the screen shows more than once ("Reply to
//! Sam" with Sam Lee and Sam Ortiz in the inbox). Pure, no model call. Target labels are untrusted
//! screen text: they only reach the user, cleaned and cut, never the model.

use std::collections::{BTreeSet, HashSet};

use nugget_policy::RiskClass;
use serde::{Deserialize, Serialize};

use crate::tool::truncate;

/// Most candidates offered to the user.
pub const MAX_CANDIDATES: usize = 6;

/// Longest candidate label, in characters.
pub const CANDIDATE_MAX: usize = 80;

/// A goal word (or words) that fits more than one item on screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ambiguity {
    /// The matched goal word(s), as the user wrote them (e.g. `Sam`, `Q3 report`).
    pub term: String,
    /// The distinct items it fits, in screen order, cleaned and cut ([`MAX_CANDIDATES`] at most).
    pub candidates: Vec<String>,
}

impl Ambiguity {
    /// The question put to the user: fixed text around the term and the candidates.
    /// With no candidates, the item was never on screen (a delete about to act on a guess).
    pub fn question(&self) -> String {
        let term = clean(&self.term, CANDIDATE_MAX);
        if self.candidates.is_empty() {
            return format!(
                "Your request says “{term}”, but I have not found it on the screen, and the \
                 selection may be something else. Which item do you mean, and where is it?"
            );
        }
        format!(
            "Your request says “{term}”, and I see more than one: {}. Which one did you mean?",
            self.candidates.join(", ")
        )
    }
}

/// Words that say nothing about which item is meant: function words and the generic nouns and
/// verbs of everyday tasks.
#[rustfmt::skip]
const STOP_WORDS: &[&str] = &[
    // Function words.
    "the", "an", "my", "me", "you", "your", "our", "we", "us", "to", "and", "or", "of", "for",
    "in", "on", "at", "by", "from", "with", "about", "into", "onto", "it", "its", "this", "that",
    "is", "are", "be", "was", "were", "please", "can", "could", "would", "will", "just", "now",
    "then", "up", "out", "off", "so", "if", "as", "do", "does", "did", "him", "her", "them",
    "his", "hers", "their", "he", "she", "they", "yes", "no", "ok", "okay", "thanks", "thank",
    "hi", "hello", "one", "again", "too", "also", "new", "old", "some", "there", "here", "what",
    "which", "who", "when", "where", "how", "say", "saying", "want", "need", "get",
    // Generic nouns.
    "email", "mail", "message", "msg", "text", "chat", "thread", "conversation", "inbox", "file",
    "document", "doc", "folder", "directory", "meeting", "event", "calendar", "invite",
    "invitation", "appointment", "contact", "person", "someone", "item", "entry", "row", "list",
    "note", "page", "tab", "window", "app",
    // Generic verbs.
    "reply", "respond", "answer", "send", "forward", "delete", "remove", "trash", "archive",
    "open", "close", "tell", "ask", "let", "know", "write", "draft", "rename", "move", "copy",
    "cancel", "reschedule", "schedule", "book", "set", "change", "update", "edit", "add",
    "create", "make", "find", "show", "read", "mark", "star", "flag", "pin", "call", "accept",
    "decline", "share", "save", "print", "check", "look", "go", "put", "give", "back",
    "empty", "erase", "clear", "clean", "tidy", "buy", "pay", "purchase",
];

/// Words that ask for several items at once: nothing to disambiguate.
#[rustfmt::skip]
const MANY_WORDS: &[&str] = &[
    "all", "every", "each", "both", "any", "these", "those", "everything", "everyone",
    "everybody", "multiple", "several",
];

/// The ambiguous target in `goal` given what the screen shows, if any: targets are grouped by
/// the content words of the goal they match (singular or plural); a best-fitting group with two
/// or more distinct items is ambiguous, unless the goal asks for several items.
pub fn ambiguity(goal: &str, targets: &[String]) -> Option<Ambiguity> {
    // A whole name the goal spells out (`draft.txt`, `sam@ourco.example`) that one target shows
    // is that target.
    let names = dotted_names(goal);
    if names.iter().any(|n| {
        targets
            .iter()
            .filter(|t| t.to_lowercase().contains(n.as_str()))
            .count()
            == 1
    }) {
        return None;
    }
    // A file extension says what kind of item, not which one.
    let extensions: HashSet<String> = names
        .iter()
        .filter_map(|n| n.rsplit_once('.').map(|(_, ext)| ext.to_string()))
        .collect();
    let all = words(goal);
    if all
        .iter()
        .any(|(_, lower)| MANY_WORDS.contains(&lower.as_str()))
    {
        return None;
    }
    // Distinct content words of the goal, in order, with their spelling in the goal.
    let mut seen = HashSet::new();
    let goal_words: Vec<(String, String)> = all
        .into_iter()
        .filter(|(_, lower)| is_content_word(lower))
        .filter(|(_, lower)| !extensions.contains(lower))
        .filter(|(_, lower)| seen.insert(lower.clone()))
        .collect();
    if goal_words.is_empty() || targets.len() < 2 {
        return None;
    }

    // Per target: the goal words it matches, and which of those it matched only as the singular
    // of a plural goal word.
    let mut matched: Vec<(BTreeSet<usize>, BTreeSet<usize>)> = Vec::with_capacity(targets.len());
    for target in targets {
        let target_words: Vec<String> = words(target).into_iter().map(|(_, l)| l).collect();
        let mut set = BTreeSet::new();
        let mut plural = BTreeSet::new();
        for (i, (_, g)) in goal_words.iter().enumerate() {
            if target_words.iter().any(|w| w == g || is_plural_of(w, g)) {
                set.insert(i);
            } else if target_words.iter().any(|w| is_plural_of(g, w)) {
                set.insert(i);
                plural.insert(i);
            }
        }
        matched.push((set, plural));
    }
    let best = matched.iter().map(|(s, _)| s.len()).max()?;
    if best == 0 {
        return None;
    }

    // The best-fitting sets, in the order the screen first shows them.
    let mut sets: Vec<&BTreeSet<usize>> = Vec::new();
    for (set, _) in &matched {
        if set.len() == best && !sets.contains(&set) {
            sets.push(set);
        }
    }
    for set in sets {
        // A goal word in its plural form means the user wants all the matching items.
        if matched
            .iter()
            .any(|(s, plural)| s == set && !plural.is_disjoint(set))
        {
            continue;
        }
        let mut keys = HashSet::new();
        let mut candidates = Vec::new();
        for (target, (s, _)) in targets.iter().zip(&matched) {
            if s != set {
                continue;
            }
            let key = normalize(target);
            if key.is_empty() || !keys.insert(key) {
                continue;
            }
            candidates.push(clean(target, CANDIDATE_MAX));
        }
        if candidates.len() < 2 {
            continue;
        }
        candidates.truncate(MAX_CANDIDATES);
        let term = set
            .iter()
            .map(|&i| goal_words[i].0.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return Some(Ambiguity { term, candidates });
    }
    None
}

/// Words after which the goal names a place, not the object (`… in Documents`). Not "from",
/// which as often names a sender ("the spam email from Prize Center").
const PLACE_WORDS: &[&str] = &["in", "inside", "within", "under", "at"];

/// The words naming what the goal acts on, as written: its content words before a place phrase
/// ("annual report" in "Delete the annual report in Documents"). `None` when there are none or
/// the goal asks for several items.
pub fn goal_object(goal: &str) -> Option<String> {
    let all = words(goal);
    if all
        .iter()
        .any(|(_, lower)| MANY_WORDS.contains(&lower.as_str()))
    {
        return None;
    }
    let mut object = Vec::new();
    for (word, lower) in all {
        if PLACE_WORDS.contains(&lower.as_str()) {
            break;
        }
        if is_content_word(&lower) {
            object.push(word);
        }
    }
    (!object.is_empty()).then(|| object.join(" "))
}

/// Whether some target shows a word of the goal's object ([`goal_object`]), singular or plural.
pub fn shows_object(goal: &str, targets: &[String]) -> bool {
    let Some(object) = goal_object(goal) else {
        return true;
    };
    let wanted: Vec<String> = words(&object).into_iter().map(|(_, l)| l).collect();
    targets.iter().any(|t| {
        words(t).iter().any(|(_, w)| {
            wanted
                .iter()
                .any(|g| w == g || is_plural_of(w, g) || is_plural_of(g, w))
        })
    })
}

/// Steps that destroy data for good, in the nine UI languages (QA GG3-2, MM1-3): emptying the
/// trash and deleting permanently. Shared with nugget-computer's risk table, which treats any of
/// them as Destroy. Han and kana terms match as text, the others as whole words.
#[rustfmt::skip]
pub const IRREVERSIBLE: &[&str] = &[
    // English
    "empty", "erase", "purge", "wipe", "permanently", "shred", "forever", "clear trash",
    "clear the trash", "clear bin", "clear recycle bin", "delete forever",
    // German
    "papierkorb leeren", "leere den papierkorb", "endgültig", "dauerhaft löschen", "unwiderruflich",
    // Spanish
    "vaciar papelera", "vaciar la papelera", "vacia la papelera", "definitivamente",
    "permanentemente", "para siempre",
    // French
    "vider la corbeille", "vider corbeille", "vide la corbeille", "videz la corbeille",
    "définitivement",
    // Portuguese
    "esvaziar lixeira", "esvaziar a lixeira", "esvazie a lixeira", "para sempre",
    // Japanese
    "ゴミ箱を空", "完全に削除", "完全削除", "永久に削除",
    // Chinese
    "清空回收站", "清空废纸篓", "清空垃圾桶", "永久删除", "彻底删除",
    // Hindi
    "ट्रैश खाली", "बिन खाली", "स्थायी रूप से", "हमेशा के लिए",
    // Arabic
    "إفراغ سلة", "افراغ سلة", "حذف نهائي", "نهائيا",
];

/// Lowercase words: accents and Arabic short vowels dropped, apostrophes removed ("don't" is
/// `dont`), Devanagari and Arabic marks kept inside their words.
fn plain_words(text: &str) -> Vec<String> {
    let folded: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '\u{064B}'..='\u{0652}'))
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            'أ' | 'إ' | 'آ' => 'ا',
            c if c.is_alphanumeric() || matches!(c, '\u{0900}'..='\u{097F}') => c,
            _ => ' ',
        })
        .collect();
    folded.split_whitespace().map(str::to_owned).collect()
}

/// Han and kana: written without spaces, so matched as text.
fn unspaced(term: &str) -> bool {
    term.chars()
        .any(|c| matches!(c, '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{9FFF}'))
}

/// The first of `terms` in `text`: as text for Han and kana, otherwise as whole words (an `s`,
/// `d`, `ed` or `ing` ending allowed on a one-word Latin term when `inflect`). Accents do not
/// matter.
fn find_term(text: &str, terms: &[&'static str], inflect: bool) -> Option<&'static str> {
    let ws = plain_words(text);
    let flat = ws.concat();
    let joined = format!(" {} ", ws.join(" "));
    terms.iter().copied().find(|t| {
        let tw = plain_words(t);
        if unspaced(t) {
            flat.contains(&tw.concat())
        } else if let [one] = tw.as_slice() {
            ws.iter().any(|w| {
                w.strip_prefix(one.as_str()).is_some_and(|r| {
                    r.is_empty()
                        || (inflect && one.is_ascii() && ["s", "d", "ed", "ing"].contains(&r))
                })
            })
        } else {
            joined.contains(&format!(" {} ", tw.join(" ")))
        }
    })
}

/// When `label` names an irreversible step (Empty Trash, Delete forever, Papierkorb leeren) that
/// `goal` does not ask for: the matched term. Asking for any irreversible step allows them.
pub fn unrequested_irreversible(goal: &str, label: &str) -> Option<&'static str> {
    let word = irreversible(label)?;
    irreversible(goal).is_none().then_some(word)
}

/// The irreversible-step term in `text`, if any ([`IRREVERSIBLE`]).
pub fn irreversible(text: &str) -> Option<&'static str> {
    find_term(text, IRREVERSIBLE, true)
}

/// Verbs, in the nine UI languages, that need a send, a deletion or a payment to have happened
/// before the request is done (QA GG3-8, MM1-2).
#[rustfmt::skip]
const SEND_VERBS: &[&str] = &[
    "send", "reply", "forward", "respond", "sende", "senden", "schicke", "schicken", "verschicke",
    "antworte", "antworten", "weiterleiten", "leite", "envia", "enviar", "manda", "mandar",
    "responde", "responder", "reenvia", "reenviar", "envoie", "envoyer", "envoyez", "reponds",
    "repondre", "repondez", "transfere", "transferer", "transferez", "envie", "responda",
    "encaminhe", "encaminhar", "mande", "送信", "送って", "送る", "返信", "転送", "发送", "发给", "回复", "转发",
    "भेजें", "भेजो", "भेज दो", "भेजिए", "जवाब दें", "जवाब दो", "أرسل", "ارسل", "رد على", "ردي على",
];
#[rustfmt::skip]
const DESTROY_VERBS: &[&str] = &[
    "delete", "trash", "remove", "erase", "losche", "loschen", "entferne", "entfernen", "elimina",
    "eliminar", "borra", "borrar", "supprime", "supprimer", "supprimez", "efface", "effacer",
    "exclua", "excluir", "apague", "apagar", "remova", "remover", "削除", "消して", "消去", "删除", "删掉",
    "移除", "हटाएं", "हटाओ", "हटा दो", "डिलीट", "احذف", "امسح", "أزل",
];
#[rustfmt::skip]
const SPEND_VERBS: &[&str] = &[
    "buy", "pay", "purchase", "kaufe", "kaufen", "bezahle", "bezahlen", "compra", "comprar", "paga",
    "pagar", "achete", "acheter", "payer", "payez", "compre", "pague", "購入", "買って", "支払", "购买",
    "支付", "付款", "खरीदें", "खरीदो", "भुगतान", "اشتر", "ادفع",
];
/// Words that may say the request stops short of the act ("draft a reply, don't send it").
#[rustfmt::skip]
const STOP_SHORT: &[&str] = &[
    "dont", "not", "without", "never", "draft", "nicht", "ohne", "nie", "entwurf", "no", "sin",
    "nunca", "borrador", "ne", "pas", "sans", "jamais", "brouillon", "nao", "sem", "rascunho",
    "ないで", "しない", "下書き", "不要", "别", "不用", "草稿", "मत", "नहीं", "बिना", "ड्राफ्ट", "لا", "دون",
    "بدون", "مسودة",
];

/// What a finished request needed: the least risk class some action must have reached
/// ([`RiskClass::Send`] for "reply", "sende", "返信"…, [`RiskClass::Destroy`] for "delete",
/// [`RiskClass::Spend`] for "buy"), and whether the goal may say it stops short of the act.
/// A model's success claim is not trusted when no such action happened: it is a failure, or
/// unverified when the goal may stop short. `None` when the goal names none of these verbs.
pub fn required_risk(goal: &str) -> Option<(RiskClass, bool)> {
    let first_is_mail = plain_words(goal)
        .first()
        .is_some_and(|x| x == "email" || x == "mail");
    let need = if first_is_mail || find_term(goal, SEND_VERBS, true).is_some() {
        RiskClass::Send
    } else if find_term(goal, DESTROY_VERBS, true).is_some() {
        RiskClass::Destroy
    } else if find_term(goal, SPEND_VERBS, true).is_some() {
        RiskClass::Spend
    } else {
        return None;
    };
    Some((need, find_term(goal, STOP_SHORT, false).is_some()))
}

/// A word that can say which item is meant: two characters or more, not a [`STOP_WORDS`] word,
/// and not a bare number of one or two digits ("2 files"; a year such as `2023` still counts,
/// and so does a clock time, which [`words`] keeps whole).
fn is_content_word(lower: &str) -> bool {
    let n = lower.chars().count();
    n >= 2 && !STOP_WORDS.contains(&lower) && !(n <= 2 && lower.chars().all(|c| c.is_ascii_digit()))
}

/// `plural` is `singular` with an `s` or `es` ending.
fn is_plural_of(plural: &str, singular: &str) -> bool {
    plural
        .strip_prefix(singular)
        .is_some_and(|rest| rest == "s" || rest == "es")
}

/// Words of `text` as (spelling, lowercase): runs of letters and digits, with apostrophes inside
/// a word dropped and a possessive `'s` (or a trailing `'`) removed.
fn words(text: &str) -> Vec<(String, String)> {
    let text = text.replace('’', "'");
    // A clock time ("13:00") is one word: its parts alone would match every time on screen.
    let chars: Vec<char> = text.chars().collect();
    let keep = |i: usize| {
        let c = chars[i];
        c.is_alphanumeric()
            || c == '\''
            || (c == ':'
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
    };
    let text: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| if keep(i) { c } else { ' ' })
        .collect();
    let mut out = Vec::new();
    for raw in text.split(' ') {
        let raw = raw.trim_matches('\'');
        let raw = raw
            .strip_suffix("'s")
            .or_else(|| raw.strip_suffix("'S"))
            .unwrap_or(raw);
        let word: String = raw.chars().filter(|c| *c != '\'').collect();
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        out.push((word, lower));
    }
    out
}

/// Lowercase tokens of `text` with a dot (or `@`) between letters or digits: file names and
/// addresses, which name one item whole.
fn dotted_names(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '“' | '”' | ',' | '(' | ')'))
        .map(|w| w.trim_end_matches(['.', '!', '?', ':', ';']).to_lowercase())
        .filter(|w| {
            let b = w.as_bytes();
            (1..b.len().saturating_sub(1)).any(|i| {
                matches!(b[i], b'.' | b'@')
                    && b[i - 1].is_ascii_alphanumeric()
                    && b[i + 1].is_ascii_alphanumeric()
            })
        })
        .collect()
}

/// A label's identity: lowercase with whitespace collapsed.
fn normalize(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Screen text made safe to show in one line: control characters become spaces, whitespace is
/// collapsed, and the result is cut to `max` characters.
fn clean(label: &str, max: usize) -> String {
    let plain: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    truncate(&plain.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|s| s.to_string()).collect()
    }

    #[rustfmt::skip]
    const INBOX: &[&str] = &[
        "Sam Lee — Lunch on Friday?", "Priya Shah — Q3 numbers", "Sam Ortiz — Re: contract draft",
        "Newsletter: Weekly digest", "Newsletter: Product update",
    ];
    const EVENTS: &[&str] = &[
        "Team sync, Mon 10:00",
        "Dentist, Tue 15:30",
        "Team sync, Thu 10:00",
    ];

    /// (goal, screen, the ambiguous term or `None`).
    #[rustfmt::skip]
    #[test]
    fn ambiguity_cases() {
        let files: &[&str] = &["Q3 report.docx — Documents", "Q3 report.docx — Desktop", "Q4 plan.xlsx — Documents"];
        let finals: &[&str] = &["Q3 report.docx", "Q3 report final.docx"];
        let drafts: &[&str] = &["File draft.txt", "File notes.txt", "Folder Projects"];
        let week: &[&str] = &["Monday 09:00 Team standup", "Tuesday 11:00 Design review", "Thursday 13:00 Customer call"];
        let thursday: &[&str] = &["Thursday 13:00 Customer call", "Thursday 14:00 1:1 with Sam"];
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            ("reply to SAM’s message", INBOX, Some("SAM")),
            // A full name, a single match, or more of the goal fitting one item.
            ("Reply to Sam Lee's email", INBOX, None),
            ("Tell Priya the numbers look fine", INBOX, None),
            ("Reply to Sam about the contract", INBOX, None),
            // Several items asked for; the singular is one item.
            ("Delete all newsletters", INBOX, None),
            ("Archive both newsletter emails", INBOX, None),
            ("Delete the newsletters", INBOX, None),
            ("Forward every Sam email to me", INBOX, None),
            ("Delete the newsletter", INBOX, Some("newsletter")),
            ("rename the Q3 report to Q3 final", files, Some("Q3 report")),
            ("rename the Q3 report", finals, Some("Q3 report")),
            ("rename Q3 report final", finals, None),
            ("open the report", &["Reports 2024", "Reports 2023"], Some("report")),
            ("Cancel the dentist", EVENTS, None),
            // Duplicate labels, empty screens, nothing naming a target.
            ("Reply to Sam", &["Sam Lee", "sam  lee", "Priya Shah"], None),
            ("Reply to Sam", &[], None),
            ("Reply to Sam", &["Sam Lee"], None),
            ("", INBOX, None),
            ("reply to the email", INBOX, None),
            ("Reply to Alex", INBOX, None),
            // A whole file name picks one; an extension alone never makes a term.
            ("In Documents, rename draft.txt to final.txt.", drafts, None),
            ("Rename the txt file to final.txt", drafts, None),
            ("Delete the q3 report", &["File q3 report.txt", "File q3 report final.txt"], Some("q3 report")),
            // Clock times are one word; a time still picks out its own event.
            ("Block Wednesday 10:00 in my calendar as \"Focus time\".", week, None),
            ("Move my Thursday 13:00 meeting", thursday, None),
            ("Cancel my Thursday meeting", thursday, Some("Thursday")),
            ("Delete the 2023 report", &["File report-2023.pdf", "File report-2024.pdf"], None),
        ];
        for (goal, screen, want) in cases {
            let got = ambiguity(goal, &t(screen));
            assert_eq!(got.as_ref().map(|a| a.term.as_str()), *want, "{goal}: {got:?}");
            if let Some(a) = got {
                assert!(a.candidates.len() >= 2, "{goal}: {a:?}");
            }
        }
    }

    #[test]
    fn candidates_and_question() {
        let a = ambiguity("Reply to Sam's email and say yes", &t(INBOX)).unwrap();
        assert_eq!(
            a.candidates,
            [
                "Sam Lee — Lunch on Friday?",
                "Sam Ortiz — Re: contract draft"
            ]
        );
        assert_eq!(
            a.question(),
            "Your request says “Sam”, and I see more than one: Sam Lee — Lunch on Friday?, \
             Sam Ortiz — Re: contract draft. Which one did you mean?"
        );
        let a = ambiguity("Move the team sync meeting to 11am", &t(EVENTS)).unwrap();
        assert_eq!(a.term, "team sync");
        assert_eq!(
            a.candidates,
            ["Team sync, Mon 10:00", "Team sync, Thu 10:00"]
        );
        // Cleaned, capped and cut.
        let mut many = vec![
            format!("Sam {}", "x".repeat(200)),
            "Sam\tLee\n<b>hi</b>".to_string(),
        ];
        many.extend((0..8).map(|i| format!("Sam {i}")));
        let a = ambiguity("call Sam", &many).unwrap();
        assert_eq!(a.candidates.len(), MAX_CANDIDATES);
        assert_eq!(a.candidates[0].chars().count(), CANDIDATE_MAX + 1);
        assert!(a.candidates[0].ends_with('…'));
        assert_eq!(a.candidates[1], "Sam Lee <b>hi</b>");
        assert!(!a.question().contains('\n'));
        let w: Vec<String> = words("Sam's  Q3-report, O'Brien’s don't")
            .into_iter()
            .map(|(w, _)| w)
            .collect();
        assert_eq!(w, ["Sam", "Q3", "report", "OBrien", "dont"]);
    }

    #[test]
    fn goal_objects_irreversible_steps_and_required_risk() {
        let report = "Delete the annual report in Documents";
        assert_eq!(goal_object(report).as_deref(), Some("annual report"));
        assert_eq!(goal_object("Delete all the newsletters"), None);
        assert_eq!(goal_object("Empty the trash"), None);
        assert_eq!(
            goal_object("Delete the spam email from Prize Center").as_deref(),
            Some("spam Prize Center")
        );
        let folders = t(&["Folder Documents", "Folder Downloads"]);
        assert!(!shows_object(report, &folders));
        assert!(shows_object(
            report,
            &t(&["File annual-report-2023.pdf", "File notes.txt"])
        ));
        assert!(shows_object("Empty the trash", &folders));

        for (goal, label, want) in [
            (
                "Delete backup.zip from Downloads",
                "Empty Trash",
                Some("empty"),
            ),
            ("Delete backup.zip from Downloads", "Move to Trash", None),
            ("Empty the trash", "Empty Trash", None),
            (
                "Erase this laptop before I sell it",
                "Erase all content",
                None,
            ),
            ("Permanently delete old.txt", "Empty Trash", None),
            // MM1-3: other wordings and the other UI languages.
            ("Delete backup.zip", "Clear Trash", Some("clear trash")),
            ("Delete backup.zip", "Delete forever", Some("forever")),
            (
                "Lösche backup.zip",
                "Papierkorb leeren",
                Some("papierkorb leeren"),
            ),
            (
                "Elimina backup.zip",
                "Vaciar papelera",
                Some("vaciar papelera"),
            ),
            (
                "Supprime backup.zip",
                "Vider la corbeille",
                Some("vider la corbeille"),
            ),
            (
                "Exclua backup.zip",
                "Excluir permanentemente",
                Some("permanentemente"),
            ),
            (
                "backup.zipを削除して",
                "ゴミ箱を空にする",
                Some("ゴミ箱を空"),
            ),
            ("删除 backup.zip", "清空回收站", Some("清空回收站")),
            ("backup.zip हटाएं", "ट्रैश खाली करें", Some("ट्रैश खाली")),
            ("احذف backup.zip", "إفراغ سلة المهملات", Some("إفراغ سلة")),
            ("Vide la corbeille", "Vider la corbeille", None),
        ] {
            assert_eq!(
                unrequested_irreversible(goal, label),
                want,
                "{goal} / {label}"
            );
        }

        use RiskClass::{Destroy, Send, Spend};
        for (goal, want) in [
            ("Reply to Sam and tell him yes", Some((Send, false))),
            ("Email Bob about Q3", Some((Send, false))),
            ("Delete the old newsletter email", Some((Destroy, false))),
            ("Buy a USB-C cable", Some((Spend, false))),
            ("Draft a reply to Sam but don't send it", Some((Send, true))),
            ("Turn on dark mode", None),
            // MM1-2: the object "draft" still needs a send; other languages count.
            ("Send the draft to Priya.", Some((Send, true))),
            ("Sende die E-Mail an Priya.", Some((Send, false))),
            ("Envoie le message à Priya.", Some((Send, false))),
            ("Priyaにメールを送信して", Some((Send, false))),
            ("给Priya回复邮件", Some((Send, false))),
            ("Priya को ईमेल भेजें", Some((Send, false))),
            ("أرسل البريد إلى بريا", Some((Send, false))),
            ("Lösche die alte Datei", Some((Destroy, false))),
            ("Compra un cable USB-C", Some((Spend, false))),
        ] {
            assert_eq!(required_risk(goal), want, "{goal}");
        }
    }
}
