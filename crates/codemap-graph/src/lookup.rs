//! Ruby's lexical constant lookup over qualified names, shared by the
//! extractor's heritage resolution and by consumers that resolve a constant
//! someone wrote.

use std::collections::HashMap;

/// What a written constant names among the definitions on record.
#[derive(Debug, PartialEq, Eq)]
pub enum Lexical<'a, T> {
    /// Exactly one definition at the innermost level that has any.
    Unique(&'a T),
    /// Several definitions at that level; a nested constant shadows outer
    /// ones, so the lookup stops there instead of guessing.
    Ambiguous(&'a [T]),
    Unknown,
}

/// Resolve `written` from inside `scope`, the fully qualified lexical owners
/// outermost first (`["Admin", "Admin::Users"]`): each owner innermost
/// first, then the top level, stopping at the first level with a match. A
/// leading `::` skips the owners.
pub fn lexical_constant<'a, T, S: std::hash::BuildHasher>(
    by_qualified: &'a HashMap<String, Vec<T>, S>,
    written: &str,
    scope: &[String],
) -> Lexical<'a, T> {
    let (written, scope) = written
        .strip_prefix("::")
        .map_or((written, scope), |absolute| (absolute, &[][..]));
    for owner in scope
        .iter()
        .rev()
        .map(String::as_str)
        .chain(std::iter::once(""))
    {
        let candidate = if owner.is_empty() {
            written.to_string()
        } else {
            format!("{owner}::{written}")
        };
        match by_qualified.get(&candidate).map(Vec::as_slice) {
            Some([only]) => return Lexical::Unique(only),
            Some(several) if !several.is_empty() => return Lexical::Ambiguous(several),
            _ => {}
        }
    }
    Lexical::Unknown
}
