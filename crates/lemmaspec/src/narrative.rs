//! Prose for humans: read facts and rule atoms back through the sentence
//! templates their relations declare.

use crate::artifact::FactValue;

/// Fill a relation's `reads` template with one fact's arguments. Placeholders
/// name a role (`{policy}`) or a position (`{1}`); anything else is left as
/// written so a malformed template never hides the fact.
pub fn read_fact(template: &str, roles: &[String], args: &[FactValue]) -> String {
    let texts: Vec<String> = args.iter().map(value_text).collect();
    read_with(template, roles, &texts)
}

/// Same as [`read_fact`] over already-textual arguments, which lets rule
/// atoms read with their variable names in place.
pub fn read_with(template: &str, roles: &[String], args: &[String]) -> String {
    let mut output = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            output.push_str(&rest[open..]);
            return output;
        };
        let placeholder = &after[..close];
        let index = roles
            .iter()
            .position(|role| role == placeholder)
            .or_else(|| placeholder.parse::<usize>().ok());
        match index.and_then(|index| args.get(index)) {
            Some(value) => output.push_str(value),
            None => output.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    output.push_str(rest);
    output
}

pub fn value_text(value: &FactValue) -> String {
    match value {
        FactValue::Symbol(value) => value.clone(),
        FactValue::Integer(value) => value.to_string(),
    }
}
