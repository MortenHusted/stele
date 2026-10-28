//! Top-level code outside any callable: the file's SLOC must exceed
//! the sum of its callables' SLOC.

pub const LIMIT: u32 = 3;

pub static NAMES: [&str; 2] = ["a", "b"];

#[derive(Debug)]
pub struct Config {
    pub limit: u32,
}

pub fn limit_or(value: Option<u32>) -> u32 {
    value.unwrap_or(LIMIT)
}
