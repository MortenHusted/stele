// Corpus fixture: trait methods, in-repo `impl Trait for Type`, and
// external-trait impls that must NOT produce an `implements` edge.

pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct Person {
    name: String,
}

impl Greeter for Person {
    fn greet(&self) -> String {
        format!("Hello, {}", self.name)
    }
}

impl std::fmt::Display for Person {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}
