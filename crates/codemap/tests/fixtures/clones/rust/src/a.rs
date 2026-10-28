// Clone oracle, file one. `summarise` is repeated verbatim in b.rs (an
// exact clone); `tally` is repeated with every name and literal changed
// (a renamed clone); `near_tokens_a` shares under fifty tokens with its
// sibling; `wide` is repeated verbatim but covers under five lines.

use std::collections::BTreeMap;

pub fn summarise(values: &[i64]) -> BTreeMap<&'static str, i64> {
    let mut out = BTreeMap::new();
    let mut total = 0;
    let mut largest = i64::MIN;
    // The running total and the largest value seen so far.
    for value in values {
        total += *value;
        if *value > largest {
            largest = *value;
        }
    }

    out.insert("total", total);
    out.insert("largest", largest);
    out.insert("count", values.len() as i64);
    out
}

pub fn other_a(flag: bool) -> u8 {
    if flag { 1 } else { 2 }
}

pub fn wide() -> u64 {
    let v: [u64; 30] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];
    v.iter().sum()
}

pub fn tally(items: &[u32], limit: u32) -> (u32, u32) {
    let mut index = 0;
    let mut kept = 0;
    let mut dropped = 0;
    while index < items.len() {
        let item = items[index];
        if item <= limit {
            kept += item;
        } else {
            dropped += 1;
        }
        index += 1;
    }
    (kept, dropped)
}

pub fn near_tokens_a(rows: &[String]) -> usize {
    let mut seen = 0;
    for row in rows {
        if row.is_empty() {
            continue;
        }
        seen += 1;
    }
    seen
}
