//! Compiled ownership admission from exact parser and compiler witnesses.

use crate::cpp_evidence::{SemanticTokens, SymbolInfo, Token};
use crate::cpp_syntax::{BodyAnchor, RawFile};
use crate::lsp::byte_offset_to_lsp_pos;
use crate::rust_syntax::RawDef;
use crate::schema::{NodeKind, Span, TypeKind};

pub struct OwnerSource<'a> {
    pub uri: &'a str,
    pub text: &'a str,
    pub raw: &'a RawFile,
}

pub fn recover_qualified_body(
    uri: &str,
    source: &str,
    body: &BodyAnchor,
    tokens: &SemanticTokens,
    info: &SymbolInfo,
    qualifier_infos: &[SymbolInfo],
    owners: &[OwnerSource<'_>],
) -> Option<RawDef> {
    if body.templated
        || body.qualifiers.is_empty()
        || body.qualifiers.len() != qualifier_infos.len()
    {
        return None;
    }
    let body_token = exact_token(tokens, source, &body.name_span)?;
    if !body_token.modifiers.contains("definition")
        || !info
            .definition
            .as_ref()?
            .matches(uri, source, &body.name_span)
        || info.name.as_deref()?
            != source.get(body.name_span.start_byte..body.name_span.end_byte)?
    {
        return None;
    }
    let mut previous_path = Vec::new();
    let mut owner_kind = NodeKind::Module;
    for (qualifier, info) in body.qualifiers.iter().zip(qualifier_infos) {
        let token = exact_token(tokens, source, qualifier)?;
        let expected = match token.kind.as_str() {
            "class" => "type",
            "namespace" => "namespace",
            _ => return None,
        };
        let name = source.get(qualifier.start_byte..qualifier.end_byte)?;
        if info.name.as_deref()? != name {
            return None;
        }
        let owner = bind_owner(info, expected, owners)?;
        let mut path = owner.scope.clone();
        path.push(owner.name.clone());
        if owner.name != name
            || !path.starts_with(&previous_path)
            || path.len() <= previous_path.len()
        {
            return None;
        }
        previous_path = path;
        owner_kind = owner.kind;
    }
    if !previous_path.starts_with(&body.lexical_scope) || !container_matches(info, &previous_path) {
        return None;
    }
    let kind = match owner_kind {
        NodeKind::Module
            if body_token.kind == "function" && !body_token.modifiers.contains("classScope") =>
        {
            NodeKind::Function
        }
        NodeKind::Type(_)
            if body_token.modifiers.contains("classScope")
                && (body_token.kind == "method"
                    || (body_token.kind == "function"
                        && body_token.modifiers.contains("static"))) =>
        {
            NodeKind::Method
        }
        _ => return None,
    };
    Some(RawDef {
        kind,
        name: body.normalized_name.clone(),
        scope: previous_path,
        span: body.span,
        parent: body.parent,
    })
}

fn bind_owner<'a>(
    info: &SymbolInfo,
    expected: &str,
    sources: &'a [OwnerSource<'_>],
) -> Option<&'a RawDef> {
    if expected == "type" && info.definition.is_none() {
        return None;
    }
    let mut matches = sources.iter().flat_map(|source| {
        source.raw.owners.iter().filter_map(move |anchor| {
            let def = source.raw.defs.get(anchor.definition)?;
            let kind_matches = match &def.kind {
                NodeKind::Module => expected == "namespace",
                NodeKind::Type(TypeKind::Class | TypeKind::Struct | TypeKind::Union) => {
                    expected == "type"
                }
                _ => false,
            };
            (kind_matches
                && !anchor.templated
                && source.raw.parse_errors == 0
                && info
                    .owner_location()
                    .matches(source.uri, source.text, &anchor.name_span)
                && info.name.as_deref() == Some(def.name.as_str())
                && container_matches(info, &def.scope))
            .then_some(def)
        })
    });
    let only = matches.next()?;
    matches.next().is_none().then_some(only)
}

pub fn exact_token<'a>(tokens: &'a SemanticTokens, source: &str, span: &Span) -> Option<&'a Token> {
    let text = source.get(span.start_byte..span.end_byte)?;
    let (line, character) = byte_offset_to_lsp_pos(source.as_bytes(), span.start_byte);
    let token = tokens.exact_token(
        line,
        character,
        u32::try_from(text.encode_utf16().count()).ok()?,
    )?;
    (!token.modifiers.contains("dependentName") && token.kind != "macro").then_some(token)
}

pub fn container_matches(info: &SymbolInfo, scope: &[String]) -> bool {
    let named = scope
        .iter()
        .filter(|name| !name.starts_with("anonymous@"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    let expected = if named.is_empty() {
        String::new()
    } else {
        format!("{}::", named.join("::"))
    };
    info.container.as_deref().unwrap_or_default() == expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp_syntax::{ScopeIndex, extract_file};
    use serde_json::{Value, json};

    const HEADER: &str =
        "namespace api {\nstruct Widget;\nstruct Widget { int run(int value) const; };\n}\n";
    const SOURCE: &str = "namespace api {\nint Widget::run(int value) const { return value; }\n}\n";
    const HEADER_URI: &str = "file:///source/owners.hpp";
    const SOURCE_URI: &str = "file:///source/unit.cpp";

    fn location(uri: &str, line: u32, character: u32, length: u32) -> Value {
        json!({"uri": uri, "range": {
            "start": {"line": line, "character": character},
            "end": {"line": line, "character": character + length},
        }})
    }

    fn owner_info() -> Value {
        json!([{"usr": "opaque-class-identity", "name": "Widget", "containerName": "api::",
            "declarationRange": location(HEADER_URI, 1, 7, 6),
            "definitionRange": location(HEADER_URI, 2, 7, 6),
        }])
    }

    fn body_info() -> Value {
        json!([{"usr": "opaque-method-identity", "name": "run", "containerName": "api::Widget::",
            "declarationRange": location(HEADER_URI, 2, 20, 3),
            "definitionRange": location(SOURCE_URI, 1, 12, 3),
        }])
    }

    fn tokens(data: &Value) -> SemanticTokens {
        let capabilities = json!({"semanticTokensProvider": {"legend": {
            "tokenTypes": ["class", "method", "function", "namespace", "macro"],
            "tokenModifiers": ["virtual", "static", "dependentName", "constructorOrDestructor", "definition", "classScope"],
        }}});
        SemanticTokens::parse(&capabilities, &json!({"data": data}), SOURCE).expect("tokens")
    }

    fn parsed(source: &str) -> RawFile {
        extract_file("unit.cpp", source.as_bytes(), &ScopeIndex::default()).expect("parse")
    }

    fn recovered(
        header: &str,
        owner: &Value,
        body: &Value,
        tokens: &SemanticTokens,
    ) -> Option<RawDef> {
        let raw_header = parsed(header);
        let raw_source = parsed(SOURCE);
        let owner = SymbolInfo::parse(owner).expect("owner response")?;
        let body = SymbolInfo::parse(body).expect("body response")?;
        recover_qualified_body(
            SOURCE_URI,
            SOURCE,
            &raw_source.bodies[0],
            tokens,
            &body,
            &[owner],
            &[OwnerSource {
                uri: HEADER_URI,
                text: header,
                raw: &raw_header,
            }],
        )
    }

    #[test]
    fn guarded_owner_body_recovers_signature_scope_and_lexical_parent() {
        let recovered = recovered(
            HEADER,
            &owner_info(),
            &body_info(),
            &tokens(&json!([1, 4, 6, 0, 0, 0, 8, 3, 1, 48])),
        )
        .expect("recovered body");
        assert_eq!(recovered.kind, NodeKind::Method);
        assert_eq!(recovered.name, "run(int) const");
        assert_eq!(recovered.scope, ["api", "Widget"]);
        let raw = parsed(SOURCE);
        assert_eq!(recovered.span, raw.bodies[0].span);
        assert_eq!(recovered.parent, Some(0));
        assert_eq!(raw.defs[0].kind, NodeKind::Module);
        assert!(!raw.defs.iter().any(|def| def.kind == NodeKind::Method));
    }

    #[test]
    fn forward_declarations_member_names_and_wrong_scope_do_not_bind_owners() {
        let positive = tokens(&json!([1, 4, 6, 0, 0, 0, 8, 3, 1, 48]));
        for location in [
            Value::Null,
            location(HEADER_URI, 1, 7, 6),
            location(HEADER_URI, 2, 20, 3),
            location("file:///other/owners.hpp", 2, 7, 6),
        ] {
            let mut owner = owner_info();
            owner[0]["definitionRange"] = location;
            assert!(recovered(HEADER, &owner, &body_info(), &positive).is_none());
        }
        for (field, value) in [
            ("name", json!("Alias")),
            ("containerName", json!("wrong::")),
        ] {
            let mut owner = owner_info();
            owner[0][field] = value;
            assert!(recovered(HEADER, &owner, &body_info(), &positive).is_none());
        }
        let owner = json!([owner_info()[0].clone(), owner_info()[0].clone()]);
        assert!(recovered(HEADER, &owner, &body_info(), &positive).is_none());
    }

    #[test]
    fn inactive_macro_dependent_and_wrong_kind_body_tokens_do_not_recover() {
        for data in [
            json!([]),
            json!([1, 4, 6, 0, 0]),
            json!([1, 4, 6, 4, 0, 0, 8, 3, 1, 48]),
            json!([1, 4, 6, 0, 4, 0, 8, 3, 1, 48]),
            json!([1, 4, 6, 0, 0, 0, 8, 3, 1, 32]),
            json!([1, 4, 6, 0, 0, 0, 8, 3, 2, 16]),
        ] {
            assert!(recovered(HEADER, &owner_info(), &body_info(), &tokens(&data)).is_none());
        }
        for (field, value) in [
            ("definitionRange", Value::Null),
            ("definitionRange", location(SOURCE_URI, 1, 13, 2)),
            ("name", json!("other")),
            ("containerName", json!("wrong::Widget::")),
        ] {
            let mut body = body_info();
            body[0][field] = value;
            assert!(
                recovered(
                    HEADER,
                    &owner_info(),
                    &body,
                    &tokens(&json!([1, 4, 6, 0, 0, 0, 8, 3, 1, 48]))
                )
                .is_none()
            );
        }
    }

    #[test]
    fn templates_and_duplicate_source_owners_remain_unproven() {
        let positive = tokens(&json!([1, 4, 6, 0, 0, 0, 8, 3, 1, 48]));
        let header = "namespace api {\ntemplate<typename T> struct Widget { int run(int value) const; };\n}\n";
        let mut owner = owner_info();
        owner[0]["definitionRange"] = location(HEADER_URI, 1, 28, 6);
        assert!(recovered(header, &owner, &body_info(), &positive).is_none());
        let raw_header = parsed(HEADER);
        let raw_source = parsed(SOURCE);
        let owner = SymbolInfo::parse(&owner_info()).unwrap().unwrap();
        let body = SymbolInfo::parse(&body_info()).unwrap().unwrap();
        let sources = [
            OwnerSource {
                uri: HEADER_URI,
                text: HEADER,
                raw: &raw_header,
            },
            OwnerSource {
                uri: HEADER_URI,
                text: HEADER,
                raw: &raw_header,
            },
        ];
        assert!(
            recover_qualified_body(
                SOURCE_URI,
                SOURCE,
                &raw_source.bodies[0],
                &positive,
                &body,
                &[owner],
                &sources
            )
            .is_none()
        );
    }
}
