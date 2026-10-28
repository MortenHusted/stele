//! Typed compiler evidence for exact source-backed admission.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Free,
    Static,
    NonvirtualMember,
}

pub struct Token {
    pub length: u32,
    pub kind: String,
    pub modifiers: BTreeSet<String>,
}

pub struct SemanticTokens {
    tokens: BTreeMap<(u32, u32), Token>,
}

impl SemanticTokens {
    pub fn parse(capabilities: &Value, response: &Value, source: &str) -> Result<Self> {
        let legend = &capabilities["semanticTokensProvider"]["legend"];
        let types = names(&legend["tokenTypes"])?;
        let modifiers = names(&legend["tokenModifiers"])?;
        if modifiers.len() > 32
            || modifiers.iter().collect::<BTreeSet<_>>().len() != modifiers.len()
            || [
                "virtual",
                "static",
                "dependentName",
                "constructorOrDestructor",
                "classScope",
            ]
            .iter()
            .any(|required| !modifiers.contains(required))
        {
            bail!("clangd semantic-token modifier legend is unusable");
        }
        let data = response["data"]
            .as_array()
            .context("clangd full semantic-token data is missing")?;
        if data.len() % 5 != 0 {
            bail!("clangd semantic-token record is incomplete");
        }
        let lines = source
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect::<Vec<_>>();
        let mut tokens = BTreeMap::new();
        let (mut line, mut character, mut previous_end) = (0u32, 0u32, None);
        for row in data.chunks_exact(5) {
            let delta_line = integer(&row[0])?;
            let delta_character = integer(&row[1])?;
            line = line
                .checked_add(delta_line)
                .context("token line overflow")?;
            character = if delta_line == 0 {
                character
                    .checked_add(delta_character)
                    .context("token column overflow")?
            } else {
                delta_character
            };
            let length = integer(&row[2])?;
            let kind = types
                .get(integer(&row[3])? as usize)
                .context("unknown token type index")?;
            let flags = integer(&row[4])?;
            if length == 0 || (modifiers.len() < 32 && flags >> modifiers.len() != 0) {
                bail!("invalid clangd token length or modifier bits");
            }
            let end = character
                .checked_add(length)
                .context("token end overflow")?;
            let text = lines
                .get(line as usize)
                .context("token line outside source")?;
            if boundary(text, character).is_none() || boundary(text, end).is_none() {
                bail!("clangd token has an invalid UTF-16 source span");
            }
            if previous_end
                .is_some_and(|(old_line, old_end)| old_line == line && character < old_end)
            {
                bail!("clangd semantic tokens overlap");
            }
            previous_end = Some((line, end));
            let selected = modifiers
                .iter()
                .enumerate()
                .filter(|(bit, _)| flags & (1u32 << bit) != 0)
                .map(|(_, name)| (*name).to_string())
                .collect();
            tokens.insert(
                (line, character),
                Token {
                    length,
                    kind: (*kind).to_string(),
                    modifiers: selected,
                },
            );
        }
        Ok(Self { tokens })
    }

    pub fn exact_token(&self, line: u32, character: u32, length: u32) -> Option<&Token> {
        self.tokens
            .get(&(line, character))
            .filter(|token| token.length == length)
    }

    /// Requires the exact callee token. A missing token proves nothing.
    pub fn call_kind(&self, line: u32, character: u32, length: u32) -> Option<CallKind> {
        let token = self.exact_token(line, character, length)?;
        if [
            "virtual",
            "abstract",
            "dependentName",
            "constructorOrDestructor",
        ]
        .iter()
        .any(|flag| token.modifiers.contains(*flag))
        {
            return None;
        }
        match token.kind.as_str() {
            "function" if !token.modifiers.contains("classScope") => Some(CallKind::Free),
            "function" if token.modifiers.contains("static") => Some(CallKind::Static),
            "method"
                if token.modifiers.contains("classScope")
                    && !token.modifiers.contains("static") =>
            {
                Some(CallKind::NonvirtualMember)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolIdentity {
    usr: String,
    declaration: Location,
}

impl SymbolIdentity {
    /// Ambiguous or absent answers cannot pair a prototype with a body.
    pub fn parse(response: &Value) -> Result<Option<Self>> {
        Ok(SymbolInfo::parse(response)?.map(|info| info.identity))
    }
}

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub identity: SymbolIdentity,
    pub definition: Option<Location>,
    pub name: Option<String>,
    pub container: Option<String>,
}

impl SymbolInfo {
    pub fn parse(response: &Value) -> Result<Option<Self>> {
        let rows = response
            .as_array()
            .context("clangd symbolInfo is not an array")?;
        let infos = rows
            .iter()
            .map(|row| {
                let usr = row["usr"]
                    .as_str()
                    .filter(|usr| !usr.is_empty() && !usr.contains('\0'))
                    .context("clangd symbolInfo has no usable USR")?;
                let declaration = row.get("declarationRange").unwrap_or(&Value::Null);
                let declaration = if declaration.is_null() {
                    None
                } else {
                    Some(Location::parse(declaration)?)
                };
                let definition = row
                    .get("definitionRange")
                    .filter(|value| !value.is_null())
                    .map(Location::parse)
                    .transpose()?;
                let name = optional_text(&row["name"])?;
                let container = optional_text(&row["containerName"])?;
                Ok(declaration.map(|declaration| Self {
                    identity: SymbolIdentity {
                        usr: usr.to_string(),
                        declaration,
                    },
                    definition,
                    name,
                    container,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(if infos.len() == 1 {
            infos.into_iter().next().flatten()
        } else {
            None
        })
    }

    pub fn owner_location(&self) -> &Location {
        self.definition
            .as_ref()
            .unwrap_or(&self.identity.declaration)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    uri: String,
    start: (u32, u32),
    end: (u32, u32),
}

impl Location {
    fn parse(location: &Value) -> Result<Self> {
        let uri = location["uri"]
            .as_str()
            .filter(|uri| valid_file_uri(uri))
            .context("clangd symbol location has no file URI")?;
        let range = &location["range"];
        let start = (
            integer(&range["start"]["line"])?,
            integer(&range["start"]["character"])?,
        );
        let end = (
            integer(&range["end"]["line"])?,
            integer(&range["end"]["character"])?,
        );
        if end <= start {
            bail!("clangd symbol range is empty or reversed");
        }
        Ok(Self {
            uri: uri.to_string(),
            start,
            end,
        })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn matches(&self, uri: &str, source: &str, span: &crate::schema::Span) -> bool {
        self.uri == uri
            && span.end_byte > span.start_byte
            && source.is_char_boundary(span.start_byte)
            && source.is_char_boundary(span.end_byte)
            && crate::lsp::byte_offset_to_lsp_pos(source.as_bytes(), span.start_byte) == self.start
            && crate::lsp::byte_offset_to_lsp_pos(source.as_bytes(), span.end_byte) == self.end
    }
}

fn optional_text(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .filter(|text| !text.contains('\0'))
        .context("clangd symbol name or container is invalid")?;
    Ok((!text.is_empty()).then(|| text.to_string()))
}

fn valid_file_uri(uri: &str) -> bool {
    let Some(path) = uri.strip_prefix("file:///") else {
        return false;
    };
    if path.is_empty() || uri.contains(['?', '#']) || uri.bytes().any(|b| b.is_ascii_whitespace()) {
        return false;
    }
    let mut decoded = vec![b'/'];
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let digits = (
                bytes.next().and_then(|b| (b as char).to_digit(16)),
                bytes.next().and_then(|b| (b as char).to_digit(16)),
            );
            let (Some(hi), Some(lo)) = digits else {
                return false;
            };
            decoded.push((hi * 16 + lo) as u8);
        } else {
            decoded.push(byte);
        }
    }
    !decoded.contains(&0) && std::str::from_utf8(&decoded).is_ok()
}

fn names(value: &Value) -> Result<Vec<&str>> {
    value
        .as_array()
        .context("semantic-token legend is missing")?
        .iter()
        .map(|name| {
            name.as_str()
                .filter(|name| !name.is_empty())
                .context("invalid semantic-token legend name")
        })
        .collect()
}

fn integer(value: &Value) -> Result<u32> {
    let number = value
        .as_u64()
        .context("semantic-token field is not unsigned")?;
    u32::try_from(number).context("semantic-token field exceeds u32")
}

fn boundary(text: &str, units: u32) -> Option<usize> {
    let mut consumed = 0u32;
    for (offset, ch) in text.char_indices() {
        if consumed == units {
            return Some(offset);
        }
        consumed += ch.len_utf16() as u32;
        if consumed > units {
            return None;
        }
    }
    (consumed == units).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_locations_match_exact_utf16_names_without_changing_symbol_identity() {
        let row = json!({"usr": "opaque", "name": "foo", "containerName": null,
            "declarationRange": {"uri": "file:///prototype.hpp", "range": {
                "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3},
            }},
            "definitionRange": {"uri": "file:///unit.cpp", "range": {
                "start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 5},
            }},
        });
        let info = SymbolInfo::parse(&json!([row])).unwrap().unwrap();
        let span = crate::schema::Span::from_ts(4, 7, (0, 4), (0, 7));
        assert!(
            info.definition
                .as_ref()
                .unwrap()
                .matches("file:///unit.cpp", "😀foo", &span)
        );
        assert!(
            !info
                .definition
                .as_ref()
                .unwrap()
                .matches("file:///other.cpp", "😀foo", &span)
        );
        assert!(
            !info
                .definition
                .as_ref()
                .unwrap()
                .matches("file:///unit.cpp", "foo", &span)
        );
        let mut no_definition = row;
        no_definition["definitionRange"] = Value::Null;
        let declaration = SymbolInfo::parse(&json!([no_definition])).unwrap().unwrap();
        assert!(declaration.definition.is_none());
        assert_eq!(info.identity, declaration.identity);
    }

    #[test]
    fn malformed_optional_definition_locations_and_names_fail_closed() {
        let declaration = json!({"uri": "file:///unit.hpp", "range": {
            "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3},
        }});
        for malformed in [
            json!({}),
            json!({"uri": "file://host/unit.cpp", "range": declaration["range"]}),
            json!({"uri": "file:///unit.cpp", "range": {"start": {"line": 0, "character": 3},
                "end": {"line": 0, "character": 2}}}),
        ] {
            assert!(
                SymbolInfo::parse(&json!([{"usr": "opaque", "declarationRange": declaration,
                "definitionRange": malformed}]))
                .is_err()
            );
        }
        for (field, value) in [
            ("name", json!(false)),
            ("name", json!("bad\0name")),
            ("containerName", json!([])),
            ("containerName", json!("bad\0scope")),
        ] {
            let mut row = json!({"usr": "opaque", "declarationRange": declaration});
            row[field] = value;
            assert!(SymbolInfo::parse(&json!([row])).is_err());
        }
    }
    use serde_json::json;

    #[test]
    fn explicitly_source_less_symbols_are_unresolved_but_malformed_locations_fail() {
        let row = json!({"usr": "builtin", "name": "builtin", "declarationRange": null, "definitionRange": null});
        assert!(SymbolInfo::parse(&json!([row])).unwrap().is_none());
        let malformed = json!({"usr": "builtin", "declarationRange": {"uri": "not-a-file"}});
        assert!(SymbolInfo::parse(&json!([malformed])).is_err());
        assert!(
            SymbolInfo::parse(&json!([{"usr": "builtin"}]))
                .unwrap()
                .is_none()
        );
    }

    fn capabilities() -> Value {
        json!({"semanticTokensProvider":{"legend":{
            "tokenTypes":["function","method","parameter","macro"],
            "tokenModifiers":["virtual","static","dependentName","constructorOrDestructor","abstract","classScope"]
        }}})
    }

    #[test]
    fn exact_callable_tokens_distinguish_dispatch_and_reject_noncallables() {
        let source = "free member static virtual dependent constructor pointer macro";
        let response = json!({"data":[0,0,4,0,0, 0,5,6,1,32, 0,7,6,0,34,
            0,7,7,1,1, 0,8,9,0,4, 0,10,11,1,8, 0,12,7,2,0, 0,8,5,3,0]});
        let parsed = SemanticTokens::parse(&capabilities(), &response, source).unwrap();
        assert_eq!(parsed.call_kind(0, 0, 4), Some(CallKind::Free));
        assert_eq!(parsed.call_kind(0, 5, 6), Some(CallKind::NonvirtualMember));
        assert_eq!(parsed.call_kind(0, 12, 6), Some(CallKind::Static));
        for (column, length) in [(19, 7), (27, 9), (37, 11), (49, 7), (57, 5), (1, 3), (0, 3)] {
            assert_eq!(parsed.call_kind(0, column, length), None);
        }
    }

    #[test]
    fn malformed_records_and_spans_fail_closed() {
        for data in [
            json!([0]),
            json!([0, 0, 0, 0, 0]),
            json!([0, -1, 1, 0, 0]),
            json!([0, 0, 1, 99, 0]),
            json!([0, 0, 1, 0, 64]),
            json!([1, 0, 1, 0, 0]),
            json!([0, 2, 1, 0, 0]),
            json!([0, 0, 1, 0, 0, 0, 0, 1, 0, 0]),
            json!([0, 0, 1, 0, 0, 4_294_967_295_u64, 0, 1, 0, 0, 1, 0, 1, 0, 0]),
            json!([0, 4_294_967_295_u64, 1, 0, 0]),
            json!([0, 0, 1.5, 0, 0]),
        ] {
            assert!(SemanticTokens::parse(&capabilities(), &json!({"data":data}), "x").is_err());
        }
        assert!(SemanticTokens::parse(&capabilities(), &Value::Null, "x").is_err());
    }

    #[test]
    fn utf16_tokens_require_character_boundaries_and_stay_on_one_line() {
        let source = "😀foo\r\nbar";
        let parsed = SemanticTokens::parse(
            &capabilities(),
            &json!({"data":[0,2,3,0,0,1,0,3,0,0]}),
            source,
        )
        .unwrap();
        assert_eq!(parsed.call_kind(0, 2, 3), Some(CallKind::Free));
        assert_eq!(parsed.call_kind(1, 0, 3), Some(CallKind::Free));
        for (start, length) in [(1, 1), (0, 1), (2, 7)] {
            assert!(
                SemanticTokens::parse(
                    &capabilities(),
                    &json!({"data":[0,start,length,0,0]}),
                    source
                )
                .is_err()
            );
        }
    }

    #[test]
    fn absent_or_unusable_dispatch_legends_cannot_establish_nonvirtuality() {
        for modifiers in [
            json!([]),
            json!(["virtual", "static"]),
            json!([
                "virtual",
                "static",
                "dependentName",
                "constructorOrDestructor",
                "virtual"
            ]),
        ] {
            let mut caps = capabilities();
            caps["semanticTokensProvider"]["legend"]["tokenModifiers"] = modifiers;
            assert!(SemanticTokens::parse(&caps, &json!({"data":[]}), "").is_err());
        }
        let parsed =
            SemanticTokens::parse(&capabilities(), &json!({"data":[]}), "missing").unwrap();
        assert_eq!(parsed.call_kind(0, 0, 7), None);
    }
    #[test]
    fn symbol_identity_requires_unique_usr_and_exact_declaration_range() {
        let row = json!({"usr":"c:@F@pick#I#", "name":"pick", "declarationRange":{
            "uri":"file:///source/include/ops.hpp", "range":{
                "start":{"line":1,"character":4}, "end":{"line":1,"character":8}
            }
        }});
        let identity = SymbolIdentity::parse(&json!([row])).unwrap().unwrap();
        assert_eq!(
            SymbolIdentity::parse(&json!([row])).unwrap().unwrap(),
            identity
        );
        for (field, value) in [
            ("usr", json!("c:@F@pick#I#I#")),
            (
                "declarationRange",
                json!({
                    "uri":"file:///source/include/decoy.hpp", "range":{
                        "start":{"line":1,"character":4}, "end":{"line":1,"character":8}
                    }
                }),
            ),
        ] {
            let mut changed = row.clone();
            changed[field] = value;
            assert_ne!(
                SymbolIdentity::parse(&json!([changed])).unwrap().unwrap(),
                identity
            );
        }
        assert!(SymbolIdentity::parse(&json!([])).unwrap().is_none());
        assert!(SymbolIdentity::parse(&json!([row, row])).unwrap().is_none());
        assert!(SymbolIdentity::parse(&Value::Null).is_err());
        for invalid in [json!({}), json!({"usr":""})] {
            assert!(SymbolIdentity::parse(&json!([invalid])).is_err());
        }
        for uri in [
            "file://",
            "file:///",
            "file://host/source.hpp",
            "file:///bad%",
            "file:///bad%XZ",
            "file:///bad%FF",
            "file:///bad%00",
            "file:///bad\0",
            "file:///bad?query",
            "file:///bad#fragment",
        ] {
            let mut malformed = row.clone();
            malformed["declarationRange"]["uri"] = json!(uri);
            assert!(
                SymbolIdentity::parse(&json!([malformed])).is_err(),
                "{uri:?}"
            );
        }
        let mut reversed = row;
        reversed["declarationRange"]["range"]["end"]["character"] = json!(3);
        assert!(SymbolIdentity::parse(&json!([reversed])).is_err());
    }
}
