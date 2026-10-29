//! Unknown call blocks preserve lexical declarations, not runtime method owners.
use std::collections::BTreeSet;

use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn unknown_blocks_cannot_prove_method_ownership_or_self_context() {
    let repo = common::make_repo_with(&[(
        "ownership.rb".into(),
        include_str!("fixtures/ruby_block_ownership/ownership.rb").into(),
    )])
    .unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: true,
        measures: true,
        twins: false,
    })
    .unwrap();
    let symbol = |id: &str| id.split_once('#').map_or("", |(_, name)| name).to_string();
    let calls: BTreeSet<_> = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| (symbol(&edge.from), symbol(&edge.to)))
        .collect();
    assert_eq!(
        calls.len(),
        snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .count(),
        "every labelled call occurs exactly once"
    );
    assert_eq!(
        calls,
        BTreeSet::from([
            ("Plain::ordinary".into(), "Plain::helper".into()),
            ("PlainHost::accepted".into(), "Plain::ordinary".into()),
            ("Yielding::unverified".into(), "Utility::self.value".into()),
            (
                "Yielding::Explicit::accepted".into(),
                "Yielding::Explicit::helper".into()
            ),
            (
                "Driver::accepted".into(),
                "Yielding::Explicit::self.value".into()
            ),
            (
                "Driver::accepted_after_block".into(),
                "Driver::helper".into()
            ),
        ]),
        "block ownership and self context need proof independent of lexical nesting"
    );
    for name in [
        "Redirected::routed",
        "Yielding::unverified",
        "Yielding::self.unverified_singleton",
    ] {
        let node = snapshot
            .nodes
            .iter()
            .find(|node| symbol(&node.id) == name)
            .unwrap_or_else(|| panic!("retain lexical declaration {name}"));
        assert!(
            snapshot
                .measures
                .iter()
                .any(|measure| measure.node_id == node.id),
            "retain lexical method measures {name}"
        );
    }
}
