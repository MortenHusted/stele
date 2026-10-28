// Twin oracle, file two. Every twin sits between neighbours that differ
// from its partner's, so no clone run reaches past a body.

fn lead() -> u8 {
    1
}

pub fn exact_b(items: &[u32], limit: u32) -> (u32, u32) {
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
    let total = kept + dropped;
    (kept, total)
}

const MARK: u32 = 7;

pub fn near_b(values: &[i64]) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let mut total = 0;
    let mut largest = i64::MIN;
    if values.is_empty() {
        return out;
    }
    for value in values {
        total += *value;
        if *value > largest {
            largest = *value;
        }
    }
    out.push(("total".to_string(), total));
    total = total.max(0);
    out.push(("largest".to_string(), largest));
    out.push(("count".to_string(), values.len() as i64));
    out
}

struct Marker;

pub fn prefix_b(rows: &[String]) -> usize {
    let mut seen = 0;
    let mut skipped = 0;
    match rows.len() {
        0 => skipped = 1,
        1 => seen = rows[0].len(),
        n if n > 10 => {
            seen = n * 2;
            skipped = n - 10;
        }
        n => {
            seen = n;
            skipped = rows.iter().filter(|row| row.is_empty()).count();
        }
    }
    let weighted = seen * 3 + skipped * 2;
    weighted.max(seen).min(1_000)
}

pub fn small_b(x: u32) -> u32 {
    if x > 1 { x * 2 } else { 1 }
}
