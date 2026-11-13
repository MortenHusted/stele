use codemap::schema::{BladeReferenceKind, Language, NodeKind, PhpLiteral, PhpMethodBody};
use codemap::{ExtractOptions, extract};

mod common;

fn fixture() -> std::path::PathBuf {
    common::make_repo_with(&[
        ("app/Models/Models.php".into(), r"<?php
namespace App\Models;
use Illuminate\Database\Eloquent\Model as Base;
use App\Models\User as Person;
class User extends Base {}
class Post extends Base {
    public function author() { return $this->belongsTo(Person::class, 'author_id'); }
    public function dynamic() { return $this->hasMany($class); }
    public function chained() { return $this->hasMany(Person::class)->where('active', true); }
    public static function singleton() { return $this->hasOne(Person::class); }
}
trait OverrideBuilder { public function hasMany($class) { return false; } }
class Altered extends Base {
    use OverrideBuilder;
    public function users() { return $this->hasMany(User::class); }
}
".into()),
        ("resources/views/layout.blade.php".into(), "<html>@yield('body')</html>".into()),
        ("resources/views/components/card.blade.php".into(), "<article>{{ $slot }}</article>".into()),
        ("resources/views/post.blade.php".into(), "@extends('layout')\n@include('components.card', ['x' => nested(1)])\n{{-- @include('comment') --}}\n@@include('escaped')\n@verbatim @include('verbatim') @endverbatim\n<x-card />\n@include($dynamic)\n".into()),
        ("src/lib.rs".into(), "pub fn unrelated() {}\n".into()),
    ]).unwrap()
}

#[test]
fn laravel_source_observations_preserve_bindings_and_templates() {
    let root = fixture();
    let options = ExtractOptions {
        path: root,
        language: Language::Php,
        measures: true,
        ..ExtractOptions::default()
    };
    let snapshot = extract(&options).unwrap();
    assert_eq!(
        codemap::to_json(&snapshot).unwrap(),
        codemap::to_json(&extract(&options).unwrap()).unwrap()
    );
    let php = snapshot.php.as_ref().unwrap();
    let post = php
        .classes
        .iter()
        .find(|class| class.qualified == "App\\Models\\Post")
        .unwrap();
    assert_eq!(
        post.parent.as_ref().unwrap().qualified,
        "Illuminate\\Database\\Eloquent\\Model"
    );
    let author = php
        .methods
        .iter()
        .find(|method| method.node.ends_with("::Post::author"))
        .unwrap();
    let PhpMethodBody::ReturnCall {
        receiver,
        name,
        arguments,
        provenance,
    } = &author.body
    else {
        panic!("direct return")
    };
    assert_eq!(receiver, "$this");
    assert_eq!(name, "belongsTo");
    let PhpLiteral::Class { name } = &arguments[0] else {
        panic!("bound class literal")
    };
    assert_eq!(name.written, "Person");
    assert_eq!(name.qualified, "App\\Models\\User");
    assert_eq!(provenance.span.start_line, 7);
    let targets: Vec<_> = php
        .blade
        .iter()
        .filter(|reference| reference.provenance.rel_path == "resources/views/post.blade.php")
        .filter_map(|reference| reference.target.as_deref())
        .collect();
    assert_eq!(targets, ["layout", "components.card", "card"]);
    assert!(php.blade.iter().any(
        |reference| reference.kind == BladeReferenceKind::Include && reference.target.is_none()
    ));
    for node in snapshot
        .nodes
        .iter()
        .filter(|node| node.provenance.rel_path.ends_with(".blade.php"))
    {
        assert_eq!(node.kind, NodeKind::File);
    }
    assert!(
        snapshot
            .call_sites
            .iter()
            .all(|site| !site.rel_path.ends_with(".blade.php"))
    );
    assert!(
        snapshot
            .file_measures
            .iter()
            .all(|measure| !measure.file_id.ends_with(".blade.php"))
    );
    codemap::validate::validate(&snapshot).unwrap();
}

#[test]
fn php_observation_validator_refuses_foreign_payloads_and_invalid_source_identity() {
    let root = fixture();
    let mut snapshot = extract(&ExtractOptions {
        path: root.clone(),
        language: Language::Php,
        ..ExtractOptions::default()
    })
    .unwrap();
    let mut foreign = extract(&ExtractOptions {
        path: root,
        language: Language::Rust,
        ..ExtractOptions::default()
    })
    .unwrap();
    foreign.php = snapshot.php.clone();
    assert!(
        codemap::validate::validate(&foreign)
            .unwrap_err()
            .to_string()
            .contains("PHP observations on a non-PHP snapshot")
    );
    snapshot.php.as_mut().unwrap().methods[0].node = "php:method:missing.php#missing".into();
    assert!(
        codemap::validate::validate(&snapshot)
            .unwrap_err()
            .to_string()
            .contains("dangling PHP method")
    );
    let mut snapshot = extract(&ExtractOptions {
        path: fixture(),
        language: Language::Php,
        ..ExtractOptions::default()
    })
    .unwrap();
    snapshot.php.as_mut().unwrap().blade[0]
        .provenance
        .span
        .end_byte = usize::MAX;
    assert!(
        codemap::validate::validate(&snapshot)
            .unwrap_err()
            .to_string()
            .contains("invalid or out-of-file span")
    );
    let mut snapshot = extract(&ExtractOptions {
        path: fixture(),
        language: Language::Php,
        ..ExtractOptions::default()
    })
    .unwrap();
    snapshot.identity.tools.blade_parser = "unreviewed".into();
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    assert!(
        codemap::validate::validate(&snapshot)
            .unwrap_err()
            .to_string()
            .contains("pinned Blade parser identity")
    );
}
