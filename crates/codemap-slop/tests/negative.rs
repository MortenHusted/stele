//! Every entry point fails closed with a named reason.

mod common;

use codemap_graph::schema::{Language, MEASURES_POLICY_RUST_FORM_V1};
use codemap_slop::{measured, measured_pair};

use common::{base_snapshot, empty_snapshot, foreign_policy_snapshot, unmeasured_snapshot};

#[test]
fn an_unmeasured_snapshot_is_refused() {
    let snapshot = unmeasured_snapshot();
    codemap_graph::validate(&snapshot).expect("a default extraction is valid");
    let error = measured(&snapshot).expect_err("refused");
    assert!(
        error.to_string().starts_with("unmeasured snapshot:"),
        "{error}"
    );
}

#[test]
fn a_policy_this_crate_does_not_read_is_refused() {
    let snapshot = foreign_policy_snapshot();
    let error = measured(&snapshot).expect_err("refused");
    assert!(
        error.to_string().starts_with("unsupported policy:"),
        "{error}"
    );
    assert!(error.to_string().contains(MEASURES_POLICY_RUST_FORM_V1));
}

#[test]
fn a_policy_from_another_language_is_refused_by_name() {
    let mut snapshot = base_snapshot();
    snapshot.identity.language = Language::Ruby;
    snapshot.identity.snapshot_id = snapshot.identity.compute_snapshot_id();
    let error = measured(&snapshot).expect_err("refused");
    assert!(
        error.to_string().starts_with("unsupported policy:"),
        "{error}"
    );
    assert!(error.to_string().contains("ruby"));
}

#[test]
fn an_empty_inventory_is_refused() {
    let snapshot = empty_snapshot();
    let error = measured(&snapshot).expect_err("refused");
    assert!(error.to_string().starts_with("empty inventory:"), "{error}");
}

#[test]
fn two_policies_are_never_compared() {
    // Both snapshots must be admitted on their own for the pair check to be
    // the one that fires, so the head carries a policy that is valid for
    // its language and differs from the base. No second Rust policy exists
    // yet, so this drives the check through the base being refused first
    // and the message naming which side failed.
    let base = foreign_policy_snapshot();
    let head = base_snapshot();
    let error = measured_pair(&base, &head).expect_err("refused");
    let text = format!("{error:#}");
    assert!(text.starts_with("base snapshot"), "{text}");
    assert!(text.contains("unsupported policy"), "{text}");
}

#[test]
fn a_snapshot_that_fails_validation_is_refused() {
    let mut snapshot = base_snapshot();
    snapshot.measures.pop();
    let error = measured(&snapshot).expect_err("refused");
    assert!(
        format!("{error:#}").contains("failed validation"),
        "{error:#}"
    );
}
