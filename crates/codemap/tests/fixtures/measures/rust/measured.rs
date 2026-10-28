//! Hand-counted oracle for the P0 measures. Every callable below is
//! listed in expected.json with the rule behind each increment.

use std::collections::HashMap;

/// Straight line: no decision points.
pub fn straight(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum * 2
}

pub fn nested_ifs(a: i32, b: i32) -> i32 {
    if a > 0 {
        if b > 0 {
            return 1;
        }
        return 2;
    } else if a < -10 {
        return 3;
    }
    0
}

pub fn guarded_match(n: i32, flag: bool) -> &'static str {
    match n {
        0 => "zero",
        1 if flag => "one, flagged",
        1 => "one",
        _ => "many",
    }
}

pub fn logic(a: bool, b: bool, c: bool) -> bool {
    let both = a && b;
    let either = a || b || c;
    both && !either
}

pub fn questions(input: &str) -> Result<i32, std::num::ParseIntError> {
    let first = input.trim().parse::<i32>()?;
    let second = "2".parse::<i32>()?;
    Ok(first + second)
}

pub fn looping(limit: u32) -> u32 {
    let mut n = 0;
    loop {
        n += 1;
        if n >= limit {
            break;
        }
    }
    while n > 0 {
        n -= 1;
    }
    for _ in 0..3 {
        n += 1;
    }
    n
}

pub fn iterator_heavy(values: &[i32]) -> Vec<i32> {
    values
        .iter()
        .filter(|v| **v > 0 && **v % 2 == 0)
        .map(|v| if *v > 10 { *v / 2 } else { *v })
        .filter_map(|v| if v > 3 { Some(v) } else { None })
        .collect()
}

pub fn iterator_heavy_two(map: &HashMap<String, i32>) -> i32 {
    map.values()
        .copied()
        .fold(0, |acc, v| match v {
            0 => acc,
            n if n < 0 => acc - n,
            n => acc + n,
        })
}

pub struct Counter {
    count: u32,
}

impl Counter {
    pub fn bump(&mut self, by: u32) -> u32 {
        if by == 0 {
            return self.count;
        }
        self.count += by;
        self.count
    }
}

pub fn commented(x: i32) -> i32 {
    // A leading comment line.

    /* a block comment
       spanning two lines */
    let y = x + 1; // trailing comment keeps the line as code

    y
}

pub fn outer(a: i32) -> i32 {
    fn inner(b: i32) -> i32 {
        if b > 0 { b } else { -b }
    }
    if a > 100 {
        return inner(a);
    }
    inner(-a)
}

macro_rules! twice {
    ($e:expr) => {
        if $e { 2 } else { 0 }
    };
}

pub fn macro_args(a: bool, b: bool) -> bool {
    assert!(a || b);
    twice!(a && b) > 0
}

pub fn else_chain(a: i32) -> i32 {
    if a == 1 {
        1
    } else if a == 2 {
        2
    } else if a == 3 {
        3
    } else {
        0
    }
}

pub fn deep(items: &[Vec<i32>]) -> usize {
    let mut total = 0;
    for row in items {
        if !row.is_empty() {
            total += row.iter().map(|v| if *v > 0 { 1 } else { 0 }).sum::<usize>();
        }
    }
    total
}

pub struct Settings {
    path: String,
}

pub struct Reader {
    settings: Settings,
}

impl Reader {
    pub fn path_len(self: &Self, extra: usize) -> usize {
        self.settings.path.len() + extra
    }
}

pub async fn fetch(client: &Client) -> Result<usize, Error> {
    let body = client.get("x").send().await?.text().await?;
    Ok(body.len())
}
