//! Twin callables: two callables whose normalised token shingles overlap
//! past a named threshold.
//!
//! Clone groups (`clones.rs`) find runs of equal normalised tokens: text
//! copied verbatim or with names changed. Code copied and then edited in
//! the middle (a statement added, two swapped) breaks every long run and
//! is invisible to that detector, though a reader calls it the same
//! method twice. This module measures that shape: for every callable with
//! at least [`TWIN_MIN_TOKENS`] tokens, the multiset of
//! [`TWIN_SHINGLE_TOKENS`]-token windows; two callables are twins when the
//! shingles they share cover at least [`TWIN_MIN_CONTAINMENT_PERCENT`] of
//! the smaller body's shingles.
//!
//! The thresholds come from a run over a 5,192-method Rails repository on
//! 2026-09-15: at five tokens, seventy percent, and eighty tokens per body
//! it named 885 pairs (17 exact, 868 near, 168 of them in `app/`) and the
//! known hand-found pair; eighty percent lost that pair, fifty tokens
//! tripled the count with short boilerplate methods. A shingle seen in more
//! than [`TWIN_NOISE_CALLABLES`] callables is boilerplate (`end end def
//! $id (`) and seeds no candidate pair, though it still counts inside a
//! pair's containment.
//!
//! Every number here is an integer computed from the token stream the
//! adapter already produced; what a twin means is a consumer's business.

use std::collections::BTreeMap;

use crate::clones::Token;
use crate::schema::{
    TWIN_MIN_CONTAINMENT_PERCENT, TWIN_MIN_TOKENS, TWIN_NOISE_CALLABLES, TWIN_SHINGLE_TOKENS,
    TwinPair,
};

/// A callable as detection sees it: its node id and line span in one file.
#[derive(Debug, Clone, Copy)]
pub struct CallableSpan<'a> {
    pub node_id: &'a str,
    pub rel_path: &'a str,
    pub start_line: u32,
    pub end_line: u32,
}

/// Every twin pair over the given token streams and callables, ordered by
/// the pair's node ids.
///
/// A token belongs to the innermost callable whose line span holds its
/// first line, so a nested definition keeps its own tokens, as measures
/// keep their own complexity.
pub fn detect(streams: &[(String, Vec<Token>)], callables: &[CallableSpan<'_>]) -> Vec<TwinPair> {
    let texts = attribute(streams, callables);
    let shingles: Vec<Option<BTreeMap<String, u32>>> = texts
        .iter()
        .map(|tokens| (tokens.len() >= TWIN_MIN_TOKENS).then(|| shingle(tokens)))
        .collect();

    let mut index: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (callable, shingles) in shingles.iter().enumerate() {
        if let Some(shingles) = shingles {
            for key in shingles.keys() {
                index.entry(key.as_str()).or_default().push(callable);
            }
        }
    }

    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for posting in index.values() {
        if posting.len() < 2 || posting.len() > TWIN_NOISE_CALLABLES {
            continue;
        }
        for (position, &first) in posting.iter().enumerate() {
            for &second in &posting[position + 1..] {
                candidates.push((first, second));
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();

    let totals: Vec<u32> = shingles
        .iter()
        .map(|shingles| shingles.as_ref().map_or(0, |s| s.values().sum()))
        .collect();
    let mut pairs: Vec<TwinPair> = candidates
        .into_iter()
        .filter_map(|(first, second)| {
            let (sa, sb) = (shingles[first].as_ref()?, shingles[second].as_ref()?);
            let shared = shared(sa, sb);
            let smaller = totals[first].min(totals[second]);
            if u64::from(shared) * 100
                < u64::from(TWIN_MIN_CONTAINMENT_PERCENT) * u64::from(smaller)
            {
                return None;
            }
            let (a, b) = if callables[first].node_id <= callables[second].node_id {
                (first, second)
            } else {
                (second, first)
            };
            Some(TwinPair {
                a: callables[a].node_id.to_string(),
                b: callables[b].node_id.to_string(),
                shared,
                shingles_a: totals[a],
                shingles_b: totals[b],
            })
        })
        .collect();
    pairs.sort_by(|x, y| x.a.cmp(&y.a).then(x.b.cmp(&y.b)));
    pairs
}

/// Each callable's tokens, in source order.
fn attribute<'a>(
    streams: &'a [(String, Vec<Token>)],
    callables: &[CallableSpan<'_>],
) -> Vec<Vec<&'a str>> {
    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, callable) in callables.iter().enumerate() {
        by_file.entry(callable.rel_path).or_default().push(index);
    }
    let mut texts: Vec<Vec<&'a str>> = vec![Vec::new(); callables.len()];
    for (rel_path, tokens) in streams {
        let Some(indices) = by_file.get(rel_path.as_str()) else {
            continue;
        };
        for token in tokens {
            let owner = indices
                .iter()
                .copied()
                .filter(|&index| {
                    let callable = &callables[index];
                    (callable.start_line..=callable.end_line).contains(&token.start_line)
                })
                .min_by_key(|&index| {
                    let callable = &callables[index];
                    (
                        callable.end_line - callable.start_line,
                        callable.start_line,
                        callable.node_id,
                    )
                });
            if let Some(owner) = owner {
                texts[owner].push(token.text.as_str());
            }
        }
    }
    texts
}

/// The multiset of `TWIN_SHINGLE_TOKENS`-token windows, keyed by the joined
/// normalised text.
fn shingle(tokens: &[&str]) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for window in tokens.windows(TWIN_SHINGLE_TOKENS) {
        *out.entry(window.join("\u{1f}")).or_insert(0) += 1;
    }
    out
}

/// Multiset intersection size.
fn shared(a: &BTreeMap<String, u32>, b: &BTreeMap<String, u32>) -> u32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small
        .iter()
        .map(|(key, count)| (*count).min(large.get(key).copied().unwrap_or(0)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(texts: &[&str], line: u32) -> Vec<Token> {
        texts
            .iter()
            .map(|text| Token {
                text: (*text).to_string(),
                start_line: line,
                end_line: line,
            })
            .collect()
    }

    fn body(prefix: &[&'static str], n: usize) -> Vec<&'static str> {
        // A body of distinct-looking tokens: `$id`, `=`, `$lit`, `;` repeated
        // gives repeating shingles, so vary with the position.
        let vocabulary = [
            "$id", "=", "$lit", ";", "if", "{", "}", "+", "(", ")", "return", "while", "<", ".",
        ];
        let mut out: Vec<&'static str> = prefix.to_vec();
        for i in 0..n {
            out.push(vocabulary[(i * 7 + i / 3) % vocabulary.len()]);
        }
        out
    }

    #[test]
    fn identical_bodies_are_exact_twins_and_short_ones_never_pair() {
        let long = body(&["def"], 100);
        let short = body(&["def"], 40);
        let streams = vec![
            (
                "a.rb".to_string(),
                [tokens(&long, 1), tokens(&short, 50)].concat(),
            ),
            (
                "b.rb".to_string(),
                [tokens(&long, 1), tokens(&short, 50)].concat(),
            ),
        ];
        let callables = [
            CallableSpan {
                node_id: "a::long",
                rel_path: "a.rb",
                start_line: 1,
                end_line: 1,
            },
            CallableSpan {
                node_id: "a::short",
                rel_path: "a.rb",
                start_line: 50,
                end_line: 50,
            },
            CallableSpan {
                node_id: "b::long",
                rel_path: "b.rb",
                start_line: 1,
                end_line: 1,
            },
            CallableSpan {
                node_id: "b::short",
                rel_path: "b.rb",
                start_line: 50,
                end_line: 50,
            },
        ];
        let pairs = detect(&streams, &callables);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!(
            (pairs[0].a.as_str(), pairs[0].b.as_str()),
            ("a::long", "b::long")
        );
        let expected = u32::try_from(101 - TWIN_SHINGLE_TOKENS + 1).expect("fits");
        assert_eq!(pairs[0].shingles_a, expected);
        assert_eq!(pairs[0].shared, pairs[0].shingles_a);
        assert_eq!(pairs[0].shingles_a, pairs[0].shingles_b);
    }

    #[test]
    fn an_edited_copy_is_a_near_twin_and_a_different_body_is_not() {
        let original = body(&["def"], 100);
        let mut edited = original.clone();
        edited.splice(
            40..40,
            ["if", "$id", ".", "$id", "(", ")", "{", "return", "}"],
        );
        edited.splice(
            80..80,
            ["$id", "=", "$id", ".", "$id", "(", "$lit", ")", ";"],
        );
        let other = body(&["fn"], 100).into_iter().rev().collect::<Vec<_>>();
        let streams = vec![
            ("a.rb".to_string(), tokens(&original, 1)),
            ("b.rb".to_string(), tokens(&edited, 1)),
            ("c.rb".to_string(), tokens(&other, 1)),
        ];
        let callables = [
            CallableSpan {
                node_id: "a",
                rel_path: "a.rb",
                start_line: 1,
                end_line: 1,
            },
            CallableSpan {
                node_id: "b",
                rel_path: "b.rb",
                start_line: 1,
                end_line: 1,
            },
            CallableSpan {
                node_id: "c",
                rel_path: "c.rb",
                start_line: 1,
                end_line: 1,
            },
        ];
        let pairs = detect(&streams, &callables);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        let pair = &pairs[0];
        assert_eq!((pair.a.as_str(), pair.b.as_str()), ("a", "b"));
        assert_eq!(
            pair.shingles_b - pair.shingles_a,
            18,
            "eighteen tokens were inserted"
        );
        assert!(pair.shared < pair.shingles_a, "an edit costs shingles");
        assert!(
            u64::from(pair.shared) * 100
                >= u64::from(TWIN_MIN_CONTAINMENT_PERCENT) * u64::from(pair.shingles_a)
        );
    }

    #[test]
    fn tokens_belong_to_the_innermost_callable() {
        let outer = body(&["def"], 100);
        let streams = vec![("a.rb".to_string(), tokens(&outer, 5))];
        let callables = [
            CallableSpan {
                node_id: "outer",
                rel_path: "a.rb",
                start_line: 1,
                end_line: 10,
            },
            CallableSpan {
                node_id: "inner",
                rel_path: "a.rb",
                start_line: 4,
                end_line: 6,
            },
        ];
        let texts = attribute(&streams, &callables);
        assert!(
            texts[0].is_empty(),
            "every token on line 5 sits inside the nested definition"
        );
        assert_eq!(texts[1].len(), 101);
    }
}
