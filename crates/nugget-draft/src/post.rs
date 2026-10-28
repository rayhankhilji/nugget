//! Streaming post-processing of model output into text that can be inserted as is.
//!
//! The processor is fed raw deltas and returns only text that is **final**: nothing it has
//! emitted is ever taken back, so the overlay can show it and the inserter can type it. To get
//! there it holds back what might still change:
//!
//! - the **head** until it is known not to be a preamble ("Sure, here's a draft reply:"), a
//!   "Subject:" line, an opening quote or code fence, or (when the field already has one) a
//!   greeting;
//! - **partial tokens**: a `[`/`[1` that may become a citation marker, markdown emphasis runs,
//!   and trailing whitespace (so a trailing newline, which would send a chat message, is never
//!   emitted);
//! - the last **short lines**, which may turn out to be a sign-off the field already has, a
//!   closing quote or fence, or a postamble ("Let me know if you'd like changes").
//!
//! Every emitted prefix is checked against the running output, and the length band cuts the
//! draft at a sentence end once it runs past the expected word count.

/// What the post-processor should do for this field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostOptions {
    /// Strip markdown emphasis, inline code and headings (chat apps and rich-text email bodies).
    pub plain_text: bool,
    /// The field is single-line: newlines become spaces.
    pub single_line: bool,
    /// The field already starts with a greeting: drop the draft's own.
    pub strip_greeting: bool,
    /// The field already holds a sign-off or signature: drop the draft's own.
    pub strip_signoff: bool,
    /// Cut the draft at a sentence end once it passes this many words.
    pub max_words: Option<usize>,
}

/// Output of one [`PostProcessor::push`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emit {
    /// Newly final text (may be empty).
    pub text: String,
    /// The draft is complete (length band reached): stop generating.
    pub stop: bool,
}

/// How much undecided head text is buffered before deciding anyway.
const HEAD_WINDOW: usize = 100;
/// Last lines up to this length are held back (sign-off, postamble).
const HOLD_LINE: usize = 100;
/// The line before a held last line is held too when it is this short ("Best," / name).
const HOLD_PREV_LINE: usize = 40;

#[derive(Debug, Clone)]
pub struct PostProcessor {
    opts: PostOptions,
    raw: String,
    /// Byte offset in `raw` where the body starts, once the head is decided.
    skip: Option<usize>,
    fence: bool,
    quote: Option<char>,
    emitted: String,
    finished: bool,
}

impl PostProcessor {
    pub fn new(opts: PostOptions) -> Self {
        Self {
            opts,
            raw: String::new(),
            skip: None,
            fence: false,
            quote: None,
            emitted: String::new(),
            finished: false,
        }
    }

    /// Everything emitted so far.
    pub fn text(&self) -> &str {
        &self.emitted
    }

    /// Whether the draft is complete (the stream ended or the length band was reached).
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Feed a raw delta; returns the text that became final.
    pub fn push(&mut self, delta: &str) -> Emit {
        if self.finished {
            return Emit::default();
        }
        self.raw.push_str(delta);
        if self.skip.is_none() {
            match self.decide_head(false) {
                Some(head) => self.apply_head(head),
                None => return Emit::default(),
            }
        }
        self.emit(false)
    }

    /// The stream ended: flush what was held back, with the closing clean-ups applied.
    pub fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        if self.skip.is_none() {
            let head = self.decide_head(true).unwrap_or_default();
            self.apply_head(head);
        }
        let out = self.emit(true);
        self.finished = true;
        out.text
    }

    fn apply_head(&mut self, head: Head) {
        self.skip = Some(head.pos);
        self.fence = head.fence;
        self.quote = head.quote;
    }

    fn emit(&mut self, last: bool) -> Emit {
        let skip = self.skip.unwrap_or(0).min(self.raw.len());
        let body = &self.raw[skip..];
        let end = if last { body.len() } else { safe_end(body) };
        let cleaned = token_clean(&body[..end], &self.opts);
        let mut candidate = if last {
            self.final_ops(&cleaned)
        } else {
            hold_lines(&cleaned, self.quote, self.fence).to_owned()
        };
        if !candidate.starts_with(&self.emitted) {
            // Nothing emitted is ever retracted; keep what was already sent.
            return Emit::default();
        }
        let mut stop = false;
        if let Some(max) = self.opts.max_words {
            let words = count_words(&candidate);
            if words > max {
                candidate = cut_to_words(&candidate, max, self.emitted.len());
                stop = true;
                self.finished = true;
            } else if !last && words >= max * 3 / 4 {
                // Near the end of the band: emit whole sentences only, so a cut never leaves a
                // dangling fragment.
                let end = last_sentence_end(&candidate)
                    .filter(|e| *e >= self.emitted.len())
                    .unwrap_or(self.emitted.len());
                candidate.truncate(end);
            }
        }
        let text = candidate[self.emitted.len()..].to_owned();
        self.emitted.push_str(&text);
        Emit { text, stop }
    }

    /// Clean-ups that need the end of the draft: closing fence or quote, postamble, a sign-off
    /// the field already has. Applied once, on the held-back tail.
    fn final_ops(&self, cleaned: &str) -> String {
        let mut t = cleaned.trim_end().to_owned();
        strip_postamble(&mut t);
        if self.fence
            && let Some(s) = t.strip_suffix("```")
        {
            t = s.trim_end().to_owned();
        }
        if let Some(q) = self.quote
            && let Some(s) = t.strip_suffix(q).or_else(|| t.strip_suffix('"'))
        {
            t = s.trim_end().to_owned();
        }
        if self.opts.strip_signoff {
            strip_signoff(&mut t);
        }
        t.trim_end().to_owned()
    }

    /// Decide where the body starts, or `None` if more text is needed to tell.
    fn decide_head(&self, last: bool) -> Option<Head> {
        let raw = self.raw.as_str();
        let mut pos = skip_ws(raw, 0);
        for _ in 0..4 {
            let rest = &raw[pos..];
            if rest.is_empty() {
                return last.then_some(Head::at(pos));
            }
            let line = first_line(rest);
            let complete = last || rest.contains('\n') || rest.len() >= HEAD_WINDOW;
            let lower = line.to_ascii_lowercase();
            if !complete && could_be_preamble(&lower) {
                return None;
            }
            match preamble_len(&lower, complete) {
                Some(k) => pos = skip_ws(raw, pos + k),
                None => break,
            }
        }
        let mut head = Head::at(pos);
        let rest = &raw[pos..];
        if rest.starts_with("```") {
            match rest.find('\n') {
                Some(nl) => {
                    head.pos = skip_ws(raw, pos + nl + 1);
                    head.fence = true;
                }
                None if last => return Some(Head::at(raw.len())),
                None => return None,
            }
        } else if let Some(open) = rest.chars().next()
            && let Some(close) = closing_quote(open)
        {
            head.pos = skip_ws(raw, pos + open.len_utf8());
            head.quote = Some(close);
        } else if rest.len() < 3 && !last && "```".starts_with(rest) {
            return None;
        }
        if self.opts.strip_greeting {
            let rest = &raw[head.pos..];
            let line = first_line(rest);
            let lower = line.to_ascii_lowercase();
            let complete = last
                || rest.contains('\n')
                || rest.len() >= HEAD_WINDOW
                || line.contains([',', '!', '.']);
            if !complete && could_be_greeting(&lower) {
                return None;
            }
            if let Some(k) = greeting_len(&lower, rest.contains('\n') || last) {
                head.pos = skip_ws(raw, head.pos + k);
            }
        }
        Some(head)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Head {
    pos: usize,
    fence: bool,
    quote: Option<char>,
}

impl Head {
    fn at(pos: usize) -> Self {
        Self {
            pos,
            fence: false,
            quote: None,
        }
    }
}

fn skip_ws(s: &str, from: usize) -> usize {
    let from = from.min(s.len());
    s.len() - s[from..].trim_start().len()
}

fn first_line(s: &str) -> &str {
    s.split('\n').next().unwrap_or("").trim_end_matches('\r')
}

fn closing_quote(open: char) -> Option<char> {
    match open {
        '"' => Some('"'),
        '“' => Some('”'),
        '«' => Some('»'),
        _ => None,
    }
}

const PREAMBLE_STARTS: &[&str] = &[
    "sure",
    "certainly",
    "of course",
    "absolutely",
    "okay",
    "ok",
    "alright",
    "great",
    "happy to",
    "here",
    "below",
    "i've drafted",
    "i have drafted",
    "i've written",
    "i have written",
    "draft",
    "reply:",
    "response:",
    "subject:",
    "a draft",
    "a reply",
    "a short",
    "a friendly",
    "this draft",
];

/// Words that make a leading line about the draft rather than part of it.
const META_WORDS: &[&str] = &[
    "draft",
    "reply",
    "response",
    "version",
    "suggestion",
    "message",
    "email",
    "rewrite",
    "in your voice",
    "you could send",
    "you can send",
];

/// Could this (partial, lower-cased) first line still turn out to be a preamble?
fn could_be_preamble(lower: &str) -> bool {
    let l = lower.trim_end();
    if l.is_empty() {
        return true;
    }
    PREAMBLE_STARTS
        .iter()
        .any(|s| l.starts_with(s) || s.starts_with(l))
}

/// Length in bytes of a preamble at the start of this lower-cased first line, if it is one.
/// Lower-casing is ASCII-only, so offsets match the original.
fn preamble_len(lower: &str, complete: bool) -> Option<usize> {
    if !PREAMBLE_STARTS.iter().any(|s| lower.starts_with(s)) {
        return None;
    }
    if lower.starts_with("subject:") {
        return complete.then_some(lower.len());
    }
    let has_meta = |s: &str| META_WORDS.iter().any(|m| s.contains(m));
    // "Sure! Here's a reply: Hi Sam, ..." – the preamble ends at the colon.
    if let Some(c) = lower.find(':')
        && c <= HEAD_WINDOW
        && has_meta(&lower[..c])
    {
        return Some(c + 1);
    }
    let t = lower.trim_end();
    if complete && t.len() <= 120 && has_meta(t) && t.ends_with([':', '.', '!']) {
        return Some(lower.len());
    }
    None
}

const GREETINGS: &[&str] = &[
    "hi",
    "hello",
    "hey",
    "dear",
    "good morning",
    "good afternoon",
    "good evening",
    "greetings",
    "hiya",
    "morning",
    "afternoon",
    "evening",
    "yo",
    "howdy",
    "hola",
    "bonjour",
    "hallo",
];

fn greeting_word(lower: &str) -> Option<&'static str> {
    GREETINGS
        .iter()
        .filter(|g| {
            lower.starts_with(**g)
                && lower[g.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric())
        })
        .max_by_key(|g| g.len())
        .copied()
}

fn could_be_greeting(lower: &str) -> bool {
    let l = lower.trim_end();
    l.is_empty() || greeting_word(l).is_some() || GREETINGS.iter().any(|g| g.starts_with(l))
}

/// Length of a greeting at the start of this lower-cased text: a whole short line ("Hi Sam,")
/// or an inline salutation ending at the first comma or exclamation mark ("Hey Sam, thanks").
fn greeting_len(lower: &str, line_complete: bool) -> Option<usize> {
    let g = greeting_word(lower)?;
    let line = first_line(lower);
    if line_complete && line.len() <= 40 && line.split_whitespace().count() <= 5 {
        return Some(line.len());
    }
    let after = &line[g.len()..];
    let end = after.find([',', '!', '.', '—'])?;
    let salutation = &after[..end];
    (salutation.split_whitespace().count() <= 3 && end <= 30).then(|| g.len() + end + 1)
}

/// The longest prefix of `body` that is safe to clean and emit now: trailing whitespace, a
/// partial citation marker and trailing markup characters are held back.
fn safe_end(body: &str) -> usize {
    let mut end = body.len();
    loop {
        let s = &body[..end];
        let t = s.trim_end();
        if t.len() != end {
            end = t.len();
            continue;
        }
        if let Some(i) = s.rfind('[')
            && s[i + 1..]
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == ' ')
        {
            end = i;
            continue;
        }
        let t = s.trim_end_matches(['*', '_', '`', '~']);
        if t.len() != end {
            end = t.len();
            continue;
        }
        let t = s.trim_end_matches('#');
        if t.len() != end && (t.is_empty() || t.ends_with('\n')) {
            end = t.len();
            continue;
        }
        return end;
    }
}

/// A citation marker `[1]`, `[1][2]` is handled by repeated calls, `[1, 2]` at `s[i..]`: its
/// byte length.
fn citation_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.first() != Some(&b'[') {
        return None;
    }
    let mut i = 1;
    let mut digits = 0;
    while i < b.len() {
        match b[i] {
            b'0'..=b'9' => digits += 1,
            b',' | b' ' if digits > 0 => {}
            b']' if digits > 0 && b[i - 1].is_ascii_digit() => return Some(i + 1),
            _ => return None,
        }
        i += 1;
    }
    None
}

/// Remove citation markers (always) and markdown (in plain-text mode); fold newlines in
/// single-line fields. Local rewrites only, so a clean of a prefix is a prefix of the clean of
/// the whole (given [`safe_end`]).
fn token_clean(s: &str, opts: &PostOptions) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(n) = citation_len(rest) {
            if out.ends_with(' ') {
                out.pop();
            }
            i += n;
            continue;
        }
        if opts.plain_text {
            if rest.starts_with("**") || rest.starts_with("__") || rest.starts_with("~~") {
                i += 2;
                continue;
            }
            if rest.starts_with('`') {
                i += 1;
                continue;
            }
            if (out.is_empty() || out.ends_with('\n')) && rest.starts_with('#') {
                let hashes = rest.len() - rest.trim_start_matches('#').len();
                if rest[hashes..].starts_with(' ') {
                    i += hashes + 1;
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        if opts.single_line && (c == '\n' || c == '\r') {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else if c == ' ' && opts.single_line && out.ends_with(' ') {
            // Collapse the spaces folded newlines leave behind.
        } else {
            out.push(c);
        }
        i += c.len_utf8();
    }
    let trimmed = out.trim_start().len();
    out.split_off(out.len() - trimmed)
}

/// The part of `cleaned` that can be emitted before the end of the stream: trailing whitespace
/// and the last short lines are held back, plus a possible closing quote or fence.
fn hold_lines(cleaned: &str, quote: Option<char>, fence: bool) -> &str {
    let mut end = cleaned.len();
    if let Some(nl) = cleaned.rfind('\n')
        && end - nl <= HOLD_LINE
    {
        // The last line, then up to two short non-empty lines before it ("Best," / a name).
        end = cleaned[..nl].trim_end().len();
        for _ in 0..2 {
            match cleaned[..end].rfind('\n') {
                Some(p) if end - p <= HOLD_PREV_LINE => end = cleaned[..p].trim_end().len(),
                _ => break,
            }
        }
    }
    let mut t = cleaned[..end].trim_end();
    if let Some(q) = quote {
        t = t.trim_end_matches([q, '"']).trim_end();
    }
    if fence {
        t = t.trim_end_matches('`').trim_end();
    }
    t
}

const POSTAMBLES: &[&str] = &[
    "let me know if",
    "feel free to",
    "i hope this helps",
    "hope this helps",
    "would you like me to",
    "want me to",
    "i can adjust",
    "i can also",
    "you can adjust",
    "adjust as needed",
    "(feel free",
    "note:",
];

fn strip_postamble(t: &mut String) {
    let Some(para_start) = t.rfind('\n') else {
        return;
    };
    let last = t[para_start..].trim().to_ascii_lowercase();
    let last = last.trim_start_matches(['-', '*', ' ']);
    if POSTAMBLES.iter().any(|p| last.starts_with(p)) && !t[..para_start].trim().is_empty() {
        t.truncate(para_start);
        let n = t.trim_end().len();
        t.truncate(n);
    }
}

const SIGNOFFS: &[&str] = &[
    "best",
    "best regards",
    "best wishes",
    "kind regards",
    "warm regards",
    "warmly",
    "regards",
    "thanks",
    "thanks!",
    "thank you",
    "thanks again",
    "many thanks",
    "cheers",
    "sincerely",
    "yours",
    "yours truly",
    "all the best",
    "talk soon",
    "take care",
    "br",
    "thx",
    "--",
    "-",
    "—",
];

/// Whether a line is a bare sign-off ("Best," / "Thanks!" / "--").
pub(crate) fn is_signoff(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    let l = l.trim_end_matches([',', '!', '.', ' ']);
    !line.trim().is_empty() && (SIGNOFFS.contains(&l) || (line.trim() == "--"))
}

/// Whether a line opens with a greeting ("Hi Sam," / "Hello,").
pub(crate) fn is_greeting_line(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    !l.is_empty() && l.len() <= 60 && greeting_word(&l).is_some()
}

fn looks_like_name(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty()
        && l.len() <= 30
        && l.split_whitespace().count() <= 3
        && !l.ends_with(['.', '?', '!', ':', ','])
        && l.chars().next().is_some_and(char::is_alphabetic)
}

/// Drop the draft's trailing sign-off ("Best,\nSam" or "Thanks!") because the field has one.
fn strip_signoff(t: &mut String) {
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() < 2 {
        return;
    }
    let n = lines.len();
    let cut = if is_signoff(lines[n - 1]) {
        Some(n - 1)
    } else if n >= 3 && looks_like_name(lines[n - 1]) && is_signoff(lines[n - 2]) {
        Some(n - 2)
    } else {
        None
    };
    if let Some(cut) = cut {
        let keep = lines[..cut].join("\n");
        *t = keep.trim_end().to_owned();
    }
}

/// Byte offset just past the last sentence end (`.`, `!`, `?` followed by whitespace, or a
/// newline).
fn last_sentence_end(s: &str) -> Option<usize> {
    s.char_indices()
        .filter(|(i, c)| {
            *c == '\n'
                || (matches!(c, '.' | '!' | '?')
                    && s[i + c.len_utf8()..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace))
        })
        .map(|(i, c)| i + c.len_utf8())
        .next_back()
}

pub(crate) fn count_words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Cut `s` to at most `max` words, at the last sentence end inside the limit when there is one
/// past the half-way mark; never shorter than `min_len` bytes (already emitted).
fn cut_to_words(s: &str, max: usize, min_len: usize) -> String {
    let mut limit = s.len();
    for (n, (i, _)) in s
        .char_indices()
        .filter(|(i, c)| !c.is_whitespace() && (*i == 0 || s[..*i].ends_with(char::is_whitespace)))
        .enumerate()
    {
        if n == max {
            limit = i;
            break;
        }
    }
    let window = &s[..limit];
    let half = s
        .char_indices()
        .filter(|(i, c)| !c.is_whitespace() && (*i == 0 || s[..*i].ends_with(char::is_whitespace)))
        .nth(max / 2)
        .map_or(0, |(i, _)| i);
    let sentence_end = window
        .char_indices()
        .filter(|(i, c)| matches!(c, '.' | '!' | '?' | '\n') && *i >= half)
        .map(|(i, c)| i + c.len_utf8())
        .rfind(|e| *e >= min_len);
    let end = sentence_end.unwrap_or(limit).max(min_len).min(s.len());
    s[..end].trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `chunks` one by one, then finish; return the concatenated output and each emission.
    fn run(opts: PostOptions, chunks: &[&str]) -> (String, Vec<String>) {
        let mut p = PostProcessor::new(opts);
        let mut out = String::new();
        let mut parts = Vec::new();
        for c in chunks {
            let e = p.push(c);
            if !e.text.is_empty() {
                parts.push(e.text.clone());
            }
            out.push_str(&e.text);
            if e.stop {
                break;
            }
        }
        let tail = p.finish();
        if !tail.is_empty() {
            parts.push(tail.clone());
        }
        out.push_str(&tail);
        assert_eq!(out, p.text());
        (out, parts)
    }

    /// Feed the text one character at a time (the harshest split).
    fn by_char(opts: PostOptions, text: &str) -> String {
        let chars: Vec<String> = text.chars().map(String::from).collect();
        let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
        run(opts, &refs).0
    }

    fn plain() -> PostOptions {
        PostOptions {
            plain_text: true,
            ..Default::default()
        }
    }

    #[test]
    fn strips_preamble_line() {
        let text = "Sure! Here's a draft reply:\n\nThursday works, see you at 3.";
        assert_eq!(by_char(plain(), text), "Thursday works, see you at 3.");
        let text = "Here is a short, friendly response in your voice.\nSounds good to me!";
        assert_eq!(by_char(plain(), text), "Sounds good to me!");
    }

    #[test]
    fn strips_inline_preamble_up_to_colon() {
        let text = "Sure, here's a reply: Thursday works for me.";
        assert_eq!(by_char(plain(), text), "Thursday works for me.");
    }

    #[test]
    fn keeps_legitimate_openers() {
        for text in [
            "Sure, Thursday at 3 works.",
            "Great news, congrats!",
            "Here's the deck: https://example.com/deck",
            "Okay, I'll send it tonight.",
            "I have a conflict then, can we do Friday?",
        ] {
            assert_eq!(by_char(plain(), text), text, "{text}");
        }
    }

    #[test]
    fn strips_subject_line_and_wrapping_quotes() {
        let text = "Subject: Re: Thursday\n\n\"Thursday works, see you then.\"";
        assert_eq!(by_char(plain(), text), "Thursday works, see you then.");
        let text = "“Sounds good!”";
        assert_eq!(by_char(plain(), text), "Sounds good!");
    }

    #[test]
    fn strips_code_fence() {
        let text = "```text\nSounds good, thanks!\n```\n";
        assert_eq!(
            by_char(PostOptions::default(), text),
            "Sounds good, thanks!"
        );
    }

    #[test]
    fn removes_citation_markers_across_splits() {
        let text = "The launch moved to May 3 [1] and the budget is fixed [2][3].";
        assert_eq!(
            by_char(plain(), text),
            "The launch moved to May 3 and the budget is fixed."
        );
        assert_eq!(by_char(plain(), "See [1, 2]."), "See.");
        // Brackets that are not markers stay.
        assert_eq!(
            by_char(plain(), "Use [draft] mode [x1]."),
            "Use [draft] mode [x1]."
        );
    }

    #[test]
    fn plain_text_strips_markdown_for_chat() {
        let text = "**Yes** — I'll bring the `slides` and ~~cake~~ snacks.";
        assert_eq!(
            by_char(plain(), text),
            "Yes — I'll bring the slides and cake snacks."
        );
        let text = "# Notes\nAll good";
        assert_eq!(by_char(plain(), text), "Notes\nAll good");
        // Markdown is kept when plain text is off (comments, documents).
        let text = "**Yes**, see `main.rs`.";
        assert_eq!(by_char(PostOptions::default(), text), text);
    }

    #[test]
    fn never_emits_trailing_newlines() {
        let (out, parts) = run(plain(), &["See you then!", "\n", "\n"]);
        assert_eq!(out, "See you then!");
        assert!(parts.iter().all(|p| !p.ends_with('\n')));
        let (out, _) = run(plain(), &["Line one\n", "Line two\n\n"]);
        assert_eq!(out, "Line one\nLine two");
    }

    #[test]
    fn single_line_folds_newlines() {
        let opts = PostOptions {
            single_line: true,
            ..Default::default()
        };
        assert_eq!(
            by_char(opts, "Acme Corp\nLondon office"),
            "Acme Corp London office"
        );
    }

    #[test]
    fn duplicate_greeting_is_dropped() {
        let opts = PostOptions {
            strip_greeting: true,
            ..plain()
        };
        assert_eq!(
            by_char(opts.clone(), "Hi Sam,\n\nThursday works for me."),
            "Thursday works for me."
        );
        assert_eq!(
            by_char(opts.clone(), "Hey Sam, Thursday works for me."),
            "Thursday works for me."
        );
        // Not a greeting: "Hiking" starts with "hi".
        assert_eq!(
            by_char(opts.clone(), "Hiking was great."),
            "Hiking was great."
        );
        // Without an existing greeting, the draft keeps its own.
        assert_eq!(
            by_char(plain(), "Hi Sam,\n\nThursday works."),
            "Hi Sam,\n\nThursday works."
        );
    }

    #[test]
    fn duplicate_signoff_is_dropped() {
        let opts = PostOptions {
            strip_signoff: true,
            ..plain()
        };
        assert_eq!(
            by_char(opts.clone(), "Thursday works for me.\n\nBest,\nTanuj"),
            "Thursday works for me."
        );
        assert_eq!(
            by_char(opts.clone(), "Thursday works for me.\n\nThanks!"),
            "Thursday works for me."
        );
        assert_eq!(
            by_char(plain(), "Thursday works for me.\n\nBest,\nTanuj"),
            "Thursday works for me.\n\nBest,\nTanuj"
        );
    }

    #[test]
    fn postamble_is_dropped() {
        let text = "Thursday works for me.\n\nLet me know if you'd like any changes!";
        assert_eq!(by_char(plain(), text), "Thursday works for me.");
    }

    #[test]
    fn length_band_cuts_at_sentence_end() {
        let opts = PostOptions {
            max_words: Some(8),
            ..plain()
        };
        let text = "Thursday works. I will bring the slides. And then we can talk about everything else at length.";
        let mut p = PostProcessor::new(opts);
        let mut out = String::new();
        let mut stopped = false;
        for w in text.split_inclusive(' ') {
            let e = p.push(w);
            out.push_str(&e.text);
            if e.stop {
                stopped = true;
                break;
            }
        }
        assert!(stopped);
        assert_eq!(out, "Thursday works. I will bring the slides.");
        assert!(p.is_finished());
        assert_eq!(p.finish(), "");
    }

    #[test]
    fn emits_early_for_ordinary_text() {
        // Time to first token: an ordinary reply is emitted before the stream ends.
        let mut p = PostProcessor::new(plain());
        let e = p.push("Thursday works ");
        assert_eq!(e.text, "Thursday works");
        let e = p.push("for me");
        assert_eq!(e.text, " for me");
    }

    #[test]
    fn emitted_text_is_never_retracted() {
        let texts = [
            "Sure! Here's a draft:\n\n\"Hi Sam,\n\nThursday [1] works.\n\nBest,\nT\"\n\nLet me know if that works!",
            "```\n**Yes**\n```",
            "Hello there\n\n\n",
            "   ",
            "",
        ];
        for text in texts {
            for opts in [
                plain(),
                PostOptions {
                    strip_greeting: true,
                    strip_signoff: true,
                    ..plain()
                },
            ] {
                let mut p = PostProcessor::new(opts);
                let mut out = String::new();
                for c in text.chars() {
                    out.push_str(&p.push(&c.to_string()).text);
                    assert_eq!(out, p.text());
                }
                out.push_str(&p.finish());
                assert_eq!(out, p.text());
                assert!(!out.ends_with('\n') && !out.starts_with(char::is_whitespace));
            }
        }
    }

    #[test]
    fn full_wrapped_draft_cleans_up() {
        let opts = PostOptions {
            strip_greeting: true,
            strip_signoff: true,
            ..plain()
        };
        let text = "Sure! Here's a draft:\n\n\"Hi Sam,\n\nThursday [1] works.\n\nBest,\nT\"\n\nLet me know if that works!";
        assert_eq!(by_char(opts, text), "Thursday works.");
    }

    #[test]
    fn line_helpers() {
        assert!(is_signoff("Best,"));
        assert!(is_signoff("Kind regards"));
        assert!(is_signoff("--"));
        assert!(!is_signoff("Best idea ever."));
        assert!(is_greeting_line("Hi Sam,"));
        assert!(is_greeting_line("Good morning team!"));
        assert!(!is_greeting_line("Hiking tomorrow?"));
    }
}
