//! Executable U1 parser-isolation probes. These do not admit canonical calls
//! or establish the production driver's full input closure.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::cpp_evidence::{CallKind, SemanticTokens, SymbolIdentity, SymbolInfo};
use crate::cpp_inputs::{CapturedInputs, InputManifest};
use crate::cpp_ownership::{OwnerSource, recover_qualified_body};
use crate::cpp_profile::ProfileRoots;
use crate::cpp_syntax;
use crate::lsp::{self, DefinitionResult, Transport};
use crate::schema::sha256_hex;

struct Workspace {
    root: tempfile::TempDir,
    source: PathBuf,
    captured: BTreeMap<String, String>,
    inputs: CapturedInputs,
}

impl Workspace {
    fn capture(files: &[(&str, &str)], units: &[&str], links: &[(&str, &str)]) -> Self {
        let root = tempfile::Builder::new()
            .prefix("codemap cpp é ")
            .tempdir()
            .expect("isolated workspace");
        let absolute = fs::canonicalize(root.path()).expect("absolute workspace");
        let source = absolute.join("source");
        let mut captured = BTreeMap::new();
        for (relative, content) in files {
            write_file(&source.join(relative), content);
            captured.insert((*relative).to_string(), (*content).to_string());
        }
        for (alias, target) in links {
            fs::hard_link(source.join(target), source.join(alias)).expect("file alias");
            captured.insert((*alias).to_string(), captured[*target].clone());
        }
        let manifest = InputManifest {
            files: captured
                .iter()
                .map(|(relative, content)| {
                    (
                        format!("{{source}}/{relative}"),
                        sha256_hex(content.as_bytes()),
                    )
                })
                .collect(),
        };
        let inputs = CapturedInputs::capture(
            &manifest,
            &ProfileRoots {
                source: source.clone(),
                build: None,
                sysroot: None,
                dependencies: Vec::new(),
            },
        )
        .expect("capture manifested inputs");
        let overlay = inputs.overlay_path();
        let driver = absolute.join("driver/clang++");
        write_file(&driver, "#!/bin/sh\nexit 93\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&driver, fs::Permissions::from_mode(0o644)).expect("inert driver");
        }
        let database = units.iter().map(|unit| json!({
            "directory": source,
            "file": source.join(unit),
            "arguments": [driver, "-std=c++20", "-target", "aarch64-unknown-linux-gnu",
                "-isysroot", source, "-resource-dir", source, "-nostdinc", "-nostdinc++",
                "-ivfsoverlay", overlay, "-I", source.join("include"), "-c", source.join(unit)],
        })).collect::<Vec<_>>();
        write_file(
            &absolute.join("compile_commands.json"),
            &json!(database).to_string(),
        );
        Self {
            root,
            source,
            captured,
            inputs,
        }
    }

    fn spawn(&self) -> Transport {
        self.spawn_with_capabilities().0
    }

    fn body_anchors(&self, relative: &str) -> Vec<cpp_syntax::BodyAnchor> {
        self.parsed_files()
            .remove(relative)
            .expect("parsed file")
            .bodies
    }

    fn parsed_files(&self) -> BTreeMap<String, cpp_syntax::RawFile> {
        let files = self
            .captured
            .iter()
            .map(|(path, source)| (path.clone(), source.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        let index = cpp_syntax::ScopeIndex::build(&files).expect("spelled scope index");
        files
            .iter()
            .map(|(relative, source)| {
                let raw =
                    cpp_syntax::extract_file(relative, source, &index).expect("source anchors");
                assert_eq!(raw.parse_errors, 0);
                (relative.clone(), raw)
            })
            .collect()
    }

    fn recovered_body(
        &self,
        transport: &Transport,
        relative: &str,
        body: &cpp_syntax::BodyAnchor,
        tokens: &SemanticTokens,
    ) -> Option<crate::rust_syntax::RawDef> {
        let files = self.parsed_files();
        let uris = files
            .keys()
            .map(|path| (path.clone(), lsp::uri_from_path(&self.source.join(path))))
            .collect::<BTreeMap<_, _>>();
        let owners = files
            .iter()
            .map(|(path, raw)| OwnerSource {
                uri: &uris[path],
                text: &self.captured[path],
                raw,
            })
            .collect::<Vec<_>>();
        let symbol = |span: &crate::schema::Span| {
            let (line, character) =
                lsp::byte_offset_to_lsp_pos(self.captured[relative].as_bytes(), span.start_byte);
            let response = transport
                .request(
                    "textDocument/symbolInfo",
                    &json!({
                        "textDocument": {"uri": uris[relative]},
                        "position": {"line": line, "character": character},
                    }),
                )
                .expect("compiled body witness");
            SymbolInfo::parse(&response).expect("validated body witness")
        };
        let info = symbol(&body.name_span)?;
        let qualifiers = body
            .qualifiers
            .iter()
            .map(symbol)
            .collect::<Option<Vec<_>>>()?;
        recover_qualified_body(
            &uris[relative],
            &self.captured[relative],
            body,
            tokens,
            &info,
            &qualifiers,
            &owners,
        )
    }

    fn spawn_with_capabilities(&self) -> (Transport, Value) {
        let binary =
            PathBuf::from(std::env::var("CODEMAP_CPP_CLANGD").expect("CODEMAP_CPP_CLANGD"));
        let digest = std::env::var("CODEMAP_CPP_CLANGD_SHA256").expect("CODEMAP_CPP_CLANGD_SHA256");
        assert!(binary.is_absolute(), "the pinned binary must be absolute");
        assert_eq!(
            sha256_hex(&fs::read(&binary).expect("read clangd")),
            digest,
            "clangd pin drift"
        );
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("HOME", self.root.path())
            .env("TMPDIR", self.root.path())
            .current_dir(self.root.path())
            .stderr(Stdio::null())
            .args([
                "--enable-config=false",
                "--background-index=false",
                "--clang-tidy=false",
                "--pch-storage=memory",
                "--skip-preamble-build",
                "--log=error",
            ])
            .arg(format!(
                "--compile-commands-dir={}",
                self.root.path().display()
            ));
        let transport =
            Transport::spawn("clangd-isolation-probe", &mut command, None).expect("spawn clangd");
        let initialized = transport
            .request(
                "initialize",
                &json!({
                    "processId": null, "rootUri": lsp::uri_from_path(&self.source),
                    "capabilities": {"textDocument": {"definition": {"linkSupport": true}}},
                }),
            )
            .expect("initialize");
        transport
            .notify("initialized", &json!({}))
            .expect("initialized");
        (transport, initialized["capabilities"].clone())
    }

    fn open(&self, transport: &Transport, relative: &str) -> Vec<Value> {
        let uri = lsp::uri_from_path(&self.source.join(relative));
        transport
            .notify(
                "textDocument/didOpen",
                &json!({"textDocument": {
                    "uri": uri, "languageId": "cpp", "version": 1,
                    "text": String::from_utf8(self.inputs.read(&format!("{{source}}/{relative}"))
                        .expect("captured document")).expect("UTF-8 document"),
                }}),
            )
            .expect("open captured document");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let message = transport
                .unsolicited()
                .expect("messages")
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("diagnostics deadline");
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
            {
                return message["params"]["diagnostics"]
                    .as_array()
                    .expect("diagnostics array")
                    .clone();
            }
        }
    }

    fn definition(
        &self,
        transport: &Transport,
        relative: &str,
        marker: &str,
    ) -> (String, u32, u32) {
        let (line, character) = marked_position(&self.captured[relative], marker);
        let response = transport
            .request(
                "textDocument/definition",
                &json!({
                    "textDocument": {"uri": lsp::uri_from_path(&self.source.join(relative))},
                    "position": {"line": line, "character": character},
                }),
            )
            .expect("definition");
        let DefinitionResult::Locations(locations) =
            lsp::parse_definition_response("clangd", &response).expect("parse definition")
        else {
            panic!("missing definition: {response}")
        };
        assert_eq!(locations.len(), 1, "unique definition: {marker}");
        let location = &locations[0];
        (
            location.uri.clone(),
            location.start_line,
            location.start_character,
        )
    }
}

fn write_file(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write fixture");
}

fn marked_position(source: &str, marker: &str) -> (u32, u32) {
    let (line, text) = source
        .lines()
        .enumerate()
        .find(|(_, text)| text.trim_end().ends_with(marker))
        .expect("independent site label");
    let end = text.find('(').expect("call or callable");
    let start = text[..end]
        .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .map_or(0, |index| index + 1);
    lsp::byte_offset_to_lsp_pos(
        source.as_bytes(),
        source
            .lines()
            .take(line)
            .map(|text| text.len() + 1)
            .sum::<usize>()
            + start,
    )
}

fn errors(diagnostics: &[Value]) -> Vec<&Value> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic["severity"] == 1)
        .collect()
}

fn call_fixture() -> Workspace {
    let files = [
        (
            "include/ops.hpp",
            include_str!("../tests/fixtures/cpp_calls/include/ops.hpp"),
        ),
        (
            "src/ops.cpp",
            include_str!("../tests/fixtures/cpp_calls/src/ops.cpp"),
        ),
        (
            "src/caller.cpp",
            include_str!("../tests/fixtures/cpp_calls/src/caller.cpp"),
        ),
        (
            "src/contexts.cpp",
            include_str!("../tests/fixtures/cpp_calls/src/contexts.cpp"),
        ),
        (
            "include/typeinfo",
            include_str!("../tests/fixtures/cpp_calls/include/typeinfo"),
        ),
    ];
    Workspace::capture(
        &files,
        &["src/ops.cpp", "src/caller.cpp", "src/contexts.cpp"],
        &[],
    )
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn manifested_fixture_preserves_all_labelled_body_locations() {
    let workspace = call_fixture();
    let transport = workspace.spawn();
    for relative in ["src/ops.cpp", "src/caller.cpp"] {
        assert!(
            errors(&workspace.open(&transport, relative)).is_empty(),
            "{relative}"
        );
    }
    let oracle: Value =
        serde_json::from_str(include_str!("../tests/fixtures/cpp_calls/expected.json"))
            .expect("independent oracle");
    for label in oracle["canonical"].as_array().expect("labels") {
        let site = format!("// site:{}", label["site"].as_str().expect("site"));
        let definition = format!(
            "// def:{}",
            label["definition"].as_str().expect("definition")
        );
        let (line, character) = marked_position(&workspace.captured["src/ops.cpp"], &definition);
        assert_eq!(
            workspace.definition(&transport, "src/caller.cpp", &site),
            (
                lsp::uri_from_path(&workspace.source.join("src/ops.cpp")),
                line,
                character
            ),
            "{site}"
        );
    }
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn undeclared_files_cannot_change_has_include_or_satisfy_direct_includes() {
    let source = "#if __has_include(\"hidden.hpp\") || __has_include(\"../../external.hpp\")\n#error undeclared_header_visible\n#endif\nint known() { return 1; } // def:known\nint run() {\n  return known(); // site:known\n}\n";
    let workspace = Workspace::capture(
        &[
            ("src/unit.cpp", source),
            (
                "src/missing.cpp",
                "#include \"hidden.hpp\"\nint run() { return hidden(); }\n",
            ),
        ],
        &["src/unit.cpp", "src/missing.cpp"],
        &[],
    );
    write_file(
        &workspace.source.join("src/hidden.hpp"),
        "inline int hidden() { return 2; }\n",
    );
    write_file(
        &workspace
            .source
            .parent()
            .expect("outside source")
            .join("external.hpp"),
        "inline int external() { return 3; }\n",
    );
    let transport = workspace.spawn();
    let diagnostics = workspace.open(&transport, "src/unit.cpp");
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    let (line, character) = marked_position(source, "// def:known");
    assert_eq!(
        workspace.definition(&transport, "src/unit.cpp", "// site:known"),
        (
            lsp::uri_from_path(&workspace.source.join("src/unit.cpp")),
            line,
            character
        )
    );
    let diagnostics = workspace.open(&transport, "src/missing.cpp");
    assert!(
        errors(&diagnostics)
            .iter()
            .any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("file not found"))),
        "undeclared header must fail: {diagnostics:?}"
    );
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn header_parsing_uses_captured_bytes_when_the_original_changes() {
    let source = "#ifdef ACTIVATED\n#error project_config_activated\n#endif\n#if !__has_include(\"config.hpp\")\n#error declared_header_missing\n#endif\n#include \"config.hpp\"\n#if VALUE != 1\n#error uncaptured_header_bytes\n#endif\nint run() { return VALUE; }\n";
    let workspace = Workspace::capture(
        &[
            ("src/unit.cpp", source),
            ("include/config.hpp", "#define VALUE 1\n"),
        ],
        &["src/unit.cpp"],
        &[],
    );
    write_file(
        &workspace.source.join("include/config.hpp"),
        "#define VALUE 2\n",
    );
    assert!(
        workspace.inputs.verify_unchanged().is_err(),
        "original drift must abort production resolution"
    );
    write_file(
        &workspace.source.join(".clangd"),
        "CompileFlags:\n  Add: [-DACTIVATED]\n",
    );
    let transport = workspace.spawn();
    let diagnostics = workspace.open(&transport, "src/unit.cpp");
    assert!(
        errors(&diagnostics).is_empty(),
        "captured header expected: {diagnostics:?}"
    );
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn pragma_once_keeps_physical_aliases_without_merging_identical_distinct_headers() {
    let header = "#pragma once\nstatic_assert(__COUNTER__ < 2, \"alias was parsed twice\");\n";
    let source = "#include \"one.hpp\"\n#include \"alias.hpp\"\n#include \"same.hpp\"\nstatic_assert(__COUNTER__ == 2, \"distinct header was merged\");\nint run() { return 0; }\n";
    let workspace = Workspace::capture(
        &[
            ("src/unit.cpp", source),
            ("include/one.hpp", header),
            ("include/same.hpp", header),
        ],
        &["src/unit.cpp"],
        &[("include/alias.hpp", "include/one.hpp")],
    );
    let transport = workspace.spawn();
    let diagnostics = workspace.open(&transport, "src/unit.cpp");
    assert!(
        errors(&diagnostics).is_empty(),
        "header identities expected: {diagnostics:?}"
    );
    workspace
        .inputs
        .verify_unchanged()
        .expect("inputs unchanged");
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn semantic_tokens_distinguish_inherited_virtual_dispatch_at_the_call_site() {
    let workspace = call_fixture();
    let (transport, capabilities) = workspace.spawn_with_capabilities();
    for relative in ["src/ops.cpp", "src/caller.cpp"] {
        let diagnostics = workspace.open(&transport, relative);
        assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    }
    let legend = &capabilities["semanticTokensProvider"]["legend"];
    let modifiers = legend["tokenModifiers"]
        .as_array()
        .expect("token modifiers");
    assert!(modifiers.iter().any(|name| name == "virtual"));
    assert!(modifiers.iter().any(|name| name == "static"));
    let response = transport.request("textDocument/semanticTokens/full", &json!({
        "textDocument": {"uri": lsp::uri_from_path(&workspace.source.join("src/caller.cpp"))},
    })).expect("semantic tokens");
    let caller = &workspace.captured["src/caller.cpp"];
    let tokens = SemanticTokens::parse(&capabilities, &response, caller).expect("validated tokens");
    for (site, length, expected, kind, virtual_call, static_call) in [
        (
            "child-concrete",
            8,
            Some(CallKind::NonvirtualMember),
            "method",
            false,
            false,
        ),
        (
            "base-concrete",
            8,
            Some(CallKind::NonvirtualMember),
            "method",
            false,
            false,
        ),
        (
            "base-static",
            6,
            Some(CallKind::Static),
            "function",
            false,
            true,
        ),
        ("virtual-base", 2, None, "method", true, false),
        ("virtual-child", 2, None, "method", true, false),
        ("virtual-grandchild", 2, None, "method", true, false),
        ("fn-pointer", 2, None, "parameter", false, false),
    ] {
        let (line, character) = marked_position(caller, &format!("// site:{site}"));
        let token = tokens
            .exact_token(line, character, length)
            .expect("exact call token must exist");
        assert_eq!(token.kind, kind, "{site}");
        assert_eq!(token.modifiers.contains("virtual"), virtual_call, "{site}");
        assert_eq!(token.modifiers.contains("static"), static_call, "{site}");
        assert_eq!(
            tokens.call_kind(line, character, length),
            expected,
            "{site}"
        );
    }
    workspace
        .inputs
        .verify_unchanged()
        .expect("inputs unchanged");
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn symbol_info_pairs_bodies_with_calls_and_distinguishes_overloads_and_decoys() {
    let workspace = call_fixture();
    let transport = workspace.spawn();
    for relative in ["src/ops.cpp", "src/caller.cpp"] {
        let diagnostics = workspace.open(&transport, relative);
        assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    }
    let oracle: Value =
        serde_json::from_str(include_str!("../tests/fixtures/cpp_calls/expected.json"))
            .expect("oracle");
    let mut identities = BTreeMap::new();
    let bodies = workspace.body_anchors("src/ops.cpp");
    for label in oracle["canonical"].as_array().expect("canonical labels") {
        let site = label["site"].as_str().expect("site");
        let definition = label["definition"].as_str().expect("definition");
        let mut witnesses = Vec::new();
        for (relative, marker) in [
            ("src/caller.cpp", format!("// site:{site}")),
            ("src/ops.cpp", format!("// def:{definition}")),
        ] {
            let (line, character) = marked_position(&workspace.captured[relative], &marker);
            if relative == "src/ops.cpp" {
                assert_eq!(
                    bodies
                        .iter()
                        .filter(|body| {
                            lsp::byte_offset_to_lsp_pos(
                                workspace.captured[relative].as_bytes(),
                                body.name_span.start_byte,
                            ) == (line, character)
                        })
                        .count(),
                    1,
                    "independent body label must locate one parser anchor"
                );
            }
            let response = transport.request("textDocument/symbolInfo", &json!({
                "textDocument": {"uri": lsp::uri_from_path(&workspace.source.join(relative))},
                "position": {"line": line, "character": character},
            })).expect("symbol identity");
            witnesses.push(
                SymbolIdentity::parse(&response)
                    .expect("valid symbol response")
                    .expect("unique identity"),
            );
        }
        assert_eq!(witnesses[0], witnesses[1], "{site}");
        identities.insert(site, witnesses.remove(0));
    }
    assert_ne!(identities["pick-int"], identities["pick-scale"]);
    assert_ne!(identities["pick-int"], identities["decoy-pick"]);
    workspace
        .inputs
        .verify_unchanged()
        .expect("inputs unchanged");
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn compiled_qualifiers_locate_guarded_classes_and_distinguish_namespace_decoys() {
    let header = "#ifndef OWNERS_HPP\n#define OWNERS_HPP\nnamespace api {\nstruct Widget;\nstruct Widget { int run(); };\n}\nnamespace utility { int run(); }\nnamespace decoy { namespace Widget { int run(); } }\n#endif\n";
    let source = "#include \"owners.hpp\"\nint api::Widget::run() { return 1; }\nint utility::run() { return 2; }\nint decoy::Widget::run() { return 3; }\n";
    let workspace = Workspace::capture(
        &[("include/owners.hpp", header), ("src/owners.cpp", source)],
        &["src/owners.cpp"],
        &[],
    );
    let (transport, capabilities) = workspace.spawn_with_capabilities();
    let diagnostics = workspace.open(&transport, "src/owners.cpp");
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    let uri = lsp::uri_from_path(&workspace.source.join("src/owners.cpp"));
    let header_uri = lsp::uri_from_path(&workspace.source.join("include/owners.hpp"));
    let response = transport
        .request(
            "textDocument/semanticTokens/full",
            &json!({
                "textDocument": {"uri": uri},
            }),
        )
        .expect("compiled tokens");
    let tokens = SemanticTokens::parse(&capabilities, &response, source).expect("exact tokens");
    let bodies = workspace.body_anchors("src/owners.cpp");
    assert_eq!(bodies.len(), 3);
    for (line, character, length, kind, target_line, target_character, body_character, body_kind) in [
        (1, 9, 6, "class", 3, 7, 17, "method"),
        (2, 4, 7, "namespace", 6, 10, 13, "function"),
        (3, 11, 6, "namespace", 7, 28, 19, "function"),
    ] {
        let body = bodies
            .iter()
            .find(|body| {
                lsp::byte_offset_to_lsp_pos(source.as_bytes(), body.name_span.start_byte)
                    == (line, body_character)
            })
            .expect("independently labelled parser body");
        assert!(
            workspace
                .recovered_body(&transport, "src/owners.cpp", body, &tokens)
                .is_some()
        );
        let qualifier = body.qualifiers.last().expect("declarator qualifier");
        let position = lsp::byte_offset_to_lsp_pos(source.as_bytes(), qualifier.start_byte);
        assert_eq!(position, (line, character));
        assert_eq!(
            u32::try_from(qualifier.end_byte - qualifier.start_byte)
                .expect("ASCII qualifier length"),
            length
        );
        assert_eq!(
            tokens
                .exact_token(line, character, length)
                .expect("qualifier token")
                .kind,
            kind,
        );
        assert_eq!(
            tokens
                .exact_token(line, body_character, 3)
                .expect("body token")
                .kind,
            body_kind,
        );
        let response = transport
            .request(
                "textDocument/symbolInfo",
                &json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": line, "character": character},
                }),
            )
            .expect("qualifier identity");
        assert_eq!(response.as_array().map(Vec::len), Some(1));
        if kind == "class" {
            assert_eq!(
                response[0]["definitionRange"],
                json!({
                    "uri": header_uri,
                    "range": {
                        "start": {"line": 4, "character": 7},
                        "end": {"line": 4, "character": 13},
                    },
                })
            );
        }
        assert_eq!(
            response[0]["declarationRange"],
            json!({
                "uri": header_uri,
                "range": {
                    "start": {"line": target_line, "character": target_character},
                    "end": {"line": target_line, "character": target_character + length},
                },
            })
        );
    }
    workspace
        .inputs
        .verify_unchanged()
        .expect("inputs unchanged");
}

#[test]
#[ignore = "requires CODEMAP_CPP_CLANGD and CODEMAP_CPP_CLANGD_SHA256"]
fn compiled_ownership_recovers_static_members_and_withholds_alias_qualifiers() {
    let header = "#ifndef ALIASES_HPP\n#define ALIASES_HPP\nnamespace api { struct Widget { int run(); int other(); static int shared(); static int alias_shared(); }; }\n#endif\n";
    let source = "#include \"aliases.hpp\"\nusing Alias = api::Widget;\nnamespace Shortcut = api;\nint api::Widget::run() { return 1; }\nint Alias::other() { return 2; }\nint api::Widget::shared() { return 3; }\nint Shortcut::Widget::alias_shared() { return 4; }\n";
    let workspace = Workspace::capture(
        &[("include/aliases.hpp", header), ("src/aliases.cpp", source)],
        &["src/aliases.cpp"],
        &[],
    );
    let (transport, capabilities) = workspace.spawn_with_capabilities();
    let diagnostics = workspace.open(&transport, "src/aliases.cpp");
    assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    let response = transport.request("textDocument/semanticTokens/full", &json!({
        "textDocument": {"uri": lsp::uri_from_path(&workspace.source.join("src/aliases.cpp"))},
    })).expect("body tokens");
    let tokens = SemanticTokens::parse(&capabilities, &response, source).expect("tokens");
    let bodies = workspace.body_anchors("src/aliases.cpp");
    assert_eq!(bodies.len(), 4);
    for body in &bodies {
        let expected = matches!(body.normalized_name.as_str(), "run()" | "shared()");
        assert_eq!(
            workspace
                .recovered_body(&transport, "src/aliases.cpp", body, &tokens)
                .is_some(),
            expected
        );
    }
    workspace
        .inputs
        .verify_unchanged()
        .expect("inputs unchanged");
}
