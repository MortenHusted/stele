// Call-impact fixture: labelled call sites.
// In syntax-only mode these are observations only; no canonical `calls`
// edges are emitted. The companion `expected.toml` documents the labels
// for the future P2 semantic phase.

mod callee;
use callee::{Calculator, free_sum};

fn main() {
    let calc = Calculator;
    let _result = calc.add(1, 2); // receiver-qualified
    let _sum = free_sum(3, 4);    // free
    println!("done");             // macro
    println!("{}", free_sum(5, 6)); // macro-argument
}
