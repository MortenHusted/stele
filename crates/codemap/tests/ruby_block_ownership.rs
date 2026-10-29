//! Unknown call blocks preserve lexical declarations, not runtime method owners.
use std::collections::BTreeSet;

use codemap::schema::{EdgeKind, Language};
use codemap::{ExtractOptions, extract};

mod common;

#[test]
#[ignore = "requires the pinned Ruby-LSP operator bundle"]
fn block_owned_methods_and_explicit_rebinding_cannot_prove_calls() {
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
            (
                "Driver::accepted_yielding_block".into(),
                "Driver::helper".into()
            ),
            (
                "Driver::accepted_yielded_response".into(),
                "Driver::helper".into()
            ),
            (
                "Driver::accepted_yielded_response".into(),
                "YieldSource::self.perform".into()
            ),
            (
                "EvalDeclared::accepted".into(),
                "EvalDeclared::helper".into()
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

#[test]
fn block_mixins_have_no_verified_implicit_receiver_even_with_reopened_binding() {
    let repo = common::make_repo_with(&[
        ("first.rb".into(), "module Feature; end\nclass ReopenedEval; end\nclass ActualReceiver; end\n".into()),
        ("second.rb".into(), "class ReopenedEval\n  ActualReceiver.class_eval do\n    include Feature\n    self.prepend Feature\n    extend Feature\n    class ExplicitReceiver\n      include Feature\n    end\n  end\nend\nmodule OrdinaryYield\n  def self.scope; yield; end\n  scope do\n    include Feature\n  end\nend\n".into()),
    ]).unwrap();
    let snapshot = extract(&ExtractOptions {
        path: repo,
        language: Language::Ruby,
        calls_required: false,
        measures: false,
        twins: false,
    })
    .unwrap();
    let heritage: Vec<_> = snapshot
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Implements | EdgeKind::Extends))
        .collect();
    assert_eq!(
        heritage.len(),
        1,
        "unmodeled block mixins cannot use their lexical scope as runtime receiver"
    );
    assert!(
        heritage[0]
            .from
            .ends_with("#ReopenedEval::ExplicitReceiver")
    );
    assert!(heritage[0].to.ends_with("#Feature"));
}
