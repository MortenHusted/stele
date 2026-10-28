// Call-impact fixture: definitions that are the intended targets of
// labelled call sites in `caller.rs`.

pub struct Calculator;

impl Calculator {
    pub fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }
}

pub fn free_sum(a: i32, b: i32) -> i32 {
    a + b
}
