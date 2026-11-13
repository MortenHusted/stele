use std::path::Path;
use std::process::Command;

use codemap::schema::{Language, Snapshot};
use codemap::{ExtractOptions, extract};
use codemap_lemmaspec::laravel;

const MODELS: &str = r"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model as Base;
use App\Models\Target as Related;
class Target extends Base {}
class Owner extends Base {
    public function targets() { return $this->hasMany(Related::class); }
    public function dynamic() { return $this->hasMany($class); }
    public function chain() { return $this->hasMany(Related::class)->where('active', true); }
    public function conditional($flag) { if ($flag) { return $this->hasOne(Related::class); } return null; }
    public function concat() { return $this->hasMany(Related::class, 'foo'.'bar'); }
    public function ordinary() { return $this->utility(); }
}
trait OverrideBuilder { public function hasMany($class) { return false; } }
class Altered extends Base {
    use OverrideBuilder;
    public function targets() { return $this->hasMany(Related::class); }
}
class ParentOverride extends Base { public function hasOne($class) { return false; } }
class ChildOverride extends ParentOverride { public function target() { return $this->hasOne(Related::class); } }
class UnknownTrait extends Base {
    use \Foreign\Unknown;
    public function targets() { return $this->hasMany(Related::class); }
}
class Unrelated { public function targets() { return $this->hasMany(Related::class); } }
";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn snapshot(source: &str) -> Snapshot {
    let root = tempfile::tempdir().unwrap().keep();
    let files = [
        ("app/Models/Models.php", source),
        (
            "resources/views/layout.blade.php",
            "<main>@yield('body')</main>",
        ),
        (
            "resources/views/shared/card.blade.php",
            "<article>card</article>",
        ),
        (
            "resources/views/components/card.blade.php",
            "<article>{{ $slot }}</article>",
        ),
        (
            "resources/views/components/slot.blade.php",
            "<span>decoy</span>",
        ),
        (
            "resources/views/post.blade.php",
            "@extends('layout')\n@include('shared.card')\n<x-card />\n<x-slot:title>title</x-slot:title>\n<x-slot>slot</x-slot>\n@include($dynamic)\n@include('missing')\n@include('vendor::remote')\n@include('shared'.'.card')\n",
        ),
        (
            "resources/views/invalid.blade.php",
            "@include('shared.card')\n@include('unterminated'\n",
        ),
    ];
    for (path, bytes) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    git(
        &root,
        &[
            "-c",
            "user.name=Synthetic",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    extract(&ExtractOptions {
        path: root,
        language: Language::Php,
        ..ExtractOptions::default()
    })
    .unwrap()
}

#[test]
fn laravel_topology_exposes_only_supported_source_relations() {
    let snapshot = snapshot(MODELS);
    let associations = laravel::associations(&snapshot);
    assert_eq!(associations.relations.len(), 1);
    assert_eq!(
        associations.relations[0].declaration,
        "targets: hasMany(App\\Models\\Target)"
    );
    assert_eq!(associations.unresolved["builder_override"], 2);
    assert_eq!(associations.unresolved["unknown_trait"], 1);
    assert_eq!(associations.unresolved["dynamic_target"], 1);
    assert_eq!(associations.unresolved["unsupported_chain"], 1);
    assert_eq!(associations.unresolved["unsupported_body"], 1);
    assert_eq!(associations.unresolved["unsupported_arguments"], 1);
    let views = laravel::templates(&snapshot);
    assert_eq!(views.relations.len(), 3);
    assert!(
        views
            .relations
            .iter()
            .all(|relation| relation.rel_path == "resources/views/post.blade.php")
    );
    assert_eq!(views.unresolved["parse_error"], 1);
    assert_eq!(views.unresolved["unsupported_slot"], 2);
    let out = tempfile::tempdir().unwrap();
    let input = out.path().join("snapshot.json");
    let json = out.path().join("topology.json");
    let html = out.path().join("topology.html");
    std::fs::write(&input, codemap::to_json(&snapshot).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_stele"))
        .args(["topology", "--snapshot"])
        .arg(input)
        .arg("--out")
        .arg(&json)
        .arg("--html")
        .arg(&html)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let topology: serde_json::Value =
        serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
    let relations = topology["codemap"]["policy_relations"].as_array().unwrap();
    assert_eq!(relations.len(), 4);
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation["policy"] == "policy:laravel-association-v1/php")
            .count(),
        1
    );
    assert!(
        std::fs::read_to_string(html)
            .unwrap()
            .contains("laravel-template-render-v1")
    );
    assert!(
        snapshot
            .edges
            .iter()
            .all(|edge| edge.kind != codemap::schema::EdgeKind::Calls)
    );
}

#[test]
fn incomplete_declaration_payloads_are_rejected_by_the_cli() {
    let snapshot = snapshot(MODELS);
    for omission in ["method", "class", "trait", "payload"] {
        let mut changed = snapshot.clone();
        let php = changed.php.as_mut().unwrap();
        match omission {
            "method" => php
                .methods
                .retain(|method| !method.node.ends_with("::OverrideBuilder::hasMany")),
            "class" => php
                .classes
                .retain(|class| !class.node.ends_with("::ParentOverride")),
            "trait" => php
                .classes
                .retain(|class| !class.node.ends_with("#App\\Models::OverrideBuilder")),
            _ => changed.php = None,
        }
        assert!(
            codemap::validate::validate(&changed).is_err(),
            "accepted missing {omission}"
        );
        let out = tempfile::tempdir().unwrap();
        let input = out.path().join("incomplete.json");
        std::fs::write(&input, codemap::to_json(&changed).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(input)
            .arg("--out")
            .arg(out.path().join("map.json"))
            .output()
            .unwrap();
        assert!(!result.status.success(), "CLI accepted missing {omission}");
    }
}

#[test]
fn invalid_method_ownership_is_rejected_by_the_cli() {
    use codemap::schema::{EdgeKind, edge_id};

    let snapshot = snapshot(
        r"<?php
namespace App;
use Illuminate\Database\Eloquent\Model;
class Target extends Model {}
class Owner extends Model {
    public function hasMany($class) { return false; }
    public function targets() { return $this->hasMany(Target::class); }
}
",
    );
    let method = snapshot
        .nodes
        .iter()
        .find(|node| node.name == "hasMany")
        .unwrap();
    let other = snapshot
        .nodes
        .iter()
        .find(|node| node.name == "Target")
        .unwrap();
    for mutation in ["missing", "wrong", "duplicate"] {
        let mut changed = snapshot.clone();
        let index = changed
            .edges
            .iter()
            .position(|edge| edge.kind == EdgeKind::Contains && edge.to == method.id)
            .unwrap();
        if mutation == "missing" {
            changed.edges.remove(index);
            *changed.report.edge_counts.get_mut("contains").unwrap() -= 1;
        } else {
            let mut edge = changed.edges[index].clone();
            edge.from = other.id.clone();
            edge.id = edge_id(
                edge.kind,
                &edge.from,
                &edge.to,
                edge.provenance.span.start_byte,
            );
            if mutation == "wrong" {
                changed.edges[index] = edge;
            } else {
                changed.edges.push(edge);
                *changed.report.edge_counts.get_mut("contains").unwrap() += 1;
            }
        }
        assert!(
            codemap::validate::validate(&changed).is_err(),
            "accepted {mutation} ownership"
        );
        assert!(
            laravel::associations(&changed).relations.is_empty(),
            "policy accepted {mutation} ownership"
        );
        let out = tempfile::tempdir().unwrap();
        let input = out.path().join("invalid.json");
        std::fs::write(&input, codemap::to_json(&changed).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_stele"))
            .args(["topology", "--snapshot"])
            .arg(input)
            .arg("--out")
            .arg(out.path().join("map.json"))
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "CLI accepted {mutation} ownership"
        );
    }
}

#[test]
fn anonymous_class_methods_do_not_become_model_associations() {
    let snapshot = snapshot(
        r"<?php
namespace App;
use Illuminate\Database\Eloquent\Model;
class Target extends Model {}
class Owner extends Model {
    public function factory() {
        return new class extends Model {
            public function nestedTargets() { return $this->hasMany(Target::class); }
        };
    }
}
return new class extends Model {
    public function targets() { return $this->hasMany(Target::class); }
};
",
    );
    codemap::validate::validate(&snapshot).unwrap();
    assert_eq!(laravel::associations(&snapshot).relations, []);
    let mut missing_scope = snapshot;
    missing_scope
        .php
        .as_mut()
        .unwrap()
        .anonymous_classes
        .clear();
    assert!(codemap::validate::validate(&missing_scope).is_err());
}

#[test]
fn callable_scopes_do_not_change_declared_class_names() {
    let snapshot = snapshot(
        r"<?php
namespace Illuminate\Database\Eloquent;
function register() { class Model { public function hasMany($class) { return false; } } }
register();
namespace App;
class Target extends \Illuminate\Database\Eloquent\Model {}
class Owner extends \Illuminate\Database\Eloquent\Model {
    public function targets() { return $this->hasMany(Target::class); }
}
",
    );
    let model = snapshot
        .php
        .as_ref()
        .unwrap()
        .classes
        .iter()
        .find(|class| class.qualified == "Illuminate\\Database\\Eloquent\\Model");
    assert!(model.is_some());
    assert_eq!(laravel::associations(&snapshot).relations, []);
}

#[test]
fn group_function_and_constant_imports_do_not_bind_model_classes() {
    for imports in [
        "use Illuminate\\Database\\Eloquent\\{function Model};",
        "use Illuminate\\Database\\Eloquent\\{const Model};",
        "use Illuminate\\Database\\Eloquent\\{Model as Base, function Model};",
        "use Illuminate\\Database\\Eloquent\\{ /* comment */ function Model, Model as Base};",
        "use function Illuminate\\Database\\Eloquent\\Model;",
        "use const Illuminate\\Database\\Eloquent\\Model;",
    ] {
        let source = format!(
            r"<?php
namespace App;
{imports}
class Model {{}}
class Target extends \Illuminate\Database\Eloquent\Model {{}}
class Owner extends Model {{ public function targets() {{ return $this->hasMany(Target::class); }} }}
"
        );
        let snapshot = snapshot(&source);
        assert_eq!(snapshot.report.parse_errors, 0, "{imports}");
        assert!(
            laravel::associations(&snapshot).relations.is_empty(),
            "{imports}"
        );
        let owner = snapshot
            .php
            .unwrap()
            .classes
            .into_iter()
            .find(|class| class.qualified == "App\\Owner")
            .unwrap();
        assert_eq!(owner.parent.unwrap().qualified, "App\\Model", "{imports}");
    }
}

#[test]
fn local_framework_root_shadows_and_adapted_traits_do_not_admit_associations() {
    let shadow = snapshot(
        r"<?php
namespace Illuminate\Database\Eloquent;
class Model {}
namespace App;
class Target extends \Illuminate\Database\Eloquent\Model {}
class Owner extends \Illuminate\Database\Eloquent\Model { public function targets() { return $this->hasMany(Target::class); } }
",
    );
    assert_eq!(laravel::associations(&shadow).relations, []);
    let adapted = snapshot(
        r"<?php
namespace App;
use Illuminate\Database\Eloquent\Model;
trait Original { public function other($class) {} }
class Target extends Model {}
class Owner extends Model {
    use Original { other as hasMany; }
    public function targets() { return $this->hasMany(Target::class); }
}
",
    );
    assert_eq!(laravel::associations(&adapted).relations, []);
    assert_eq!(
        laravel::associations(&adapted).unresolved["trait_adaptation"],
        1
    );
}
