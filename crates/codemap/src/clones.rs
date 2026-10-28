//! Clone detection over normalised token streams.
//!
//! Language modules hand this module one token stream per file: the
//! parser's leaves in document order, comments dropped, identifiers and
//! literals replaced by placeholders, each token carrying the lines it
//! spans. Detection is the jscpd shape: a Rabin-Karp rolling hash
//! over windows of [`CLONE_MIN_TOKENS`] tokens, equal windows merged into
//! maximal runs, a run kept when every occurrence covers
//! [`CLONE_MIN_LINES`] lines or more. Two occurrences with equal
//! normalised tokens are a clone whether the identifiers match (Type-1) or
//! not (Type-2). Gapped clones (Type-3) are out of scope.
//!
//! Everything here is deterministic: token texts are interned in sorted
//! order, window occurrences are visited in file order, and every output
//! list is sorted by content, never by map iteration.
//!
//! **Maximal runs.** A window belongs to the set of every window in every
//! file with the same normalised tokens. A run starts where that set is
//! not the previous window's set shifted by one token, and extends while
//! the set shifts along with it. Two runs may overlap when one stretch is
//! shared by more files than a longer stretch around it (three files share
//! sixty tokens, two of them share eighty): both are reported, each with
//! its own member set, and the union of clone lines per file counts every
//! line once, so overlapping runs never inflate a ratio.

use std::collections::{BTreeMap, BTreeSet};

use codemap_graph::schema::{
    CLONE_MIN_LINES, CLONE_MIN_TOKENS, CloneGroup, CloneOccurrence, sha256_hex,
};

use crate::measures::{LineClass, count_lines};

/// One normalised token with the source lines it spans (1-based,
/// inclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// The token stream of one file.
#[derive(Debug, Clone)]
pub struct FileTokens<'a> {
    pub rel_path: &'a str,
    pub tokens: &'a [Token],
}

/// One member of a run: a token range `[start, end)` in file `file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Occurrence {
    pub file: usize,
    pub start: usize,
    pub end: usize,
}

impl Occurrence {
    /// First and last source line the range spans.
    pub const fn lines(&self, files: &[FileTokens<'_>]) -> (u32, u32) {
        let tokens = files[self.file].tokens;
        (tokens[self.start].start_line, tokens[self.end - 1].end_line)
    }
}

/// A maximal run of equal normalised tokens shared by two or more
/// occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Sorted by file, then start.
    pub occurrences: Vec<Occurrence>,
    /// Tokens in the run.
    pub tokens: usize,
    /// The normalised tokens, for the content digest.
    pub content: Vec<String>,
}

/// Windows repeated more often than this are treated as noise (a generated
/// table of one literal, a run of the same token) and never start or
/// extend a run. Deterministic, and named here so the report can say so.
pub const CLONE_MAX_WINDOW_OCCURRENCES: usize = 4096;

/// Find every clone run across `files` under the policy thresholds.
pub fn detect(files: &[FileTokens<'_>]) -> Vec<Run> {
    detect_with(files, CLONE_MIN_TOKENS, CLONE_MIN_LINES)
}

/// [`detect`] with explicit thresholds, for tests.
pub fn detect_with(files: &[FileTokens<'_>], min_tokens: usize, min_lines: u32) -> Vec<Run> {
    if min_tokens == 0 {
        return Vec::new();
    }
    let ids = intern(files);
    let index = WindowIndex::build(&ids, min_tokens);
    let mut seen: BTreeSet<Vec<Occurrence>> = BTreeSet::new();
    let mut runs = Vec::new();

    for (file, file_ids) in ids.iter().enumerate() {
        if file_ids.len() < min_tokens {
            continue;
        }
        let windows = file_ids.len() - min_tokens + 1;
        for pos in 0..windows {
            let Some(mut members) = index.members(&ids, file, pos, min_tokens) else {
                continue;
            };
            if members.len() < 2 || overlaps_itself(&members, file, pos, min_tokens) {
                continue;
            }
            // Peel the member set from the outside in: the run every member
            // shares, then the run the survivors of its end share, and so
            // on. A set whose run began at an earlier window was emitted
            // from that window already.
            loop {
                if pos > 0 {
                    let previous = index.members(&ids, file, pos - 1, min_tokens);
                    let started_earlier = previous
                        .as_deref()
                        .is_some_and(|previous| is_shifted_subset(&members, previous, -1));
                    if started_earlier {
                        break;
                    }
                }
                let (length, survivors) =
                    index.extend(&ids, file, pos, &members, min_tokens, windows);
                let occurrences = drop_overlaps(
                    members
                        .iter()
                        .map(|&(f, p)| Occurrence {
                            file: f,
                            start: p,
                            end: p + length,
                        })
                        .collect(),
                );
                if occurrences.len() >= 2
                    && !too_short(&occurrences, files, min_lines)
                    && seen.insert(occurrences.clone())
                {
                    let first = occurrences[0];
                    let content = files[first.file].tokens[first.start..first.end]
                        .iter()
                        .map(|token| token.text.clone())
                        .collect();
                    runs.push(Run {
                        occurrences,
                        tokens: length,
                        content,
                    });
                }
                if survivors.len() < 2 || survivors.len() == members.len() {
                    break;
                }
                members = survivors;
            }
        }
    }
    runs.sort_by(|a, b| a.occurrences.cmp(&b.occurrences));
    runs
}

/// Whether every member shifted by `shift` windows is in `of` (both
/// sorted by file then position).
fn is_shifted_subset(members: &[(usize, usize)], of: &[(usize, usize)], shift: isize) -> bool {
    members.iter().all(|&(f, p)| {
        let Some(q) = p.checked_add_signed(shift) else {
            return false;
        };
        of.binary_search(&(f, q)).is_ok()
    })
}

/// A window that matches another window of its own file less than a
/// window apart is periodic content; it never starts a run.
fn overlaps_itself(members: &[(usize, usize)], file: usize, pos: usize, width: usize) -> bool {
    members
        .iter()
        .any(|&(f, p)| f == file && p != pos && p.abs_diff(pos) < width)
}

fn too_short(occurrences: &[Occurrence], files: &[FileTokens<'_>], min_lines: u32) -> bool {
    occurrences.iter().any(|occ| {
        let (start, end) = occ.lines(files);
        end - start + 1 < min_lines
    })
}

/// Clone groups as the snapshot carries them, plus what attaches to the
/// measures around them.
#[derive(Debug, Default)]
pub struct Attached {
    pub groups: Vec<CloneGroup>,
    /// Source lines covered by at least one occurrence, per file.
    pub file_clone_sloc: BTreeMap<String, u32>,
    /// Group ids per callable, parallel to the `callables` given.
    pub callable_groups: Vec<Vec<String>>,
}

/// Run detection over `streams` (path, tokens) and shape the result for the
/// snapshot. `classes` holds each file's line classes under the one
/// eligibility rule; `callables` are `(path, start_line, end_line)` per
/// measured callable, in the order their measures were emitted.
#[allow(clippy::too_many_lines)] // detection and snapshot shaping share one pass
pub fn attach(
    streams: &[(String, Vec<Token>)],
    classes: &BTreeMap<String, Vec<LineClass>>,
    callables: &[(String, u32, u32)],
) -> Attached {
    let files: Vec<FileTokens<'_>> = streams
        .iter()
        .map(|(rel_path, tokens)| FileTokens { rel_path, tokens })
        .collect();
    let runs = detect(&files);

    let mut ids_taken: BTreeMap<String, usize> = BTreeMap::new();
    let mut groups: Vec<CloneGroup> = runs
        .iter()
        .map(|run| {
            let digest = sha256_hex(run.content.join("\u{1f}").as_bytes());
            let base = format!("clone:{}", &digest[..24]);
            let taken = ids_taken.entry(base.clone()).or_insert(0);
            let id = if *taken == 0 {
                base
            } else {
                format!("{base}:{taken}")
            };
            *taken += 1;
            let mut occurrences: Vec<CloneOccurrence> = run
                .occurrences
                .iter()
                .map(|occ| {
                    let (start_line, end_line) = occ.lines(&files);
                    let rel_path = files[occ.file].rel_path;
                    let sloc = classes
                        .get(rel_path)
                        .map_or(0, |classes| count_lines(classes, start_line, end_line).sloc);
                    CloneOccurrence {
                        rel_path: rel_path.to_string(),
                        start_line,
                        end_line,
                        sloc,
                    }
                })
                .collect();
            occurrences.sort_by(|a, b| {
                a.rel_path
                    .cmp(&b.rel_path)
                    .then(a.start_line.cmp(&b.start_line))
                    .then(a.end_line.cmp(&b.end_line))
            });
            let lines = occurrences
                .iter()
                .map(|occ| occ.end_line - occ.start_line + 1)
                .min()
                .unwrap_or(0);
            CloneGroup {
                id,
                occurrences,
                tokens: u32::try_from(run.tokens).unwrap_or(u32::MAX),
                lines,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        let first = |g: &CloneGroup| {
            (
                g.occurrences[0].rel_path.clone(),
                g.occurrences[0].start_line,
                g.id.clone(),
            )
        };
        first(a).cmp(&first(b))
    });

    let mut file_clone_sloc = BTreeMap::new();
    for (rel_path, classes) in classes {
        let mut covered = vec![false; classes.len()];
        for group in &groups {
            for occ in &group.occurrences {
                if occ.rel_path != *rel_path {
                    continue;
                }
                let first = occ.start_line.saturating_sub(1) as usize;
                let last = (occ.end_line as usize).min(classes.len());
                for flag in covered.iter_mut().take(last).skip(first) {
                    *flag = true;
                }
            }
        }
        let sloc = covered
            .iter()
            .zip(classes)
            .filter(|(flag, class)| **flag && **class == LineClass::Source)
            .count();
        file_clone_sloc.insert(rel_path.clone(), u32::try_from(sloc).unwrap_or(u32::MAX));
    }

    let callable_groups = callables
        .iter()
        .map(|(rel_path, start, end)| {
            let mut ids: Vec<String> = groups
                .iter()
                .filter(|group| {
                    group.occurrences.iter().any(|occ| {
                        occ.rel_path == *rel_path
                            && occ.start_line <= *end
                            && *start <= occ.end_line
                    })
                })
                .map(|group| group.id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        })
        .collect();

    Attached {
        groups,
        file_clone_sloc,
        callable_groups,
    }
}

/// Within one run, an occurrence that overlaps an earlier occurrence of
/// the same file (a repeating pattern matching itself one token over) is
/// dropped; the earlier one stays.
fn drop_overlaps(occurrences: Vec<Occurrence>) -> Vec<Occurrence> {
    let mut kept: Vec<Occurrence> = Vec::new();
    for occ in occurrences {
        let overlaps = kept
            .iter()
            .any(|k| k.file == occ.file && k.start.max(occ.start) < k.end.min(occ.end));
        if !overlaps {
            kept.push(occ);
        }
    }
    kept
}

/// Token texts interned in sorted order, so ids are a function of the
/// vocabulary alone.
fn intern(files: &[FileTokens<'_>]) -> Vec<Vec<u32>> {
    let vocabulary: BTreeSet<&str> = files
        .iter()
        .flat_map(|file| file.tokens.iter().map(|token| token.text.as_str()))
        .collect();
    let ids: BTreeMap<&str, u32> = vocabulary
        .into_iter()
        .enumerate()
        .map(|(id, text)| (text, u32::try_from(id).expect("vocabulary fits u32")))
        .collect();
    files
        .iter()
        .map(|file| {
            file.tokens
                .iter()
                .map(|token| ids[token.text.as_str()])
                .collect()
        })
        .collect()
}

const HASH_BASE: u64 = 1_099_511_628_211;

/// Rolling window hashes of every file, and the occurrences behind each
/// hash. Equality is confirmed on the token ids before a window counts as
/// a member, so a hash collision never produces a clone.
struct WindowIndex {
    hashes: Vec<Vec<u64>>,
    occurrences: BTreeMap<u64, Vec<(usize, usize)>>,
}

impl WindowIndex {
    fn build(ids: &[Vec<u32>], width: usize) -> Self {
        let mut hashes = Vec::with_capacity(ids.len());
        let mut occurrences: BTreeMap<u64, Vec<(usize, usize)>> = BTreeMap::new();
        let drop_factor = HASH_BASE.wrapping_pow(u32::try_from(width - 1).expect("width fits"));
        for (file, file_ids) in ids.iter().enumerate() {
            let mut file_hashes = Vec::new();
            if file_ids.len() >= width {
                let mut hash: u64 = 0;
                for id in &file_ids[..width] {
                    hash = hash
                        .wrapping_mul(HASH_BASE)
                        .wrapping_add(u64::from(*id) + 1);
                }
                file_hashes.push(hash);
                for pos in width..file_ids.len() {
                    let out = u64::from(file_ids[pos - width]) + 1;
                    hash = hash
                        .wrapping_sub(out.wrapping_mul(drop_factor))
                        .wrapping_mul(HASH_BASE)
                        .wrapping_add(u64::from(file_ids[pos]) + 1);
                    file_hashes.push(hash);
                }
                for (pos, hash) in file_hashes.iter().enumerate() {
                    occurrences.entry(*hash).or_default().push((file, pos));
                }
            }
            hashes.push(file_hashes);
        }
        Self {
            hashes,
            occurrences,
        }
    }

    /// How far every member of the run starting at `(file, pos)` keeps
    /// matching, in tokens, and which members still match at the window
    /// where the first one stops (empty when the run reaches the end).
    fn extend(
        &self,
        ids: &[Vec<u32>],
        file: usize,
        pos: usize,
        members: &[(usize, usize)],
        min_tokens: usize,
        windows: usize,
    ) -> (usize, Vec<(usize, usize)>) {
        let mut length = min_tokens;
        loop {
            let next = pos + length - min_tokens + 1;
            if next >= windows {
                return (length, Vec::new());
            }
            let Some(ahead) = self.members(ids, file, next, min_tokens) else {
                return (length, Vec::new());
            };
            let shift = isize::try_from(next - pos).expect("shift fits");
            if is_shifted_subset(members, &ahead, shift) {
                length += 1;
                continue;
            }
            let survivors = members
                .iter()
                .copied()
                .filter(|&(f, p)| ahead.binary_search(&(f, p + (next - pos))).is_ok())
                .collect();
            return (length, survivors);
        }
    }

    /// Every window equal to the one at `(file, pos)`, itself included,
    /// sorted by file then position; `None` when the window is noise
    /// ([`CLONE_MAX_WINDOW_OCCURRENCES`]).
    fn members(
        &self,
        ids: &[Vec<u32>],
        file: usize,
        pos: usize,
        width: usize,
    ) -> Option<Vec<(usize, usize)>> {
        let hash = self.hashes[file][pos];
        let bucket = &self.occurrences[&hash];
        if bucket.len() > CLONE_MAX_WINDOW_OCCURRENCES {
            return None;
        }
        let window = &ids[file][pos..pos + width];
        Some(
            bucket
                .iter()
                .copied()
                .filter(|&(f, p)| &ids[f][p..p + width] == window)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct tokens `t<first>` … one per line from `first_line`.
    fn distinct(first: usize, n: usize, first_line: u32) -> Vec<Token> {
        (0..n)
            .map(|i| Token {
                text: format!("t{}", first + i),
                start_line: first_line + u32::try_from(i).unwrap(),
                end_line: first_line + u32::try_from(i).unwrap(),
            })
            .collect()
    }

    fn on_one_line(words: &[&str], line: u32) -> Vec<Token> {
        words
            .iter()
            .map(|w| Token {
                text: (*w).to_string(),
                start_line: line,
                end_line: line,
            })
            .collect()
    }

    fn files<'a>(streams: &'a [(&'a str, Vec<Token>)]) -> Vec<FileTokens<'a>> {
        streams
            .iter()
            .map(|(rel_path, tokens)| FileTokens { rel_path, tokens })
            .collect()
    }

    #[test]
    fn two_files_sharing_a_long_stretch_form_one_run() {
        // a: x y | t0..t11 | z      b: q | t0..t11 | r s
        let mut a = on_one_line(&["x", "y"], 1);
        a.extend(distinct(0, 12, 2));
        a.extend(on_one_line(&["z"], 14));
        let mut b = on_one_line(&["q"], 1);
        b.extend(distinct(0, 12, 2));
        b.extend(on_one_line(&["r", "s"], 14));
        let streams = [("a", a), ("b", b)];
        let files = files(&streams);
        let runs = detect_with(&files, 5, 3);
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].tokens, 12);
        assert_eq!(
            runs[0].occurrences,
            vec![
                Occurrence {
                    file: 0,
                    start: 2,
                    end: 14
                },
                Occurrence {
                    file: 1,
                    start: 1,
                    end: 13
                }
            ]
        );
        assert_eq!(runs[0].occurrences[0].lines(&files), (2, 13));
        assert_eq!(runs[0].content[0], "t0");
        assert_eq!(runs[0].content[11], "t11");
    }

    #[test]
    fn a_stretch_under_the_token_threshold_is_not_a_run() {
        let streams = [("a", distinct(0, 4, 1)), ("b", distinct(0, 4, 1))];
        assert_eq!(detect_with(&files(&streams), 5, 1).len(), 0);
    }

    #[test]
    fn a_run_on_too_few_lines_is_dropped() {
        let streams = [
            ("a", on_one_line(&["t0", "t1", "t2", "t3", "t4", "t5"], 1)),
            ("b", on_one_line(&["t0", "t1", "t2", "t3", "t4", "t5"], 7)),
        ];
        assert_eq!(detect_with(&files(&streams), 5, 2).len(), 0);
        assert_eq!(detect_with(&files(&streams), 5, 1).len(), 1);
    }

    #[test]
    fn periodic_content_never_starts_a_run() {
        // Twenty copies of one token: every window matches every other
        // window of the same file one token over. Noise, not a clone,
        // even when a second file carries the same twenty.
        let a: Vec<Token> = (0..20)
            .map(|i| Token {
                text: "a".into(),
                start_line: i + 1,
                end_line: i + 1,
            })
            .collect();
        let streams = [("a", a.clone()), ("b", a)];
        let runs = detect_with(&files(&streams), 5, 1);
        assert!(runs.is_empty(), "{runs:?}");
    }

    #[test]
    fn three_members_and_a_longer_pair_are_two_runs() {
        // x and y share t0..t13; z shares only t0..t7 with them.
        let mut x = distinct(0, 14, 1);
        let mut y = distinct(0, 14, 1);
        let mut z = distinct(0, 8, 1);
        z.extend(distinct(100, 6, 9));
        x.extend(on_one_line(&["u"], 15));
        y.extend(on_one_line(&["v"], 15));
        let streams = [("x", x), ("y", y), ("z", z)];
        let runs = detect_with(&files(&streams), 4, 1);
        let shapes: Vec<(usize, usize)> = runs
            .iter()
            .map(|r| (r.occurrences.len(), r.tokens))
            .collect();
        assert_eq!(shapes, vec![(3, 8), (2, 14)], "{runs:?}");
    }

    #[test]
    fn output_is_independent_of_file_order() {
        let mut b = on_one_line(&["k"], 1);
        b.extend(distinct(0, 10, 2));
        let forward = [("a", distinct(0, 10, 1)), ("b", b.clone())];
        let backward = [("b", b), ("a", distinct(0, 10, 1))];
        let one = detect_with(&files(&forward), 5, 1);
        let two = detect_with(&files(&backward), 5, 1);
        assert_eq!(one.len(), 1);
        assert_eq!(two.len(), 1);
        assert_eq!(one[0].content, two[0].content);
        assert_eq!(one[0].tokens, 10);
    }
}
