//! The named rules that decide which snapshot facts become evidence.
//!
//! Every classification the adapter makes on its own authority lives here
//! as a plain function with a constant name. A fact classified by one of
//! these rules cites `policy:<name>/<language>` as its provenance, so a
//! reviewer reading a finding can trace it to the exact rule and challenge
//! that rule rather than the extractor.

use codemap_graph::schema::{Language, Node, NodeKind};

/// A callable that ships: a function, method, or macro outside any inline
/// Rust test module. Types and traits are reached through their methods,
/// which a call graph sees, never through themselves.
pub const SHIPPED_DEFINITION: &str = "shipped-definition";
/// Which directory a file lives under decides whether it ships or tests.
pub const FILE_LAYOUT: &str = "file-layout";
/// Where execution starts by convention of the language's toolchain.
pub const ENTRY_POINT: &str = "entry-point";
/// The cyclomatic complexity above which the `hotspots` checker calls an
/// entry point a hotspot.
///
/// Radon's high band, `SlopCodeBench`'s erosion cut, and
/// `codemap_slop::policy::HIGH_CC_CUTOFF`. The checker carries the literal
/// in its rule; tests on both sides hold the three equal.
pub const HOTSPOT_CUTOFF: u32 = 10;

/// The provenance string a policy-classified fact carries.
pub fn provenance(policy: &str, language: Language) -> String {
    format!("policy:{policy}/{}", language.tag())
}

/// Whether a node is a callable definition that ships.
///
/// Only a callable can be the target of a `calls` edge, so only a callable
/// can be judged live or dead; a type or trait is reached through its
/// methods. Files and modules are containers, and `ImportRef` is codemap's
/// placeholder for a `use` target it could not map. A Rust item whose
/// scope passes through a `tests` or `test` module is compiled only under
/// `#[cfg(test)]` by convention, so it neither ships nor counts as a
/// production caller.
pub fn is_shipped_definition(language: Language, node: &Node) -> bool {
    if !matches!(
        node.kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Macro
    ) {
        return false;
    }
    match language {
        Language::Rust => !in_rust_test_module(&node.symbol_key),
        Language::Ruby | Language::Php => true,
        // A `_test.go` file is compiled only by `go test`, so nothing in
        // it ships: the Go analogue of Rust's `#[cfg(test)]` modules.
        Language::Go => !is_go_test_file(&node.provenance.rel_path),
        // C++ has no shipping policy yet. Absence checkers must not treat
        // that as "nothing ships"; callers keep C++ out of those checkers.
        Language::Cpp | Language::JsTs | Language::Python => false,
    }
}

fn is_go_test_file(rel_path: &str) -> bool {
    rel_path.ends_with("_test.go")
}

fn in_rust_test_module(symbol_key: &str) -> bool {
    let Some((_, qualified)) = symbol_key.split_once('#') else {
        return false;
    };
    let Some((scope, _name)) = qualified.rsplit_once("::") else {
        return false;
    };
    scope
        .split("::")
        .any(|segment| segment == "tests" || segment == "test")
}

/// File roles under the named conventional-layout policy, not Cargo configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    Production,
    Test,
    /// No marker directory on the path: the file gets `defines` facts but no
    /// layout fact, so no rule can call it dead or test-only.
    Unclassified,
}

/// Classify a file under the language's conventional layout.
/// Rust test/benchmark/example directories anywhere on the path, and
/// `_test.rs` / `_tests.rs` suffixes, take priority over `src`. These are
/// naming conventions; they do not establish which cfgs Cargo compiles.
/// Other languages use the first marker directory unless stated below.
///
/// Matching on segments rather than a path prefix keeps workspaces
/// (`crates/x/src/lib.rs`) classified. Go tests live next to the code they
/// test and test inputs under any `testdata` directory, so for Go those two
/// rules decide before the layout markers: a `_test.go` file, or anything
/// under `testdata`, is a test wherever it sits.
pub fn classify_file(language: Language, rel_path: &str) -> FileClass {
    let (production, test): (&[&str], &[&str]) = match language {
        Language::Rust => (&["src"], &["tests", "benches", "examples"]),
        Language::Ruby => (&["lib", "app"], &["spec", "test"]),
        Language::Php => (&["src", "app", "lib"], &["tests", "test", "spec"]),
        Language::Go => (&["cmd", "internal", "pkg"], &[]),
        // No conventional layout yet. Every file stays unclassified so a
        // directory name cannot decide that C++ code is dead.
        Language::Cpp | Language::JsTs | Language::Python => return FileClass::Unclassified,
    };
    if language == Language::Rust {
        let (directories, file_name) = rel_path.rsplit_once('/').unwrap_or(("", rel_path));
        if directories
            .split('/')
            .any(|segment| test.contains(&segment))
            || file_name.ends_with("_test.rs")
            || file_name.ends_with("_tests.rs")
        {
            return FileClass::Test;
        }
    }
    let Some((directories, _file_name)) = rel_path.rsplit_once('/') else {
        return if language == Language::Go && is_go_test_file(rel_path) {
            FileClass::Test
        } else {
            FileClass::Unclassified
        };
    };
    if language == Language::Go
        && (is_go_test_file(rel_path) || directories.split('/').any(|s| s == "testdata"))
    {
        return FileClass::Test;
    }
    for segment in directories.split('/') {
        if production.contains(&segment) {
            return FileClass::Production;
        }
        if test.contains(&segment) {
            return FileClass::Test;
        }
    }
    FileClass::Unclassified
}

/// Whether a node is where execution starts by convention of the
/// language's toolchain or its dominant application framework.
///
/// - Rust: the top-level `main` of a binary root (`src/main.rs`,
///   `src/bin/x.rs`, `src/bin/x/main.rs`).
/// - Go: the `main` function of a `main` package, which the snapshot
///   knows from the package scope rather than the path because that is
///   Go's own rule.
/// - Ruby (Rails): instance methods of classes under `app/controllers`
///   (actions), `perform` under `app/jobs`, and `perform` or `call` under
///   `app/workers`. `initialize` and singleton methods are never actions.
/// - PHP (Laravel, Symfony): methods of classes under
///   `app/Http/Controllers` or `src/Controller` (actions, including
///   `__invoke`), and `handle` or `__invoke` under `app/Jobs` and
///   `app/Console/Commands`. `__construct` is never an action.
///
/// The rules read paths and names from the snapshot only; the snapshot
/// does not record visibility, so a private helper inside a controller
/// counts as an entry point too. That over-approximation can hide dead
/// code behind a controller; it can never report live code as dead.
/// Anything else is a review decision passed on the command line.
pub fn is_default_entry_point(language: Language, node: &Node) -> bool {
    let path = node.provenance.rel_path.as_str();
    let name = node.name.as_str();
    match language {
        Language::Rust => {
            node.kind == NodeKind::Function
                && node.symbol_key.ends_with("#main")
                && is_rust_binary_root(path)
        }
        Language::Ruby => {
            node.kind == NodeKind::Method
                && !name.starts_with("self.")
                && name != "initialize"
                && (under(path, &["app", "controllers"])
                    || (under(path, &["app", "jobs"]) && name == "perform")
                    || (under(path, &["app", "workers"]) && (name == "perform" || name == "call")))
        }
        Language::Php => {
            node.kind == NodeKind::Method
                && name != "__construct"
                && ((under(path, &["app", "Http", "Controllers"])
                    || under(path, &["src", "Controller"]))
                    || ((under(path, &["app", "Jobs"])
                        || under(path, &["app", "Console", "Commands"]))
                        && (name == "handle" || name == "__invoke")))
        }
        Language::Go => node.kind == NodeKind::Function && node.symbol_key.ends_with("#main::main"),
        // Entry points are reviewer-declared until a C++ coverage contract exists.
        Language::Cpp | Language::JsTs | Language::Python => false,
    }
}

/// Whether the file's directories contain `marker` as consecutive
/// segments anywhere, so an engine or package nested in a monorepo
/// (`engines/billing/app/controllers/...`) is classified like the root.
fn under(rel_path: &str, marker: &[&str]) -> bool {
    let Some((directories, _file_name)) = rel_path.rsplit_once('/') else {
        return false;
    };
    let segments: Vec<&str> = directories.split('/').collect();
    segments
        .windows(marker.len())
        .any(|window| window == marker)
}

fn is_rust_binary_root(rel_path: &str) -> bool {
    let segments: Vec<&str> = rel_path.split('/').collect();
    let Some(src) = segments.iter().position(|segment| *segment == "src") else {
        return false;
    };
    match &segments[src + 1..] {
        ["main.rs"] | ["bin", _, "main.rs"] => true,
        ["bin", file] => std::path::Path::new(file).extension() == Some("rs".as_ref()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codemap_graph::schema::{FactSource, Provenance, Span, TypeKind, node_id, symbol_key};

    fn node(
        language: Language,
        kind: NodeKind,
        rel_path: &str,
        scope: &[&str],
        name: &str,
    ) -> Node {
        let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
        Node {
            python: None,
            id: node_id(language, kind, rel_path, &scope, name),
            kind,
            name: name.to_string(),
            symbol_key: symbol_key(language, rel_path, &scope, name),
            provenance: Provenance {
                repository: "r".into(),
                commit_sha: "c".into(),
                rel_path: rel_path.to_string(),
                span: Span {
                    start_byte: 0,
                    end_byte: 1,
                    start_line: 1,
                    start_col: 0,
                    end_line: 1,
                    end_col: 1,
                },
                file_hash: "h".into(),
                extractor_version: "v".into(),
                fact_source: FactSource::TreeSitter,
                confidence: u8::MAX,
            },
        }
    }

    #[test]
    fn callables_ship_but_types_containers_and_placeholders_do_not() {
        let rust = Language::Rust;
        for kind in [NodeKind::Function, NodeKind::Method, NodeKind::Macro] {
            assert!(is_shipped_definition(
                rust,
                &node(rust, kind, "src/a.rs", &[], "x")
            ));
        }
        for kind in [
            NodeKind::File,
            NodeKind::Module,
            NodeKind::ImportRef,
            NodeKind::Trait,
            NodeKind::Type(TypeKind::Struct),
        ] {
            assert!(!is_shipped_definition(
                rust,
                &node(rust, kind, "src/a.rs", &[], "x")
            ));
        }
    }

    #[test]
    fn rust_items_inside_inline_test_modules_do_not_ship() {
        let rust = Language::Rust;
        let f = NodeKind::Function;
        assert!(!is_shipped_definition(
            rust,
            &node(rust, f, "src/a.rs", &["tests"], "checks")
        ));
        assert!(!is_shipped_definition(
            rust,
            &node(rust, f, "src/a.rs", &["test"], "checks")
        ));
        assert!(!is_shipped_definition(
            rust,
            &node(rust, f, "src/a.rs", &["inner", "tests"], "checks")
        ));
        assert!(is_shipped_definition(
            rust,
            &node(rust, f, "src/a.rs", &["inner"], "run")
        ));
        // A function merely named `tests` is not a test module.
        assert!(is_shipped_definition(
            rust,
            &node(rust, f, "src/a.rs", &[], "tests")
        ));
    }

    #[test]
    fn nested_rust_test_files_have_test_layout() {
        let rust = Language::Rust;
        for path in [
            "src/service/tests/check.rs",
            "crates/widget/src/tests/nested/check.rs",
            "src/service/benches/measure.rs",
            "src/service/examples/demo.rs",
            "src/service/parser_test.rs",
            "src/service/parser_tests.rs",
            "parser_tests.rs",
        ] {
            assert_eq!(classify_file(rust, path), FileClass::Test, "{path}");
            assert!(
                is_shipped_definition(rust, &node(rust, NodeKind::Function, path, &[], "helper")),
                "{path}"
            );
        }
        for path in [
            "src/service/latest.rs",
            "src/contest/run.rs",
            "src/test_support.rs",
        ] {
            assert_eq!(classify_file(rust, path), FileClass::Production, "{path}");
            assert!(
                is_shipped_definition(rust, &node(rust, NodeKind::Function, path, &[], "helper")),
                "{path}"
            );
        }
    }

    #[test]
    fn ruby_scopes_named_like_test_modules_still_ship() {
        let ruby = Language::Ruby;
        let m = NodeKind::Method;
        assert!(is_shipped_definition(
            ruby,
            &node(ruby, m, "lib/a.rb", &["Tests"], "run")
        ));
        assert!(is_shipped_definition(
            ruby,
            &node(ruby, m, "lib/a.rb", &["tests"], "run")
        ));
    }

    #[test]
    fn the_first_marker_directory_classifies_a_file() {
        let rust = Language::Rust;
        assert_eq!(classify_file(rust, "src/lib.rs"), FileClass::Production);
        assert_eq!(
            classify_file(rust, "crates/x/src/lib.rs"),
            FileClass::Production
        );
        assert_eq!(
            classify_file(rust, "caller/src/main.rs"),
            FileClass::Production
        );
        assert_eq!(classify_file(rust, "tests/it.rs"), FileClass::Test);
        assert_eq!(
            classify_file(rust, "crates/x/benches/b.rs"),
            FileClass::Test
        );
        assert_eq!(classify_file(rust, "examples/demo.rs"), FileClass::Test);
        assert_eq!(classify_file(rust, "tests/src/helper.rs"), FileClass::Test);
        assert_eq!(classify_file(rust, "build.rs"), FileClass::Unclassified);
        assert_eq!(
            classify_file(rust, "xtask/main.rs"),
            FileClass::Unclassified
        );

        let ruby = Language::Ruby;
        assert_eq!(classify_file(ruby, "lib/a.rb"), FileClass::Production);
        assert_eq!(
            classify_file(ruby, "app/models/u.rb"),
            FileClass::Production
        );
        assert_eq!(classify_file(ruby, "spec/a_spec.rb"), FileClass::Test);
        assert_eq!(classify_file(ruby, "test/a_test.rb"), FileClass::Test);
        assert_eq!(classify_file(ruby, "Rakefile"), FileClass::Unclassified);

        let php = Language::Php;
        assert_eq!(classify_file(php, "src/Service.php"), FileClass::Production);
        assert_eq!(
            classify_file(php, "app/Models/User.php"),
            FileClass::Production
        );
        assert_eq!(classify_file(php, "lib/util.php"), FileClass::Production);
        assert_eq!(
            classify_file(php, "tests/Unit/UserTest.php"),
            FileClass::Test
        );
        assert_eq!(classify_file(php, "spec/UserSpec.php"), FileClass::Test);
        assert_eq!(
            classify_file(php, "public/index.php"),
            FileClass::Unclassified
        );

        let go = Language::Go;
        assert_eq!(classify_file(go, "cmd/app/main.go"), FileClass::Production);
        assert_eq!(
            classify_file(go, "internal/svc/svc.go"),
            FileClass::Production
        );
        assert_eq!(classify_file(go, "pkg/gui/gui.go"), FileClass::Production);
        assert_eq!(
            classify_file(go, "pkg/gui/gui_test.go"),
            FileClass::Test,
            "a Go test file is a test wherever it lives"
        );
        assert_eq!(classify_file(go, "main_test.go"), FileClass::Test);
        assert_eq!(classify_file(go, "pkg/testdata/input.go"), FileClass::Test);
        assert_eq!(classify_file(go, "main.go"), FileClass::Unclassified);
        assert_eq!(classify_file(go, "tools/gen.go"), FileClass::Unclassified);

        let cpp = Language::Cpp;
        assert_eq!(
            classify_file(cpp, "src/unit.cpp"),
            FileClass::Unclassified,
            "C++ has no layout policy yet"
        );
    }

    #[test]
    fn go_test_files_do_not_ship() {
        let go = Language::Go;
        assert!(!is_shipped_definition(
            go,
            &node(go, NodeKind::Function, "pkg/x/x_test.go", &["x"], "TestRun")
        ));
        assert!(is_shipped_definition(
            go,
            &node(go, NodeKind::Function, "pkg/x/x.go", &["x"], "Run")
        ));
        assert!(is_shipped_definition(
            go,
            &node(go, NodeKind::Method, "pkg/x/x.go", &["x", "Thing"], "Run")
        ));
    }

    #[test]
    fn the_main_function_of_a_go_main_package_is_an_entry_point_by_default() {
        let go = Language::Go;
        let f = NodeKind::Function;
        assert!(is_default_entry_point(
            go,
            &node(go, f, "main.go", &["main"], "main")
        ));
        assert!(is_default_entry_point(
            go,
            &node(go, f, "cmd/tool/main.go", &["main"], "main")
        ));
        assert!(
            !is_default_entry_point(go, &node(go, f, "pkg/app/app.go", &["app"], "main")),
            "a main function outside package main is an ordinary function"
        );
        assert!(!is_default_entry_point(
            go,
            &node(go, f, "main.go", &["main"], "run")
        ));
        assert!(!is_default_entry_point(
            go,
            &node(go, NodeKind::Method, "main.go", &["main", "App"], "main")
        ));
    }

    #[test]
    fn rails_actions_jobs_and_workers_are_entry_points_by_default() {
        let ruby = Language::Ruby;
        let m = NodeKind::Method;
        let controller = "app/controllers/users_controller.rb";
        let scope = &["UsersController"];
        assert!(is_default_entry_point(
            ruby,
            &node(ruby, m, controller, scope, "index")
        ));
        assert!(is_default_entry_point(
            ruby,
            &node(ruby, m, controller, scope, "archive")
        ));
        assert!(is_default_entry_point(
            ruby,
            &node(
                ruby,
                m,
                "engines/billing/app/controllers/invoices_controller.rb",
                &["InvoicesController"],
                "show"
            )
        ));
        assert!(is_default_entry_point(
            ruby,
            &node(ruby, m, "app/jobs/sync_job.rb", &["SyncJob"], "perform")
        ));
        assert!(is_default_entry_point(
            ruby,
            &node(
                ruby,
                m,
                "app/workers/mail_worker.rb",
                &["MailWorker"],
                "call"
            )
        ));

        // Decoys: never actions.
        assert!(!is_default_entry_point(
            ruby,
            &node(ruby, m, controller, scope, "initialize")
        ));
        assert!(!is_default_entry_point(
            ruby,
            &node(ruby, m, controller, scope, "self.build")
        ));
        assert!(!is_default_entry_point(
            ruby,
            &node(ruby, m, "app/jobs/sync_job.rb", &["SyncJob"], "chunk")
        ));
        assert!(
            !is_default_entry_point(
                ruby,
                &node(
                    ruby,
                    m,
                    "lib/controllers/users_controller.rb",
                    scope,
                    "index"
                )
            ),
            "lib is not app"
        );
        assert!(!is_default_entry_point(
            ruby,
            &node(ruby, m, "app/models/user.rb", &["User"], "perform")
        ));
        assert!(
            !is_default_entry_point(
                ruby,
                &node(ruby, NodeKind::Function, controller, &[], "index")
            ),
            "only methods are actions"
        );
    }

    #[test]
    fn php_controllers_jobs_and_commands_are_entry_points_by_default() {
        let php = Language::Php;
        let m = NodeKind::Method;
        let laravel = "app/Http/Controllers/UserController.php";
        let symfony = "src/Controller/UserController.php";
        let scope = &["App\\Http\\Controllers", "UserController"];
        assert!(is_default_entry_point(
            php,
            &node(php, m, laravel, scope, "index")
        ));
        assert!(is_default_entry_point(
            php,
            &node(php, m, laravel, scope, "__invoke")
        ));
        assert!(is_default_entry_point(
            php,
            &node(php, m, symfony, scope, "list")
        ));
        assert!(is_default_entry_point(
            php,
            &node(php, m, "app/Jobs/SyncUsers.php", &["SyncUsers"], "handle")
        ));
        assert!(is_default_entry_point(
            php,
            &node(
                php,
                m,
                "app/Console/Commands/Prune.php",
                &["Prune"],
                "handle"
            )
        ));
        assert!(is_default_entry_point(
            php,
            &node(
                php,
                m,
                "app/Console/Commands/Prune.php",
                &["Prune"],
                "__invoke"
            )
        ));

        // Decoys: never actions.
        assert!(!is_default_entry_point(
            php,
            &node(php, m, laravel, scope, "__construct")
        ));
        assert!(!is_default_entry_point(
            php,
            &node(php, m, "app/Jobs/SyncUsers.php", &["SyncUsers"], "chunk")
        ));
        assert!(!is_default_entry_point(
            php,
            &node(php, m, "app/Services/Sync.php", &["Sync"], "handle")
        ));
        assert!(
            !is_default_entry_point(
                php,
                &node(php, m, "src/Service/UserController.php", scope, "index")
            ),
            "the class name is not the rule, the tree is"
        );
        assert!(!is_default_entry_point(
            php,
            &node(php, NodeKind::Function, laravel, &[], "index")
        ));
    }

    #[test]
    fn a_rust_binary_root_main_is_an_entry_point_by_default() {
        let rust = Language::Rust;
        let f = NodeKind::Function;
        assert!(is_default_entry_point(
            rust,
            &node(rust, f, "src/main.rs", &[], "main")
        ));
        assert!(is_default_entry_point(
            rust,
            &node(rust, f, "crates/x/src/main.rs", &[], "main")
        ));
        assert!(is_default_entry_point(
            rust,
            &node(rust, f, "src/bin/tool.rs", &[], "main")
        ));
        assert!(is_default_entry_point(
            rust,
            &node(rust, f, "src/bin/tool/main.rs", &[], "main")
        ));
        assert!(!is_default_entry_point(
            rust,
            &node(rust, f, "src/lib.rs", &[], "main")
        ));
        assert!(!is_default_entry_point(
            rust,
            &node(rust, f, "src/main.rs", &[], "run")
        ));
        assert!(!is_default_entry_point(
            rust,
            &node(rust, f, "src/main.rs", &["cli"], "main")
        ));
        assert!(!is_default_entry_point(
            rust,
            &node(rust, NodeKind::Method, "src/main.rs", &[], "main")
        ));
        assert!(!is_default_entry_point(
            Language::Ruby,
            &node(Language::Ruby, NodeKind::Method, "lib/main.rb", &[], "main")
        ));
        assert!(!is_default_entry_point(
            Language::Php,
            &node(
                Language::Php,
                NodeKind::Function,
                "public/index.php",
                &[],
                "main"
            )
        ));
    }

    #[test]
    fn policy_provenance_names_the_rule_and_the_language() {
        assert_eq!(
            provenance(FILE_LAYOUT, Language::Rust),
            "policy:file-layout/rust"
        );
        assert_eq!(
            provenance(ENTRY_POINT, Language::Ruby),
            "policy:entry-point/ruby"
        );
    }
}
