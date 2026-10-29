//! Direct construction has a receiver type only under the default Class#new provider.
use std::collections::BTreeSet;

use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn constructor_chains_require_default_new_and_unambiguous_instance_targets() {
    let cases = [
        (
            include_str!("fixtures/ruby_constructors/constructors.rb"),
            BTreeSet::from([
                ("Driver::direct".into(), "Widget::perform".into()),
                ("Driver::inherited".into(), "Widget::perform".into()),
                (
                    "Driver::instance_named_new".into(),
                    "InstanceNew::perform".into(),
                ),
                ("Driver::mixed".into(), "Feature::act".into()),
            ]),
        ),
        (
            include_str!("fixtures/ruby_constructors/unknown_mutation.rb"),
            BTreeSet::new(),
        ),
        (
            include_str!("fixtures/ruby_constructors/instance_mutation.rb"),
            BTreeSet::from([("Driver::unaffected".into(), "Widget::act".into())]),
        ),
    ];
    for (source, expected) in cases {
        let repo = common::make_repo_with(&[
            ("constructors.rb".into(), source.into()),
            (
                "reopened.rb".into(),
                "class Reopened; def perform; 2; end; end\n".into(),
            ),
        ])
        .unwrap();
        let snapshot = extract(&ExtractOptions {
            path: repo,
            language: Language::Ruby,
            calls_required: true,
            measures: false,
            twins: false,
        })
        .unwrap();
        let symbol = |id: &str| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.id == id)
                .unwrap()
                .symbol_key
                .split_once('#')
                .unwrap()
                .1
                .to_string()
        };
        let calls: BTreeSet<_> = snapshot
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::Calls
                    && symbol(&edge.from).starts_with("Driver::")
                    && (symbol(&edge.to).ends_with("::perform")
                        || symbol(&edge.to).ends_with("::act"))
            })
            .map(|edge| (symbol(&edge.from), symbol(&edge.to)))
            .collect();
        assert_eq!(calls, expected);
    }
}
