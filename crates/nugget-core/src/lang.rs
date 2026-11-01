//! Text helpers shared across languages: normalisation, word matching and dates (month and
//! weekday names in en, de, fr, es, pt, hi, ja, zh, ar; numeric and CJK forms; relative days).

use time::{Date, Duration, Month, Weekday};

/// Lowercase, with ASCII digits for Arabic-Indic, Devanagari and full-width ones, `'` for `’`,
/// one Arabic alef and no Arabic diacritics or Devanagari nukta.
pub fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| {
            let digit = |base: u32| char::from_digit(c as u32 - base, 10);
            match c {
                '\u{0660}'..='\u{0669}' => digit(0x0660),
                '\u{06F0}'..='\u{06F9}' => digit(0x06F0),
                '\u{0966}'..='\u{096F}' => digit(0x0966),
                '\u{FF10}'..='\u{FF19}' => digit(0xFF10),
                '\u{064B}'..='\u{0652}' | '\u{093C}' => None,
                'أ' | 'إ' | 'آ' => Some('ا'),
                '\u{095E}' => Some('फ'),
                '’' => Some('\''),
                '／' => Some('/'),
                _ => Some(c),
            }
        })
        .collect()
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || ('\u{0900}'..='\u{097F}').contains(&c)
}

fn is_cjk(c: char) -> bool {
    c >= '\u{3000}'
}

/// Byte offsets of `word` in `text`, at word boundaries (anywhere for CJK words).
pub fn word_hits<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let cjk = word.chars().next().is_some_and(is_cjk);
    text.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        cjk || (!text[..i].chars().next_back().is_some_and(word_char)
            && !text[i + word.len()..].chars().next().is_some_and(word_char))
    })
}

/// Whether `text` holds `word` at word boundaries (anywhere for CJK words).
pub fn contains_word(text: &str, word: &str) -> bool {
    word_hits(text, word).next().is_some()
}

#[rustfmt::skip]
const MONTHS: [&[&str]; 12] = [
    &["january", "jan", "januar", "jänner", "janvier", "janv", "enero", "ene", "janeiro", "जनवरी", "يناير"],
    &["february", "feb", "februar", "février", "févr", "fév", "febrero", "fevereiro", "fev", "फरवरी", "फ़रवरी", "فبراير"],
    &["march", "mar", "märz", "mär", "mrz", "mars", "marzo", "março", "मार्च", "مارس"],
    &["april", "apr", "avril", "avr", "abril", "abr", "अप्रैल", "ابريل"],
    &["may", "mai", "mayo", "maio", "मई", "مايو"],
    &["june", "jun", "juni", "juin", "junio", "junho", "जून", "يونيو", "يونيه"],
    &["july", "jul", "juli", "juillet", "juil", "julio", "julho", "जुलाई", "يوليو", "يوليه"],
    &["august", "aug", "août", "agosto", "अगस्त", "اغسطس"],
    &["september", "sep", "sept", "septembre", "septiembre", "setiembre", "setembro", "सितंबर", "सितम्बर", "سبتمبر"],
    &["october", "oct", "oktober", "okt", "octobre", "octubre", "outubro", "अक्टूबर", "अक्तूबर", "اكتوبر"],
    &["november", "nov", "novembre", "noviembre", "novembro", "नवंबर", "नवम्बर", "نوفمبر"],
    &["december", "dec", "dezember", "dez", "décembre", "déc", "diciembre", "dic", "dezembro", "दिसंबर", "दिसम्बर", "ديسمبر"],
];

#[rustfmt::skip]
const WEEKDAYS: [(Weekday, &[&str]); 7] = [
    (Weekday::Monday, &["monday", "montag", "lundi", "lunes", "segunda-feira", "segunda", "सोमवार", "الاثنين", "月曜", "周一", "星期一"]),
    (Weekday::Tuesday, &["tuesday", "dienstag", "mardi", "martes", "terça-feira", "terça", "मंगलवार", "الثلاثاء", "火曜", "周二", "星期二"]),
    (Weekday::Wednesday, &["wednesday", "mittwoch", "mercredi", "miércoles", "miercoles", "quarta-feira", "quarta", "बुधवार", "الاربعاء", "水曜", "周三", "星期三"]),
    (Weekday::Thursday, &["thursday", "donnerstag", "jeudi", "jueves", "quinta-feira", "quinta", "गुरुवार", "बृहस्पतिवार", "الخميس", "木曜", "周四", "星期四"]),
    (Weekday::Friday, &["friday", "freitag", "vendredi", "viernes", "sexta-feira", "sexta", "शुक्रवार", "الجمعة", "金曜", "周五", "星期五"]),
    (Weekday::Saturday, &["saturday", "samstag", "sonnabend", "samedi", "sábado", "sabado", "शनिवार", "السبت", "土曜", "周六", "星期六"]),
    (Weekday::Sunday, &["sunday", "sonntag", "dimanche", "domingo", "रविवार", "الاحد", "日曜", "周日", "周天", "星期日", "星期天"]),
];

/// Words that are a month name elsewhere but a common word in another language.
const NOT_MONTHS: &[&str] = &["out", "set", "ago"];

/// The month a single (normalised) word names, full or abbreviated.
pub fn month_of(word: &str) -> Option<Month> {
    let w = word.trim_end_matches('.');
    if NOT_MONTHS.contains(&w) {
        return None;
    }
    let i = MONTHS.iter().position(|names| names.contains(&w))?;
    Month::try_from(i as u8 + 1).ok()
}

/// The weekday named earliest in (normalised) `text`.
pub fn weekday_in(text: &str) -> Option<Weekday> {
    WEEKDAYS
        .iter()
        .flat_map(|(wd, names)| {
            names
                .iter()
                .filter_map(move |n| Some((word_hits(text, n).next()?, *wd)))
        })
        .min_by_key(|(i, _)| *i)
        .map(|(_, wd)| wd)
}

const TODAY: &[&str] = &[
    "today",
    "tonight",
    "heute",
    "aujourd'hui",
    "ce soir",
    "hoy",
    "esta noche",
    "hoje",
    "आज",
    "今天",
    "今日",
    "اليوم",
];
const TOMORROW: &[&str] = &[
    "tomorrow", "morgen", "demain", "mañana", "amanhã", "कल", "明天", "明日", "غدا",
];
const DAY_AFTER: &[&str] = &[
    "day after tomorrow",
    "übermorgen",
    "après-demain",
    "pasado mañana",
    "depois de amanhã",
    "परसों",
    "后天",
    "明後日",
    "بعد غد",
];
/// Words before "morgen" / "mañana" that make it "morning".
const MORNING: &[&str] = &["guten", "heute", "am", "jeden", "la", "esta", "cada"];

/// `today`, `tomorrow`, `the day after tomorrow` or `in N days` in any of the languages, from
/// `reference`.
pub fn relative_day(text: &str, reference: Date) -> Option<Date> {
    let l = normalize(text);
    if DAY_AFTER.iter().any(|w| contains_word(&l, w)) {
        return Some(reference + Duration::days(2));
    }
    let tomorrow = TOMORROW.iter().any(|w| {
        word_hits(&l, w).any(|i| {
            let before = l[..i].split_whitespace().next_back().unwrap_or("");
            !MORNING.contains(&before)
        })
    });
    if tomorrow {
        return reference.next_day();
    }
    if TODAY.iter().any(|w| contains_word(&l, w)) {
        return Some(reference);
    }
    let toks: Vec<&str> = l.split_whitespace().map(trim_punct).collect();
    toks.windows(3).find_map(|w| {
        let unit = ["day", "tag", "jour", "día", "dia", "ايام", "يوم"];
        let n = w[1].parse::<i64>().ok().filter(|n| (0..=366).contains(n))?;
        (["in", "dans", "en", "em", "خلال"].contains(&w[0])
            && unit.iter().any(|u| w[2].starts_with(u)))
        .then(|| reference + Duration::days(n))
    })
}

fn trim_punct(t: &str) -> &str {
    t.trim_matches(|c: char| {
        (c.is_ascii_punctuation() && c != '-' && c != '\'')
            || c.is_whitespace()
            || "，。、；：！？（）「」『』【】«»“”‘¿¡؟،।".contains(c)
    })
}

fn day_of(tok: &str) -> Option<u8> {
    let t = ["st", "nd", "rd", "th", "er", "º", "°", "ª"]
        .iter()
        .find_map(|s| tok.strip_suffix(s))
        .unwrap_or(tok);
    if t.is_empty() || t.len() > 2 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok().filter(|d| (1..=31).contains(d))
}

fn year_of(tok: &str) -> Option<i32> {
    (tok.len() == 4)
        .then(|| tok.parse().ok())
        .flatten()
        .filter(|y| (1900..=2100).contains(y))
}

/// A numeric date: `2026-11-13`, `13.11.2026`, `13/11/2026`, `2026/11/13` or `13/11`
/// (day first unless only the other reading is valid).
fn numeric(tok: &str, infer: &dyn Fn(Month, u8) -> Option<Date>) -> Option<Date> {
    let tok = tok
        .get(..10)
        .filter(|d| d.as_bytes().get(4) == Some(&b'-'))
        .unwrap_or(tok);
    let sep = tok.chars().find(|c| matches!(c, '-' | '.' | '/'))?;
    let parts: Vec<&str> = tok.split(sep).collect();
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let num = |s: &str| s.parse::<u8>().ok();
    let ymd = |y: i32, m: u8, d: u8| Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok();
    match parts[..] {
        [y, m, d] if y.len() == 4 && sep != '.' => ymd(year_of(y)?, num(m)?, num(d)?),
        [d, m, y] if y.len() == 4 && sep != '-' => {
            let (d, m) = (num(d)?, num(m)?);
            ymd(year_of(y)?, m, d).or_else(|| ymd(year_of(y)?, d, m))
        }
        [a, b] if sep == '/' && a.len() <= 2 && b.len() <= 2 && tok != "24/7" => {
            let (a, b) = (num(a)?, num(b)?);
            Month::try_from(b)
                .ok()
                .and_then(|m| infer(m, a))
                .or_else(|| infer(Month::try_from(a).ok()?, b))
        }
        _ => None,
    }
}

/// Every date written in `text`, in order: ISO and numeric dates, `November 3, 2026`, `3 Nov`,
/// `3. November 2026`, `13 de noviembre de 2026`, `2026年11月13日`, `١٣ نوفمبر`. A date without
/// a year takes `reference`'s year, or the next year when that would be more than a month
/// before `reference`.
pub fn find_dates(text: &str, reference: Date) -> Vec<Date> {
    let text = normalize(text);
    let infer = |m: Month, d: u8| -> Option<Date> {
        let this = Date::from_calendar_date(reference.year(), m, d).ok()?;
        if this < reference - Duration::days(31) {
            Date::from_calendar_date(reference.year() + 1, m, d).ok()
        } else {
            Some(this)
        }
    };
    let dated = |m: Month, d: u8, y: Option<i32>| match y {
        Some(y) => Date::from_calendar_date(y, m, d).ok(),
        None => infer(m, d),
    };
    let mut out: Vec<(usize, Date)> = Vec::new();
    let toks: Vec<(usize, &str)> = text
        .split_whitespace()
        .map(|t| (t.as_ptr() as usize - text.as_ptr() as usize, trim_punct(t)))
        .filter(|(_, t)| !t.is_empty())
        .collect();
    let tok = |i: usize| toks.get(i).map_or("", |t| t.1);
    let de = |i: usize| i + usize::from(matches!(tok(i), "de" | "del" | "of"));
    let mut i = 0;
    while i < toks.len() {
        let (at, t) = toks[i];
        if let Some(d) = numeric(t, &infer) {
            out.push((at, d));
            i += 1;
            continue;
        }
        // Month day[,] [year]; not "mar. 3 nov." (Spanish/French Tuesday).
        if let Some(m) = month_of(t)
            && let Some(d) = day_of(tok(i + 1))
            && month_of(tok(i + 2)).is_none()
        {
            let y = year_of(tok(i + 2));
            if let Some(date) = dated(m, d, y) {
                out.push((at, date));
                i += 2 + usize::from(y.is_some());
                continue;
            }
        }
        // day [de] Month [de] [year]
        if let Some(d) = day_of(t)
            && let Some(m) = month_of(tok(de(i + 1)))
        {
            let next = de(de(i + 1) + 1);
            let y = year_of(tok(next));
            if let Some(date) = dated(m, d, y) {
                out.push((at, date));
                i = if y.is_some() { next + 1 } else { de(i + 1) + 1 };
                continue;
            }
        }
        i += 1;
    }
    // [YYYY年]M月D日 / M月D号
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let digits_back = |k: usize, max: usize| {
        let n = chars[..k]
            .iter()
            .rev()
            .take_while(|(_, c)| c.is_ascii_digit())
            .count()
            .min(max);
        (
            k - n,
            chars[k - n..k].iter().map(|(_, c)| c).collect::<String>(),
        )
    };
    for (k, &(_, c)) in chars.iter().enumerate() {
        if c != '月' {
            continue;
        }
        let (start, m) = digits_back(k, 2);
        let after: String = chars[k + 1..]
            .iter()
            .map(|(_, c)| *c)
            .take_while(char::is_ascii_digit)
            .collect();
        let ends = chars.get(k + 1 + after.len()).map(|(_, c)| *c);
        let (Ok(m), Ok(d)) = (m.parse::<u8>(), after.parse::<u8>()) else {
            continue;
        };
        if !matches!(ends, Some('日' | '号')) || after.len() > 2 {
            continue;
        }
        let (from, y) = match start.checked_sub(1) {
            Some(j) if chars[j].1 == '年' => {
                let (ys, y) = digits_back(j, 4);
                (ys, year_of(&y))
            }
            _ => (start, None),
        };
        if let Some(date) = Month::try_from(m).ok().and_then(|m| dated(m, d, y)) {
            out.push((chars[from].0, date));
        }
    }
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, d)| d).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(y: i32, m: u8, d: u8) -> Date {
        Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap()
    }

    #[test]
    fn dates_in_nine_languages() {
        let r = ymd(2026, 10, 28);
        for (text, want) in [
            ("renews on November 3, 2026.", Some(ymd(2026, 11, 3))),
            ("bis 3. Dezember", Some(ymd(2026, 12, 3))),
            ("am 16. November 2026", Some(ymd(2026, 11, 16))),
            ("fällig 13.11.2026", Some(ymd(2026, 11, 13))),
            ("antes del 7/11", Some(ymd(2026, 11, 7))),
            ("11月13日（周五）上午", Some(ymd(2026, 11, 13))),
            ("2027年1月5日", Some(ymd(2027, 1, 5))),
            ("el 13 de noviembre de 2026", Some(ymd(2026, 11, 13))),
            ("le 13 novembre", Some(ymd(2026, 11, 13))),
            ("Le mar. 3 nov.", Some(ymd(2026, 11, 3))),
            ("حتى ١٣ نوفمبر", Some(ymd(2026, 11, 13))),
            ("13 नवंबर तक", Some(ymd(2026, 11, 13))),
            ("may I ask", None),
            ("out 3 days, 24/7 support", None),
        ] {
            assert_eq!(find_dates(text, r).first().copied(), want, "{text}");
        }
        for (text, want) in [
            ("bis morgen", Some(ymd(2026, 10, 29))),
            ("guten Morgen, heute noch", Some(r)),
            ("明天", Some(ymd(2026, 10, 29))),
            ("dans 3 jours", Some(ymd(2026, 10, 31))),
            ("por la mañana", None),
        ] {
            assert_eq!(relative_day(text, r), want, "{text}");
        }
        assert_eq!(weekday_in(&normalize("bis Montag")), Some(Weekday::Monday));
        assert_eq!(weekday_in("下周五"), Some(Weekday::Friday));
    }
}
