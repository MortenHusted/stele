// Twin oracle, file one. `exact_a` has a verbatim twin in b.rs (a clone
// group and a twin at once); `near_a` has a twin in b.rs that inserts two
// statements, so no fifty-token run survives but most five-token shingles
// do; `prefix_a` shares only its opening tokens with `prefix_b`; `small_a`
// and `small_b` are identical but under eighty tokens.

pub fn exact_a(items: &[u32], limit: u32) -> (u32, u32) {
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

pub fn near_a(values: &[i64]) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let mut total = 0;
    let mut largest = i64::MIN;
    for value in values {
        total += *value;
        if *value > largest {
            largest = *value;
        }
    }
    out.push(("total".to_string(), total));
    out.push(("largest".to_string(), largest));
    out.push(("count".to_string(), values.len() as i64));
    out
}

pub fn prefix_a(rows: &[String]) -> usize {
    let mut seen = 0;
    let mut skipped = 0;
    for row in rows {
        if row.is_empty() {
            skipped += 1;
            continue;
        }
        if row.starts_with('#') {
            skipped += 2;
        } else if row.ends_with(';') {
            seen += 3;
        } else {
            seen += 1;
        }
    }
    let bonus = seen / 2;
    seen + skipped + bonus
}

pub fn small_a(x: u32) -> u32 {
    if x > 1 { x * 2 } else { 1 }
}
