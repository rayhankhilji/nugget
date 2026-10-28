//! Token-budgeted context packing with `[n]` citations.
//!
//! Packing is greedy and verified: every addition is rendered and measured with
//! [`nugget_extract::estimate_tokens`] over the *whole* text, and kept only if the total stays
//! within the budget. So the budget holds whatever that estimator does (it need not be additive),
//! and nothing is ever cut silently: a passage that is cut to fit ends with `…` and is reported as
//! truncated.
//!
//! Phases (each in rank order, so earlier sources get room first):
//! 1. the best passage of each ranked item, one citation per item;
//! 2. the chunk before and after each passage (`ord ± 1`);
//! 3. the item's other matching chunks.
//!
//! An item's chunks are rendered in document order under one `[n] Title (source)` header;
//! adjacent chunks merge into one passage and gaps show as `…`.

use std::collections::BTreeMap;

use nugget_core::ids::ItemRef;
use nugget_core::ipc::{Citation, ContextResponse};
use nugget_extract::estimate_tokens;
use nugget_store::{Chunk, ChunkId};

use crate::types::PackedPassage;

/// Everything known about one ranked item, ready to pack.
#[derive(Debug, Clone)]
pub(crate) struct PackItem {
    pub item: ItemRef,
    pub title: Option<String>,
    pub url: Option<String>,
    /// The item is marked sensitive (see `Citation::sensitive`).
    pub sensitive: bool,
    pub primary: Chunk,
    /// Chunks at `ord - 1` and `ord + 1` that exist.
    pub neighbours: Vec<Chunk>,
    /// Other matching chunks, best first.
    pub secondary: Vec<Chunk>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PackOptions {
    pub budget: usize,
    pub expand_neighbours: bool,
    pub include_secondary: bool,
    pub min_truncated_tokens: usize,
}

#[derive(Debug, Clone)]
struct Piece {
    id: ChunkId,
    text: String,
}

#[derive(Debug, Clone)]
struct Block {
    n: u32,
    /// Index into the `PackItem` slice.
    idx: usize,
    pieces: BTreeMap<u32, Piece>,
    truncated: bool,
}

/// Separator between non-adjacent chunks of one item.
const GAP: &str = "\n…\n";

fn header(n: u32, it: &PackItem) -> String {
    let title = it
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(it.item.key.as_str());
    let source = it.url.as_deref().unwrap_or(it.item.source.as_str());
    // Keep the header on one line whatever the metadata holds.
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("[{n}] {} ({})", one_line(title), one_line(source))
}

fn render(blocks: &[Block], items: &[PackItem]) -> String {
    let mut out = String::new();
    for (bi, b) in blocks.iter().enumerate() {
        if bi > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&header(b.n, &items[b.idx]));
        out.push('\n');
        let mut prev: Option<u32> = None;
        for (ord, piece) in &b.pieces {
            match prev {
                None => {}
                Some(p) if p + 1 == *ord => out.push('\n'),
                Some(_) => out.push_str(GAP),
            }
            out.push_str(&piece.text);
            prev = Some(*ord);
        }
    }
    out
}

fn fits(blocks: &[Block], items: &[PackItem], budget: usize) -> bool {
    estimate_tokens(&render(blocks, items)) <= budget
}

/// The first `chars` characters of `text`, cut back to a word boundary when there is one, plus `…`.
fn cut(text: &str, chars: usize) -> String {
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i);
    if end >= text.len() {
        return text.to_owned();
    }
    let head = &text[..end];
    let head = match head.rfind(char::is_whitespace) {
        Some(i) if i > 0 => &head[..i],
        _ => head,
    };
    format!("{}…", head.trim_end())
}

/// Pack `items` (in rank order) into at most `opts.budget` estimated tokens.
pub(crate) fn pack(items: &[PackItem], opts: PackOptions) -> (ContextResponse, Vec<PackedPassage>) {
    let mut blocks: Vec<Block> = Vec::new();
    let budget = opts.budget;

    // Phase 1: best passage per item.
    for (idx, it) in items.iter().enumerate() {
        let text = it.primary.text.trim();
        if text.is_empty() {
            continue;
        }
        let n = blocks.len() as u32 + 1;
        let mut block = Block {
            n,
            idx,
            pieces: BTreeMap::from([(
                it.primary.ord,
                Piece {
                    id: it.primary.id,
                    text: text.to_owned(),
                },
            )]),
            truncated: false,
        };
        blocks.push(block.clone());
        if fits(&blocks, items, budget) {
            continue;
        }
        blocks.pop();
        // Too long whole: cut it to the longest prefix that fits, if enough of it remains.
        let total = text.chars().count();
        let (mut lo, mut hi) = (0usize, total);
        let mut best: Option<String> = None;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let piece = cut(text, mid);
            block.pieces.insert(
                it.primary.ord,
                Piece {
                    id: it.primary.id,
                    text: piece.clone(),
                },
            );
            blocks.push(block.clone());
            let ok = fits(&blocks, items, budget);
            blocks.pop();
            if ok {
                best = Some(piece);
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if let Some(piece) = best.filter(|p| estimate_tokens(p) >= opts.min_truncated_tokens) {
            block.pieces.insert(
                it.primary.ord,
                Piece {
                    id: it.primary.id,
                    text: piece,
                },
            );
            block.truncated = true;
            blocks.push(block);
        }
    }

    // Phases 2 and 3: grow whole (untruncated) passages while the budget allows.
    let mut grow = |pick: &dyn Fn(&PackItem) -> &[Chunk]| {
        for bi in 0..blocks.len() {
            if blocks[bi].truncated {
                continue;
            }
            let it = &items[blocks[bi].idx];
            for ch in pick(it) {
                let text = ch.text.trim();
                if ch.item != it.item || text.is_empty() || blocks[bi].pieces.contains_key(&ch.ord)
                {
                    continue;
                }
                blocks[bi].pieces.insert(
                    ch.ord,
                    Piece {
                        id: ch.id,
                        text: text.to_owned(),
                    },
                );
                if !fits(&blocks, items, budget) {
                    blocks[bi].pieces.remove(&ch.ord);
                }
            }
        }
    };
    if opts.expand_neighbours {
        grow(&|it| &it.neighbours);
    }
    if opts.include_secondary {
        grow(&|it| &it.secondary);
    }

    // Every step above was measured; this only guards against a non-monotone estimator.
    while !blocks.is_empty() && !fits(&blocks, items, budget) {
        blocks.pop();
    }

    let text = render(&blocks, items);
    let citations = blocks
        .iter()
        .map(|b| {
            let it = &items[b.idx];
            Citation {
                n: b.n,
                item: it.item.clone(),
                chunk_id: it.primary.id,
                title: it.title.clone(),
                url: it.url.clone(),
                sensitive: it.sensitive,
            }
        })
        .collect();
    let passages = blocks
        .iter()
        .map(|b| PackedPassage {
            n: b.n,
            item: items[b.idx].item.clone(),
            chunk_ids: b.pieces.values().map(|p| p.id).collect(),
            truncated: b.truncated,
        })
        .collect();
    (
        ContextResponse {
            text,
            citations,
            excluded_sensitive: 0,
        },
        passages,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, SourceId};

    fn iref(key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new("notes"),
            key: ItemKey::new(key),
        }
    }

    fn chunk(key: &str, id: ChunkId, ord: u32, text: &str) -> Chunk {
        Chunk {
            id,
            item: iref(key),
            ord,
            text: text.into(),
            title: Some(key.to_uppercase()),
        }
    }

    fn item(key: &str, primary: Chunk, neighbours: Vec<Chunk>, secondary: Vec<Chunk>) -> PackItem {
        PackItem {
            item: iref(key),
            title: Some(key.to_uppercase()),
            url: None,
            sensitive: false,
            primary,
            neighbours,
            secondary,
        }
    }

    fn opts(budget: usize) -> PackOptions {
        PackOptions {
            budget,
            expand_neighbours: true,
            include_secondary: true,
            min_truncated_tokens: 4,
        }
    }

    #[test]
    fn merges_adjacent_chunks_and_marks_gaps() {
        let items = vec![item(
            "a",
            chunk("a", 11, 1, "middle"),
            vec![chunk("a", 10, 0, "first"), chunk("a", 12, 2, "third")],
            vec![chunk("a", 14, 4, "fifth")],
        )];
        let (resp, passages) = pack(&items, opts(1000));
        assert_eq!(resp.text, "[1] A (notes)\nfirst\nmiddle\nthird\n…\nfifth");
        assert_eq!(passages[0].chunk_ids, vec![10, 11, 12, 14]);
        assert_eq!(resp.citations.len(), 1);
        assert_eq!(resp.citations[0].chunk_id, 11);
    }

    #[test]
    fn truncates_only_with_a_marker() {
        let long = "word ".repeat(200);
        let items = vec![item("a", chunk("a", 1, 0, &long), vec![], vec![])];
        let (resp, passages) = pack(&items, opts(40));
        assert!(estimate_tokens(&resp.text) <= 40);
        assert!(resp.text.ends_with('…'));
        assert!(passages[0].truncated);
        // Too small for even the minimum cut: nothing at all.
        let (resp, passages) = pack(&items, opts(6));
        assert!(resp.text.is_empty() && passages.is_empty());
    }

    #[test]
    fn foreign_neighbours_are_ignored() {
        let items = vec![item(
            "a",
            chunk("a", 2, 0, "alpha"),
            vec![chunk("b", 3, 1, "not mine")],
            vec![],
        )];
        let (resp, _) = pack(&items, opts(1000));
        assert!(!resp.text.contains("not mine"));
    }

    #[test]
    fn cut_respects_char_and_word_boundaries() {
        assert_eq!(cut("héllo wörld again", 9), "héllo…");
        assert_eq!(cut("short", 10), "short");
        assert_eq!(cut("nospaceshere", 4), "nosp…");
    }
}
