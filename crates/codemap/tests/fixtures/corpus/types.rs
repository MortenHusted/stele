// Corpus fixture: structs, nested symbols, and inherent methods.

pub struct Repository {
    path: String,
}

impl Repository {
    pub fn commit_pending(&self) {
        fn nested_helper() {}
        nested_helper();
    }

    pub fn method(&self) -> i32 {
        42
    }
}

pub struct Calculator;

impl Calculator {
    pub fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }
}
