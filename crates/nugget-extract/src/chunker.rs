//! Structure-aware chunking: sections → paragraphs → sentences/lines → words → characters,
//! packed greedily to the token target, with word-aligned overlap inside a section.

use std::collections::BTreeSet;

use crate::util::floor_char_boundary;
use crate::{ChunkOptions, ChunkOut, Extracted, Section};

/// Token-estimate counters. Additive, so a span's estimate can be grown incrementally.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Counts {
    cjk: usize,
    other: usize,
}

impl Counts {
    pub fn of(s: &str) -> Self {
        let mut c = Self::default();
        if s.is_ascii() {
            c.other = s.len();
            return c;
        }
        for ch in s.chars() {
            c.push(ch);
        }
        c
    }

    fn push(&mut self, ch: char) {
        if is_cjk(ch) {
            self.cjk += 1;
        } else {
            self.other += 1;
        }
    }

    fn add(self, o: Self) -> Self {
        Self {
            cjk: self.cjk + o.cjk,
            other: self.other + o.other,
        }
    }

    pub fn tokens(self) -> usize {
        self.cjk + self.other.div_ceil(4)
    }
}

/// CJK ideographs, kana, Hangul, CJK punctuation and full-width forms: about one token each.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF
        | 0x2E80..=0x2FDF
        | 0x3000..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF
        | 0x20000..=0x3FFFF)
}

/// Byte length of the longest prefix of `s` whose estimate is at most `n` tokens.
fn take_tokens(s: &str, n: usize) -> usize {
    let mut c = Counts::default();
    for (i, ch) in s.char_indices() {
        let mut next = c;
        next.push(ch);
        if next.tokens() > n {
            return i;
        }
        c = next;
    }
    s.len()
}

struct Opts {
    target: usize,
    max: usize,
    overlap: usize,
}

impl Opts {
    fn new(o: &ChunkOptions) -> Self {
        let max = o.max_tokens.max(1);
        Self {
            max,
            target: o.target_tokens.clamp(1, max),
            overlap: o.overlap_tokens.min(max / 2),
        }
    }
}

pub(crate) fn chunk(doc: &Extracted, opts: &ChunkOptions) -> Vec<ChunkOut> {
    let o = Opts::new(opts);
    let text = doc.text.as_str();
    let mut out = Vec::new();
    for seg in segments(text, &doc.sections) {
        chunk_segment(text, &seg, &o, &mut out);
    }
    out
}

/// A contiguous region of the text with one heading path.
struct Seg<'a> {
    start: usize,
    end: usize,
    owner: Option<usize>,
    path: &'a [String],
}

/// Partition `0..text.len()` by the innermost section covering each byte (sections may be
/// unsorted, overlapping, nested or out of range; uncovered bytes get an empty heading path).
fn segments<'a>(text: &str, sections: &'a [Section]) -> Vec<Seg<'a>> {
    let len = text.len();
    let mut iv: Vec<(usize, usize, usize)> = sections
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let end = floor_char_boundary(text, s.end.min(len));
            let start = floor_char_boundary(text, s.start.min(end));
            (start < end).then_some((start, end, i))
        })
        .collect();
    iv.sort_unstable();
    let mut points: Vec<usize> = iv.iter().flat_map(|&(s, e, _)| [s, e]).collect();
    points.extend([0, len]);
    points.sort_unstable();
    points.dedup();

    let mut next = 0;
    let mut active: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut ends: BTreeSet<(usize, usize, usize)> = BTreeSet::new();
    let mut out: Vec<Seg<'a>> = Vec::new();
    for w in points.windows(2) {
        let (p, q) = (w[0], w[1]);
        while let Some(&(s, e, i)) = iv.get(next) {
            if s > p {
                break;
            }
            active.insert((s, i));
            ends.insert((e, s, i));
            next += 1;
        }
        while let Some(&(e, s, i)) = ends.first() {
            if e > p {
                break;
            }
            ends.pop_first();
            active.remove(&(s, i));
        }
        let owner = active.last().map(|&(_, i)| i);
        match out.last_mut() {
            Some(last) if last.end == p && last.owner == owner => last.end = q,
            _ => out.push(Seg {
                start: p,
                end: q,
                owner,
                path: owner.map_or(&[][..], |i| sections[i].heading_path.as_slice()),
            }),
        }
    }
    out
}

/// `"Guide > Install\n"`, shortened to at most a quarter of `max` tokens (dropped if that is
/// under 2 tokens).
fn make_prefix(path: &[String], max: usize) -> String {
    let parts: Vec<&str> = path
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let allowed = max / 4;
    if allowed < 2 {
        return String::new();
    }
    let joined = crate::util::normalize_ws(&parts.join(" > "));
    let mut s = joined.clone();
    s.push('\n');
    if Counts::of(&s).tokens() <= allowed {
        return s;
    }
    let cut = take_tokens(&joined, allowed - 1);
    let mut s = joined[..cut].trim_end().to_owned();
    if s.is_empty() {
        return String::new();
    }
    s.push('\n');
    s
}

fn trim_span(text: &str, a: usize, b: usize) -> Option<(usize, usize)> {
    let s = &text[a..b];
    let lead = s.len() - s.trim_start().len();
    let trail = s.len() - s.trim_end().len();
    (lead + trail < s.len()).then(|| (a + lead, b - trail))
}

/// Blank-line-separated paragraphs of `a..b`, trimmed.
fn paragraphs(text: &str, a: usize, b: usize, out: &mut Vec<(usize, usize)>) {
    let mut start: Option<usize> = None;
    let mut last_end = a;
    let mut pos = a;
    for line in text[a..b].split_inclusive('\n') {
        let ls = pos;
        pos += line.len();
        if line.trim().is_empty() {
            if let Some(st) = start.take() {
                out.extend(trim_span(text, st, last_end));
            }
        } else {
            start.get_or_insert(ls);
            last_end = pos;
        }
    }
    if let Some(st) = start {
        out.extend(trim_span(text, st, last_end));
    }
}

/// Sentences and lines of `a..b`, trimmed.
fn sentences(text: &str, a: usize, b: usize, out: &mut Vec<(usize, usize)>) {
    let s = &text[a..b];
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = i + c.len_utf8();
        let boundary = match c {
            '\n' | '。' | '！' | '？' | '；' => true,
            '.' | '!' | '?' => it.peek().is_some_and(|&(_, n)| n.is_whitespace()),
            _ => false,
        };
        if boundary {
            out.extend(trim_span(text, a + start, a + end));
            start = end;
        }
    }
    if start < s.len() {
        out.extend(trim_span(text, a + start, b));
    }
}

/// Whitespace-separated words of `a..b`.
fn words(text: &str, a: usize, b: usize, out: &mut Vec<(usize, usize)>) {
    let mut start: Option<usize> = None;
    for (i, c) in text[a..b].char_indices() {
        if c.is_whitespace() {
            if let Some(st) = start.take() {
                out.push((a + st, a + i));
            }
        } else {
            start.get_or_insert(i);
        }
    }
    if let Some(st) = start {
        out.push((a + st, b));
    }
}

/// Pieces of `a..b` of at most `budget` tokens each (at least one char each).
fn hard_split(text: &str, a: usize, b: usize, budget: usize, out: &mut Vec<(usize, usize)>) {
    let mut pos = a;
    while pos < b {
        let mut n = take_tokens(&text[pos..b], budget);
        if n == 0 {
            n = text[pos..b].chars().next().map_or(b - pos, char::len_utf8);
        }
        out.push((pos, pos + n));
        pos += n;
    }
}

fn fits(text: &str, (a, b): (usize, usize), budget: usize) -> bool {
    // Cheap pre-check: an estimate never exceeds the byte length.
    b - a <= budget || Counts::of(&text[a..b]).tokens() <= budget
}

/// Atomic pieces of `a..b`, each within `budget`, covering every non-whitespace byte.
fn units(text: &str, a: usize, b: usize, budget: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut paras = Vec::new();
    paragraphs(text, a, b, &mut paras);
    let mut sents = Vec::new();
    let mut ws = Vec::new();
    for p in paras {
        if fits(text, p, budget) {
            out.push(p);
            continue;
        }
        sents.clear();
        sentences(text, p.0, p.1, &mut sents);
        for &s in &sents {
            if fits(text, s, budget) {
                out.push(s);
                continue;
            }
            ws.clear();
            words(text, s.0, s.1, &mut ws);
            for &w in &ws {
                if fits(text, w, budget) {
                    out.push(w);
                } else {
                    hard_split(text, w.0, w.1, budget, &mut out);
                }
            }
        }
    }
    out
}

/// Where an overlap of at most `k` tokens taken from the end of `ps..pe` starts, aligned to a
/// word start. `None` when no such overlap exists or it would repeat the whole span.
fn overlap_start(text: &str, ps: usize, pe: usize, k: usize) -> Option<usize> {
    let mut c = Counts::default();
    let mut pos = pe;
    for (i, ch) in text[ps..pe].char_indices().rev() {
        let mut n = c;
        n.push(ch);
        if n.tokens() > k {
            break;
        }
        c = n;
        pos = ps + i;
    }
    if pos >= pe {
        return None;
    }
    let before = text[..pos].chars().next_back();
    let at = text[pos..].chars().next();
    let aligned = before.is_some_and(|p| p.is_whitespace() || is_cjk(p)) || at.is_some_and(is_cjk);
    if !aligned {
        pos += text[pos..pe].find(char::is_whitespace)?;
    }
    pos += text[pos..pe].find(|c: char| !c.is_whitespace())?;
    (pos > ps && pos < pe).then_some(pos)
}

fn chunk_segment(text: &str, seg: &Seg<'_>, o: &Opts, out: &mut Vec<ChunkOut>) {
    if text[seg.start..seg.end].trim().is_empty() {
        return;
    }
    let prefix = make_prefix(seg.path, o.max);
    let pt = Counts::of(&prefix).tokens();
    let budget = o.max.saturating_sub(pt).max(1);
    let target = o.target.saturating_sub(pt).max(1).min(budget);
    let units = units(text, seg.start, seg.end, budget);

    let mut i = 0;
    let mut prev: Option<(usize, usize)> = None;
    while i < units.len() {
        let (us, ue) = units[i];
        let mut cs = us;
        let mut counts = Counts::of(&text[us..ue]);
        if let (Some((ps, pe)), true) = (prev, o.overlap > 0)
            && let Some(os) = overlap_start(text, ps, pe, o.overlap)
        {
            let with = Counts::of(&text[os..us]).add(counts);
            if with.tokens() <= budget {
                cs = os;
                counts = with;
            }
        }
        let mut ce = ue;
        i += 1;
        while let Some(&(_, ne)) = units.get(i) {
            let grown = counts.add(Counts::of(&text[ce..ne]));
            if grown.tokens() > target {
                break;
            }
            counts = grown;
            ce = ne;
            i += 1;
        }
        let mut chunk_text = String::with_capacity(prefix.len() + ce - cs);
        chunk_text.push_str(&prefix);
        chunk_text.push_str(&text[cs..ce]);
        let tokens = Counts::of(&chunk_text).tokens();
        out.push(ChunkOut {
            text: chunk_text,
            heading_path: seg.path.to_vec(),
            start: cs,
            end: ce,
            tokens,
        });
        prev = Some((cs, ce));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_estimate() {
        assert_eq!(Counts::of("abcd").tokens(), 1);
        assert_eq!(Counts::of("日本語のテキスト").tokens(), 8);
        assert_eq!(Counts::of("한국어").tokens(), 3);
        assert_eq!(Counts::of("ab日本").tokens(), 3);
    }

    #[test]
    fn segments_nested_and_gaps() {
        let text = "0123456789";
        let s = |p: &str, a, b| Section {
            heading_path: vec![p.into()],
            start: a,
            end: b,
        };
        let secs = vec![s("outer", 2, 9), s("inner", 4, 6), s("bad", 50, 60)];
        let segs: Vec<_> = segments(text, &secs)
            .into_iter()
            .map(|g| (g.start, g.end, g.path.first().cloned().unwrap_or_default()))
            .collect();
        assert_eq!(
            segs,
            [
                (0, 2, String::new()),
                (2, 4, "outer".into()),
                (4, 6, "inner".into()),
                (6, 9, "outer".into()),
                (9, 10, String::new()),
            ]
        );
    }

    #[test]
    fn prefix_is_capped() {
        let long: Vec<String> = vec!["x".repeat(400)];
        let p = make_prefix(&long, 40);
        assert!(Counts::of(&p).tokens() <= 10, "{p}");
        assert!(make_prefix(&long, 4).is_empty());
    }
}
