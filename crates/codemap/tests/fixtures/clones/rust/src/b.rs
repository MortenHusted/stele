// Clone oracle, file two. Every item that has a twin in a.rs sits between
// different neighbours here, so a token run never reaches past the item
// itself.

fn lead() -> u8 {
    1
}

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

struct Marker;

pub fn wide() -> u64 {
    let v: [u64; 30] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];
    v.iter().sum()
}

const LIMIT: u32 = 3;

pub fn count_up(numbers: &[u32], ceiling: u32) -> (u32, u32) {
    let mut cursor = 0;
    let mut sum = 0;
    let mut skipped = 0;
    while cursor < numbers.len() {
        let number = numbers[cursor];
        if number <= ceiling {
            sum += number;
        } else {
            skipped += 7;
        }
        cursor += 2;
    }
    (sum, skipped)
}

fn near_tokens_b(rows: &[String]) -> usize {
    let mut seen = 0;
    for row in rows {
        if row.starts_with('#') {
            seen += 2;
        } else {
            seen += 1;
        }
    }
    seen
}

use std::collections::BTreeMap;
