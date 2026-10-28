//! Exact source-backed body and direct-call admission for compiler evidence.

use crate::cpp_evidence::{CallKind, SemanticTokens, SymbolInfo};
use crate::cpp_ownership::{OwnerSource, container_matches, exact_token, recover_qualified_body};
use crate::cpp_syntax::{BodyAnchor, CallAnchor};
use crate::lsp::byte_offset_to_lsp_pos;
use crate::rust_syntax::RawDef;
use crate::schema::{NodeKind, Span};

pub struct CompiledBody {
    pub uri: String,
    pub definition: RawDef,
    pub name_span: Span,
    pub info: SymbolInfo,
    pub target_kind: Option<CallKind>,
}

impl CompiledBody {
    pub fn admit(
        source: &OwnerSource<'_>,
        anchor: &BodyAnchor,
        tokens: &SemanticTokens,
        info: SymbolInfo,
        qualifiers: &[SymbolInfo],
        owners: &[OwnerSource<'_>],
    ) -> Option<Self> {
        if source.raw.parse_errors != 0 || anchor.templated {
            return None;
        }
        let token = exact_token(tokens, source.text, &anchor.name_span)?;
        if !token.modifiers.contains("definition")
            || token.modifiers.contains("constructorOrDestructor")
            || !info
                .definition
                .as_ref()?
                .matches(source.uri, source.text, &anchor.name_span)
            || info.name.as_deref()?
                != source
                    .text
                    .get(anchor.name_span.start_byte..anchor.name_span.end_byte)?
        {
            return None;
        }
        let definition = if anchor.qualifiers.is_empty() {
            if !qualifiers.is_empty() || !container_matches(&info, &anchor.lexical_scope) {
                return None;
            }
            let kind = match token.kind.as_str() {
                "function" if !token.modifiers.contains("classScope") => NodeKind::Function,
                "method" if token.modifiers.contains("classScope") => NodeKind::Method,
                "function"
                    if token.modifiers.contains("classScope")
                        && token.modifiers.contains("static") =>
                {
                    NodeKind::Method
                }
                _ => return None,
            };
            RawDef {
                kind,
                name: anchor.normalized_name.clone(),
                scope: anchor.lexical_scope.clone(),
                span: anchor.span,
                parent: anchor.parent,
            }
        } else {
            recover_qualified_body(
                source.uri,
                source.text,
                anchor,
                tokens,
                &info,
                qualifiers,
                owners,
            )?
        };
        let (line, character) =
            byte_offset_to_lsp_pos(source.text.as_bytes(), anchor.name_span.start_byte);
        let target_kind = tokens.call_kind(line, character, token.length);
        Some(Self {
            uri: source.uri.to_string(),
            definition,
            name_span: anchor.name_span,
            info,
            target_kind,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AdmittedCall {
    pub caller: usize,
    pub target: usize,
    pub span: Span,
}

pub fn admit_call(
    source: &OwnerSource<'_>,
    anchor: &CallAnchor,
    tokens: &SemanticTokens,
    info: &SymbolInfo,
    bodies: &[CompiledBody],
) -> Option<AdmittedCall> {
    if source.raw.parse_errors != 0
        || anchor.templated
        || anchor.inside_lambda
        || anchor.unevaluated
        || !matches!(
            anchor.function_kind.as_str(),
            "identifier" | "qualified_identifier" | "field_expression"
        )
        || !matches!(anchor.name_kind.as_str(), "identifier" | "field_identifier")
        || info.name.as_deref()?
            != source
                .text
                .get(anchor.name_span.start_byte..anchor.name_span.end_byte)?
    {
        return None;
    }
    let token = exact_token(tokens, source.text, &anchor.name_span)?;
    let (line, character) =
        byte_offset_to_lsp_pos(source.text.as_bytes(), anchor.name_span.start_byte);
    let kind = tokens.call_kind(line, character, token.length)?;
    let caller_span = anchor.caller_span?;
    if caller_span.start_byte > anchor.span.start_byte
        || caller_span.end_byte < anchor.span.end_byte
    {
        return None;
    }
    let caller = unique_index(bodies, |body| {
        body.uri == source.uri && body.definition.span == caller_span
    })?;
    if unique_index(bodies, |body| {
        body.info.identity == bodies[caller].info.identity
    }) != Some(caller)
    {
        return None;
    }
    let target = unique_index(bodies, |body| body.info.identity == info.identity)?;
    if bodies[target].target_kind != Some(kind)
        || (info.definition.is_some() && info.definition != bodies[target].info.definition)
        || info.container != bodies[target].info.container
    {
        return None;
    }
    Some(AdmittedCall {
        caller,
        target,
        span: anchor.span,
    })
}

fn unique_index(bodies: &[CompiledBody], matches: impl Fn(&CompiledBody) -> bool) -> Option<usize> {
    let mut selected = bodies
        .iter()
        .enumerate()
        .filter(|(_, body)| matches(body))
        .map(|(index, _)| index);
    let only = selected.next()?;
    selected.next().is_none().then_some(only)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp_syntax::{ScopeIndex, extract_file};
    use serde_json::{Value, json};

    const URI: &str = "file:///source/unit.cpp";
    const SOURCE: &str = "int target(int n) { return n; }\nint caller() { return target(1); }\n";

    fn capabilities() -> Value {
        json!({"semanticTokensProvider": {"legend": {
            "tokenTypes": ["function", "method", "variable"],
            "tokenModifiers": ["virtual", "static", "dependentName", "constructorOrDestructor", "abstract", "classScope", "definition"],
        }}})
    }

    fn tokens(data: &Value) -> SemanticTokens {
        SemanticTokens::parse(&capabilities(), &json!({"data": data}), SOURCE).unwrap()
    }

    fn info(name: &str, line: u32, declaration_uri: &str) -> SymbolInfo {
        let location = |uri| {
            json!({"uri": uri, "range": {
                "start": {"line": line, "character": 4}, "end": {"line": line, "character": 10},
            }})
        };
        SymbolInfo::parse(&json!([{"usr": name, "name": name,
            "declarationRange": location(declaration_uri), "definitionRange": location(URI),
        }]))
        .unwrap()
        .unwrap()
    }

    fn bodies(source: &OwnerSource<'_>, tokens: &SemanticTokens) -> Vec<CompiledBody> {
        source
            .raw
            .bodies
            .iter()
            .zip([("target", 0), ("caller", 1)])
            .map(|(anchor, (name, line))| {
                CompiledBody::admit(
                    source,
                    anchor,
                    tokens,
                    info(name, line, "file:///source/api.hpp"),
                    &[],
                    &[],
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn exact_caller_and_prototype_identity_admit_only_the_labelled_edge() {
        let raw = extract_file("unit.cpp", SOURCE.as_bytes(), &ScopeIndex::default()).unwrap();
        let source = OwnerSource {
            uri: URI,
            text: SOURCE,
            raw: &raw,
        };
        let tokens = tokens(&json!([0, 4, 6, 0, 64, 1, 4, 6, 0, 64, 0, 18, 6, 0, 0]));
        let bodies = bodies(&source, &tokens);
        let call_info = info("target", 0, "file:///source/api.hpp");
        let edges = raw
            .call_anchors
            .iter()
            .filter_map(|anchor| admit_call(&source, anchor, &tokens, &call_info, &bodies))
            .collect::<Vec<_>>();
        assert_eq!(
            edges,
            [AdmittedCall {
                caller: 1,
                target: 0,
                span: raw.calls[0].span
            }]
        );
        let mut prototype_only = info("target", 0, "file:///source/api.hpp");
        prototype_only.definition = None;
        assert_eq!(
            admit_call(
                &source,
                &raw.call_anchors[0],
                &tokens,
                &prototype_only,
                &bodies
            ),
            Some(AdmittedCall {
                caller: 1,
                target: 0,
                span: raw.calls[0].span
            })
        );
    }

    #[test]
    fn missing_or_ambiguous_identity_and_unsupported_dispatch_admit_no_edges() {
        let raw = extract_file("unit.cpp", SOURCE.as_bytes(), &ScopeIndex::default()).unwrap();
        let source = OwnerSource {
            uri: URI,
            text: SOURCE,
            raw: &raw,
        };
        let active = tokens(&json!([0, 4, 6, 0, 64, 1, 4, 6, 0, 64, 0, 18, 6, 0, 0]));
        let mut bodies = bodies(&source, &active);
        let valid = info("target", 0, "file:///source/api.hpp");
        let mut wrong = info("target", 0, "file:///source/decoy.hpp");
        assert!(admit_call(&source, &raw.call_anchors[0], &active, &wrong, &bodies).is_none());
        wrong = info("target", 0, "file:///source/api.hpp");
        wrong.definition = info("caller", 1, URI).definition;
        assert!(admit_call(&source, &raw.call_anchors[0], &active, &wrong, &bodies).is_none());
        for flags in [1, 4, 8, 16] {
            let unsupported = tokens(&json!([0, 4, 6, 0, 64, 1, 4, 6, 0, 64, 0, 18, 6, 0, flags]));
            assert!(
                admit_call(&source, &raw.call_anchors[0], &unsupported, &valid, &bodies).is_none()
            );
        }
        let pointer = tokens(&json!([0, 4, 6, 0, 64, 1, 4, 6, 0, 64, 0, 18, 6, 2, 0]));
        assert!(admit_call(&source, &raw.call_anchors[0], &pointer, &valid, &bodies).is_none());
        for flag in [0, 1, 2] {
            let mut anchor = raw.call_anchors[0].clone();
            match flag {
                0 => anchor.templated = true,
                1 => anchor.inside_lambda = true,
                _ => anchor.caller_span = None,
            }
            assert!(admit_call(&source, &anchor, &active, &valid, &bodies).is_none());
        }
        bodies.push(
            CompiledBody::admit(
                &source,
                &raw.bodies[0],
                &active,
                info("target", 0, "file:///source/api.hpp"),
                &[],
                &[],
            )
            .unwrap(),
        );
        assert!(admit_call(&source, &raw.call_anchors[0], &active, &valid, &bodies).is_none());
    }

    #[test]
    fn body_activity_definition_scope_and_template_evidence_are_required() {
        let raw = extract_file("unit.cpp", SOURCE.as_bytes(), &ScopeIndex::default()).unwrap();
        let source = OwnerSource {
            uri: URI,
            text: SOURCE,
            raw: &raw,
        };
        for data in [json!([]), json!([0, 4, 6, 0, 0]), json!([0, 4, 6, 0, 68])] {
            let tokens = tokens(&data);
            assert!(
                CompiledBody::admit(
                    &source,
                    &raw.bodies[0],
                    &tokens,
                    info("target", 0, URI),
                    &[],
                    &[]
                )
                .is_none()
            );
        }
        let active = tokens(&json!([0, 4, 6, 0, 64]));
        let mut missing = info("target", 0, URI);
        missing.definition = None;
        assert!(CompiledBody::admit(&source, &raw.bodies[0], &active, missing, &[], &[]).is_none());
        let mut scoped = info("target", 0, URI);
        scoped.container = Some("other::".into());
        assert!(CompiledBody::admit(&source, &raw.bodies[0], &active, scoped, &[], &[]).is_none());
        let mut template = raw.bodies[0].clone();
        template.templated = true;
        assert!(
            CompiledBody::admit(
                &source,
                &template,
                &active,
                info("target", 0, URI),
                &[],
                &[]
            )
            .is_none()
        );
    }

    #[test]
    fn exact_ast_caller_span_prevents_outer_function_fallback() {
        let text = b"int f() { struct Local { int nested() { return target(); } }; auto l = [] { return target(); }; return target(); }\nint initialized = target();\n";
        let raw = extract_file("unit.cpp", text, &ScopeIndex::default()).unwrap();
        assert_eq!(raw.call_anchors.len(), 4);
        let nested = raw.call_anchors[0].caller_span.unwrap();
        let outer = raw.call_anchors[2].caller_span.unwrap();
        assert_ne!(nested, outer);
        assert_eq!(
            &text[nested.start_byte..nested.end_byte],
            b"int nested() { return target(); }"
        );
        assert!(raw.call_anchors[1].inside_lambda);
        assert_eq!(raw.call_anchors[1].caller_span, Some(outer));
        assert!(raw.call_anchors[3].caller_span.is_none());
    }

    #[test]
    fn static_linkage_free_functions_do_not_become_static_members() {
        let source = "local member";
        let tokens = SemanticTokens::parse(
            &capabilities(),
            &json!({"data": [0,0,5,0,2, 0,6,6,0,34]}),
            source,
        )
        .unwrap();
        assert_eq!(tokens.call_kind(0, 0, 5), Some(CallKind::Free));
        assert_eq!(tokens.call_kind(0, 6, 6), Some(CallKind::Static));
    }
    #[test]
    fn anonymous_source_scope_is_retained_when_compiler_container_omits_it() {
        let text = "namespace {\nint caller() { return 1; }\n}\n";
        let raw = extract_file("unit.cpp", text.as_bytes(), &ScopeIndex::default()).unwrap();
        let source = OwnerSource {
            uri: URI,
            text,
            raw: &raw,
        };
        let tokens =
            SemanticTokens::parse(&capabilities(), &json!({"data": [1,4,6,0,64]}), text).unwrap();
        let body = CompiledBody::admit(
            &source,
            &raw.bodies[0],
            &tokens,
            info("caller", 1, URI),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(body.definition.scope, raw.bodies[0].lexical_scope);
        assert!(body.definition.scope[0].starts_with("anonymous@"));
        let mut wrong = info("caller", 1, URI);
        wrong.container = Some("named_decoy::".into());
        assert!(CompiledBody::admit(&source, &raw.bodies[0], &tokens, wrong, &[], &[]).is_none());
    }
    #[test]
    fn type_initializers_and_unevaluated_references_do_not_acquire_callers() {
        let source = b"void f() { struct Local { int value = target(); }; (void)sizeof(target()); using T = decltype(target()); bool n = noexcept(target()); bool r = requires { target(); }; (void)typeid(target()); }";
        let raw = extract_file("unit.cpp", source, &ScopeIndex::default()).unwrap();
        assert_eq!(raw.call_anchors.len(), 8);
        assert!(raw.call_anchors[0].caller_span.is_none());
        assert!(raw.call_anchors.iter().skip(1).all(|call| call.unevaluated));
    }
}
