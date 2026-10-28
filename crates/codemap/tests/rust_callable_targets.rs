//! Rust definition locations must identify callable declarations, not enclosing bodies.

use std::collections::{BTreeMap, BTreeSet};

use codemap::schema::{CallSiteCategory, EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

const SOURCE: &str = r"
enum Mode { Search(u32) }
struct Tuple(u32);
struct Holder { callback: fn(u32) -> u32 }
const fn value(x: u32) -> u32 { x }
static STATIC: fn(u32) -> u32 = value;
const CONSTANT: fn(u32) -> u32 = value;
static INITIALIZED: u32 = value(1);
macro_rules! number { () => { 1 } }
fn macro_call() -> u32 { number!() }
trait Required { fn required(&self) -> u32; }
fn trait_call(value: &dyn Required) -> u32 { value.required() }
fn local_closure() { let f = |x| x; let _ = f(1); }
fn same_name() { let same_name = |x| x; let _ = same_name(1); }
fn closure_parameter(f: impl Fn(u32) -> u32) { let _ = f(1); }
fn pointer_parameter(f: fn(u32) -> u32) { let _ = f(1); }
fn function_binding() { let f = value; let _ = f(1); }
fn constructors(mode: Mode) {
    let _ = Mode::Search(1);
    let _ = Tuple(1);
    let _ = matches!(mode, Mode::Search(_));
    if let Mode::Search(x) = mode { let _ = x; }
}
impl Holder {
    fn indirect(&self) { let _ = (self.callback)(1); }
    fn direct(&self) -> u32 { value(1) }
}
fn globals() { let _ = STATIC(1); let _ = CONSTANT(1); }
fn recursive(x: u32) -> u32 { if x == 0 { 0 } else { recursive(x - 1) } }
fn caller() -> u32 { value(1) }
fn nested() -> u32 { fn inner() -> u32 { 1 } inner() }
fn method(h: &Holder) -> u32 { h.direct() }
";

const FILES: [(&str, &str); 3] = [
    (
        "Cargo.toml",
        "[package]\nname = \"callable_targets\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"callable_targets\"\nversion = \"0.1.0\"\n",
    ),
    ("src/lib.rs", SOURCE),
];

#[test]
#[ignore = "requires rust-analyzer and rust-src on PATH"]
fn only_exact_callable_declarations_are_canonical_targets() {
    let files = FILES.map(|(path, content)| (path.to_owned(), content.to_owned()));
    let repo = common::make_repo_with(&files).expect("create declaration fixture");
    let snapshot = extract(&ExtractOptions {
        path: repo,
        calls_required: true,
        measures: false,
        twins: false,
        language: Language::Rust,
    })
    .expect("semantic declaration fixture");
    let names: BTreeMap<_, _> = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.symbol_key.as_str()))
        .collect();
    let actual: BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| (names[edge.from.as_str()], names[edge.to.as_str()]))
        .collect();
    let key = |name: &str| format!("rust:src/lib.rs#{name}");
    let expected = [
        ("Holder::direct", "value"),
        ("recursive", "recursive"),
        ("caller", "value"),
        ("nested", "inner"),
        ("method", "Holder::direct"),
    ]
    .map(|(from, to)| (key(from), key(to)));
    let expected: BTreeSet<_> = expected
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    assert_eq!(actual, expected);
    for (line, text) in SOURCE.lines().enumerate() {
        if text.contains("let f =")
            || text.contains("let same_name =")
            || text.contains("fn closure_parameter")
            || text.contains("fn pointer_parameter")
            || text.contains("fn globals")
            || text.contains("fn indirect")
            || text.contains("let _ = Mode::Search")
            || text.contains("let _ = Tuple")
            || text.contains("matches!(mode")
        {
            let observations: Vec<_> = snapshot
                .call_sites
                .iter()
                .filter(|site| {
                    site.span.start_line == u32::try_from(line + 1).expect("fixture line")
                })
                .collect();
            assert!(
                !observations.is_empty(),
                "missing forbidden-site observation: {text}"
            );
            assert!(
                observations
                    .iter()
                    .filter(|site| site.raw_name != "matches")
                    .all(|site| site.category == CallSiteCategory::Unmapped),
                "non-callable target must be reported Unmapped: {text}"
            );
        }
    }
    for name in ["number", "required"] {
        let sites: Vec<_> = snapshot
            .call_sites
            .iter()
            .filter(|site| site.raw_name == name)
            .collect();
        assert_eq!(sites.len(), 1, "missing unsupported target: {name}");
        assert_eq!(sites[0].category, CallSiteCategory::Unmapped);
    }
    let line = SOURCE
        .lines()
        .position(|line| line.contains("static INITIALIZED"))
        .expect("initializer line")
        + 1;
    let initializer = snapshot
        .call_sites
        .iter()
        .find(|site| site.span.start_line == u32::try_from(line).expect("fixture line"))
        .expect("initializer observation");
    assert_eq!(
        initializer.category,
        CallSiteCategory::Candidate,
        "a real target outside a callable is a candidate, not a resolved edge"
    );
}
