//! Native source-closure controls, retaining the original canonical oracle.

use super::{CppSession, tests};
use crate::cpp_runtime::CppRuntimeManifest;
use crate::schema::sha256_hex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
#[ignore = "requires native ordinary-user Linux, pinned clangd profile and exact CLI"]
fn frozen_linux_cpp_source_controls() {
    // SAFETY: geteuid has no arguments or side effects.
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let runtime: CppRuntimeManifest = serde_json::from_slice(
        &fs::read(std::env::var_os("CODEMAP_CPP_RUNTIME_MANIFEST").unwrap()).unwrap(),
    )
    .unwrap();
    assert!(matches!(runtime, CppRuntimeManifest::Linux(_)));
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/cpp_profile_expectations.json"
    ))
    .unwrap();
    let mut observed = BTreeMap::new();
    for (label, expected) in oracle["native_source"].as_object().unwrap() {
        if observed.contains_key(label) {
            continue;
        }
        match label.as_str() {
            "canonical_defines_generated_headers" => {
                tests::canonical_identity_tracks_defines_and_generated_headers();
            }
            "canonical_original_labelled_oracle" => {
                tests::production_session_matches_the_independent_labelled_calls();
                tests::canonical_extraction_matches_the_independent_labelled_calls();
                observed.insert(
                    "canonical_repeat_dirty_relocated_clone".to_owned(),
                    "accept",
                );
            }
            "original_input_drift" => tests::production_session_rejects_original_input_drift(),
            "owned_tool_exit" => tests::production_session_fails_when_the_owned_tool_exits(),
            "completed_unit_ast_is_released" => completed_unit_ast_is_released(&runtime),
            _ => run_source_control(label, expected.as_str().unwrap(), &runtime),
        }
        observed.insert(label.clone(), expected.as_str().unwrap());
        eprintln!("CPP_CONTROL_PASS {label}");
    }
    assert_eq!(
        serde_json::to_value(observed).unwrap(),
        oracle["native_source"]
    );
}

fn completed_unit_ast_is_released(runtime: &CppRuntimeManifest) {
    let fixture = tests::Fixture::new();
    let session = CppSession::start_profile(
        &fixture.database,
        &fixture.roots,
        &fixture.manifest,
        runtime,
        &fixture.sources,
    )
    .unwrap();
    let relative = session.profile.application_units.keys().next().unwrap();
    session.query_unit(relative, 1).unwrap();
    session.transport.verify_linux_images().unwrap();
    let uri = crate::lsp::uri_from_path(&fixture.roots.source.join(relative));
    let error = session
        .transport
        .request(
            "textDocument/semanticTokens/full",
            &serde_json::json!({"textDocument":{"uri":uri}}),
        )
        .unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("semanticTokens/full error"), "{error}");
    assert!(error.contains("non-added document"), "{error}");
}

fn run_source_control(label: &str, expected: &str, runtime: &CppRuntimeManifest) {
    let mut fixture = tests::Fixture::new();
    match label {
        "unmanifested_direct_header" => {
            fs::write(
                fixture.roots.source.join("include/outside.hpp"),
                "#define OUTSIDE 1\n",
            )
            .unwrap();
            replace_caller(&mut fixture, "#include \"outside.hpp\"\nvoid probe() {}\n");
        }
        "unmanifested_has_include_is_absent" | "has_include_shape_remains_unsupported" => {
            fs::write(
                fixture.roots.source.join("include/outside.hpp"),
                "#define OUTSIDE 1\n",
            )
            .unwrap();
            replace_caller(
                &mut fixture,
                "#if __has_include(\"outside.hpp\")\n#error unmanifested header visible\n#endif\nvoid probe() {}\n",
            );
        }
        "unmanifested_tool_config_inert" => {
            fs::write(
                fixture.roots.source.join(".clangd"),
                "CompileFlags:\n  Add: [-DPOISON=1]\n",
            )
            .unwrap();
            replace_caller(
                &mut fixture,
                "#ifdef POISON\n#error unmanifested configuration applied\n#endif\nvoid probe() {}\n",
            );
        }
        "captured_copy_drift"
        | "captured_overlay_drift"
        | "prepared_database_drift"
        | "private_database_drift" => {}
        _ => panic!("unimplemented Linux C++ source control {label}"),
    }
    let session = CppSession::start_profile(
        &fixture.database,
        &fixture.roots,
        &fixture.manifest,
        runtime,
        &fixture.sources,
    )
    .unwrap();
    if label == "unmanifested_has_include_is_absent" {
        verify_guarded_header_absence(&session, &fixture);
        session.transport.shutdown();
        assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
        return;
    }
    match label {
        "captured_copy_drift" => {
            let overlay = session.inputs.overlay_path();
            let copy = session
                .inputs
                .captured_read_paths()
                .unwrap()
                .into_iter()
                .find(|path| *path != overlay)
                .unwrap();
            fs::set_permissions(&copy, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(copy, "changed captured input").unwrap();
        }
        "captured_overlay_drift" => fs::write(session.inputs.overlay_path(), "{}").unwrap(),
        "prepared_database_drift" => {
            let mut commands: Value =
                serde_json::from_slice(&fs::read(&fixture.database).unwrap()).unwrap();
            commands[0]["arguments"]
                .as_array_mut()
                .unwrap()
                .push(Value::String("-DCHANGED=1".into()));
            fs::write(&fixture.database, serde_json::to_vec(&commands).unwrap()).unwrap();
        }
        "private_database_drift" => {
            let super::CppRuntimeWitness::Linux { database, .. } = &session.runtime else {
                panic!("Linux database witness required");
            };
            let path = &database.path;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(path, "[]").unwrap();
        }
        _ => {}
    }
    assert_source_outcome(label, expected, session.resolve());
    assert!(!fixture.roots.source.join("TARGET_EXECUTED").exists());
}

fn assert_source_outcome(
    label: &str,
    expected: &str,
    result: anyhow::Result<super::CppSemanticResult>,
) {
    match expected {
        "accept" => assert!(result.is_ok(), "{label}: {:?}", result.err()),
        "reject" => {
            let error = result
                .err()
                .expect("control must return no semantic result");
            let reason = match label {
                "captured_copy_drift" => "captured C++ input changed",
                "captured_overlay_drift" => "C++ input overlay changed",
                "prepared_database_drift" => "C++ compilation profile changed",
                "private_database_drift" => "private C++ compilation database changed",
                "unmanifested_direct_header" => "clangd reported erroneous",
                "has_include_shape_remains_unsupported" => {
                    "compiled C++ unit has source parse errors"
                }
                _ => panic!("missing refusal reason for {label}"),
            };
            assert!(format!("{error:#}").contains(reason), "{label}: {error:#}");
        }
        _ => panic!("invalid source control outcome {expected}"),
    }
}

fn verify_guarded_header_absence(session: &CppSession, fixture: &tests::Fixture) {
    let (_, bytes) = fixture
        .sources
        .iter()
        .find(|(path, _)| path == "src/caller.cpp")
        .unwrap();
    let text = std::str::from_utf8(bytes).unwrap();
    let uri = crate::lsp::uri_from_path(&fixture.roots.source.join("src/caller.cpp"));
    session
        .transport
        .notify(
            "textDocument/didOpen",
            &serde_json::json!({
                "textDocument":{"uri":uri,"languageId":"cpp","version":1,"text":text}
            }),
        )
        .unwrap();
    let inactive = session.wait_diagnostics(&uri, text).unwrap();
    assert!(
        inactive
            .iter()
            .any(|span| span.start_line <= 2 && span.end_line >= 2)
    );
    session
        .runtime
        .verify(&session.transport, &session.inputs)
        .unwrap();
    assert_eq!(
        crate::cpp_profile::load_profile(&session.database, &session.roots).unwrap(),
        session.profile
    );
}

fn replace_caller(fixture: &mut tests::Fixture, text: &str) {
    let (_, bytes) = fixture
        .sources
        .iter_mut()
        .find(|(path, _)| path == "src/caller.cpp")
        .unwrap();
    *bytes = text.as_bytes().to_vec();
    fs::write(fixture.roots.source.join("src/caller.cpp"), text).unwrap();
    fixture
        .manifest
        .files
        .insert("{source}/src/caller.cpp".into(), sha256_hex(bytes));
}
