//! Text normalization and keyword matching shared by the classifier, the rules and redaction.

use unicode_normalization::UnicodeNormalization;

/// NFKC, lowercase, invisible characters removed, whitespace collapsed. Fullwidth letters,
/// ligatures and compatibility forms fold to their plain forms; zero-width and bidi controls
/// (often used to split a word like "De\u{200B}lete") disappear.
///
/// Public as [`crate::normalize_text`].
pub fn normalize(s: &str) -> String {
    normalize_with(s, false)
}

/// Like [`normalize`], but invisible characters become a space instead of disappearing, so
/// words joined by one (`ignore\u{2060}previous`) stay separate words. Scanners looking for
/// phrases should try both forms: deleting catches a split word (`Ign\u{200B}ore`), spacing
/// catches joined words.
///
/// Public as [`crate::normalize_text_spaced`].
pub fn normalize_spaced(s: &str) -> String {
    normalize_with(s, true)
}

fn normalize_with(s: &str, invisible_as_space: bool) -> String {
    let folded: String = s
        .nfkc()
        .filter_map(|c| match (is_invisible(c), invisible_as_space) {
            (false, _) => Some(c),
            (true, true) => Some(' '),
            (true, false) => None,
        })
        .collect::<String>()
        .to_lowercase();
    let mut out = String::with_capacity(folded.len());
    let mut space = false;
    for c in folded.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    // "P A Y  N O W" → "pay now" is not recoverable, but "P a y" → "pay" is: when every token
    // is a single character, join them.
    let tokens: Vec<&str> = out.split(' ').collect();
    if tokens.len() >= 3 && tokens.iter().all(|t| t.chars().count() == 1) {
        return tokens.concat();
    }
    out
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
    )
}

/// Map Cyrillic and Greek letters that look like Latin ones to Latin, so "dеlete" with a
/// Cyrillic `е` still reads as "delete". Applied as a second pass only; native Cyrillic keywords
/// are matched on the unfolded text.
pub fn fold_confusables(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'а' => 'a',
            'в' => 'b',
            'с' => 'c',
            'ԁ' => 'd',
            'е' | 'ё' => 'e',
            'һ' => 'h',
            'і' | 'ι' => 'i',
            'ј' => 'j',
            'к' | 'κ' => 'k',
            'м' => 'm',
            'п' => 'n',
            'о' | 'ο' => 'o',
            'р' | 'ρ' => 'p',
            'ԛ' => 'q',
            'г' => 'r',
            'ѕ' => 's',
            'т' | 'τ' => 't',
            'υ' => 'u',
            'ν' => 'v',
            'ԝ' => 'w',
            'х' | 'χ' => 'x',
            'у' => 'y',
            'α' => 'a',
            'ε' => 'e',
            'η' => 'n',
            other => other,
        })
        .collect()
}

/// True for scripts written without spaces between words (or where we cannot rely on
/// word boundaries), where keywords are matched as substrings.
fn substring_script(c: char) -> bool {
    !(c.is_ascii()
        || ('\u{00C0}'..='\u{024F}').contains(&c) // Latin-1 supplement and Latin extended
        || ('\u{0370}'..='\u{03FF}').contains(&c) // Greek
        || ('\u{0400}'..='\u{052F}').contains(&c)) // Cyrillic
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// A keyword pattern: `word` matches a whole word, `stem*` matches words starting with `stem`,
/// and phrases (`move to trash`) match at word boundaries. Keywords in scripts without word
/// spacing (CJK, Arabic, Devanagari, ...) match anywhere.
#[derive(Debug, Clone)]
pub(crate) struct Keyword {
    text: String,
    prefix: bool,
    substring: bool,
}

impl Keyword {
    pub(crate) fn new(pattern: &str) -> Self {
        let (raw, prefix) = match pattern.strip_suffix('*') {
            Some(stem) => (stem, true),
            None => (pattern, false),
        };
        let text = normalize(raw);
        let substring = text.chars().any(substring_script);
        Self {
            text,
            prefix,
            substring,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Match against text already passed through [`normalize`].
    pub(crate) fn matches(&self, haystack: &str) -> bool {
        if self.text.is_empty() {
            return false;
        }
        if self.substring {
            return haystack.contains(self.text.as_str());
        }
        for (start, _) in haystack.match_indices(self.text.as_str()) {
            let before_ok = haystack[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c));
            let end = start + self.text.len();
            let after_ok = self.prefix
                || haystack[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_word_char(c));
            if before_ok && after_ok {
                return true;
            }
        }
        false
    }
}

/// Strip leading and trailing punctuation and symbols, for comparing a whole label against a
/// list ("OK!", "« Continue »").
pub(crate) fn trim_label(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Truncate to at most `max` characters on a char boundary, appending `…` when cut.
pub(crate) fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_and_strips() {
        assert_eq!(normalize("  De\u{200B}lete   NOW "), "delete now");
        assert_eq!(normalize("ＤＥＬＥＴＥ"), "delete");
        assert_eq!(normalize("P a y"), "pay");
        assert_eq!(normalize("Send \u{202A}(⌘Enter)\u{202C}"), "send (⌘enter)");
    }

    #[test]
    fn spaced_variant_keeps_joined_words_apart() {
        assert_eq!(
            normalize_spaced("ignore\u{2060}previous  Instructions"),
            "ignore previous instructions"
        );
        assert_eq!(normalize("ignore\u{2060}previous"), "ignoreprevious");
        assert_eq!(normalize_spaced("Ign\u{200B}ore"), "ign ore");
        assert_eq!(
            fold_confusables(&normalize("Ignоre ＰＲＥＶＩＯＵＳ")),
            "ignore previous"
        );
    }

    #[test]
    fn confusables_fold_to_latin() {
        assert_eq!(fold_confusables(&normalize("Dеlеtе")), "delete");
    }

    #[test]
    fn keyword_modes() {
        let send = Keyword::new("send");
        assert!(send.matches("send now"));
        assert!(send.matches("click to send"));
        assert!(!send.matches("sender"));
        let delete = Keyword::new("delet*");
        assert!(delete.matches("deleting all"));
        assert!(!delete.matches("undelete"));
        let phrase = Keyword::new("move to trash");
        assert!(phrase.matches("move to trash"));
        assert!(!phrase.matches("move to trashcan"));
        let cjk = Keyword::new("删除");
        assert!(cjk.matches("永久删除文件"));
    }

    #[test]
    fn cap_on_char_boundary() {
        assert_eq!(cap("héllo", 3), "hél…");
        assert_eq!(cap("hi", 3), "hi");
    }
}
