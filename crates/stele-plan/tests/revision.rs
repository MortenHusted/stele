//! A decision follows the wording the person saw through later implementation edits.
use std::collections::BTreeMap;

use stele_plan::{
    load,
    revision::{ProductRevision, product_revision},
};

const CATALOG: &str = r#"catalog inventory {
    intent owns_stock { text: "Stores know what they hold." }
    definition tracks_stock { refines: owns_stock text: "Count each item." }
    channel shop { text: "In a shop." }
    exception hold { narrows: tracks_stock channels: [shop] kind: holds_instead text: "Count reserved items separately." }
    intent unrelated { text: "Other purpose." }
}"#;
const PLAN: &str = r#"plan show_stock {
    use "../intent/catalog.stele" as cat
    exception reservation {
        replaces: cat.hold narrows: cat.tracks_stock channels: [cat.shop] kind: holds_instead
        text: "Show the reserved count." examples: ["One reservation shows one."]
    }
    decision display_stock {
        scope: product governs: [reservation] text: "Display both counts."
        rejected: "Showing only available stock conceals reservations."
        examples: ["With zero available, reserved stock remains visible."]
    }
    decision reuse { governs: [cat.tracks_stock] text: "Reuse the inventory controller." }
    change paint { serves: [reservation] text: "Update template." touches: ["view.html"] }
}"#;

fn revision(catalog: &str, source: &str) -> ProductRevision {
    let files = BTreeMap::from([
        ("intent/catalog.stele".into(), catalog.into()),
        ("plans/change.stele".into(), source.into()),
    ]);
    product_revision(&load(&files, "plans/change.stele").unwrap()).unwrap()
}

#[test]
fn requester_sees_the_exact_behavior_being_confirmed() {
    let original = revision(CATALOG, PLAN);
    for text in [
        "Stores know what they hold.",
        "Count each item.",
        "In a shop.",
        "Count reserved items separately.",
        "Replaces: hold",
        "Show the reserved count.",
        "One reservation shows one.",
        "Display both counts.",
        "Showing only available stock conceals reservations.",
        "With zero available, reserved stock remains visible.",
    ] {
        assert!(original.wording.contains(text), "missing {text}");
    }
    assert!(!original.wording.contains("Reuse the inventory controller."));
    assert!(!original.wording.contains("Update template."));
    assert!(!original.wording.contains("Other purpose."));
    let receipt = original
        .confirm(&original.digest, "requester-12", "decision/951")
        .unwrap();
    for (catalog, plan) in [
        (
            CATALOG.to_owned(),
            PLAN.replace("view.html", "new_view.html"),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("Update template.", "Share a helper."),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("Reuse the inventory controller.", "Split the controller."),
        ),
        (
            CATALOG.replace("Other purpose.", "Unrelated change."),
            PLAN.to_owned(),
        ),
    ] {
        revision(&catalog, &plan).verify(&receipt).unwrap();
    }
    for (catalog, plan) in [
        (
            CATALOG.replace("what they hold", "what they need"),
            PLAN.to_owned(),
        ),
        (CATALOG.replace("In a shop.", "Online."), PLAN.to_owned()),
        (
            CATALOG.to_owned(),
            PLAN.replace("holds_instead", "does_not_apply"),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("Show the reserved count.", "Hide the reserved count."),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("One reservation shows one.", "Reservations count twice."),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("Display both counts.", "Show one count."),
        ),
        (
            CATALOG.to_owned(),
            PLAN.replace("remains visible", "is hidden"),
        ),
    ] {
        let changed = revision(&catalog, &plan);
        assert!(changed.verify(&receipt).is_err());
        assert!(
            changed
                .confirm(&original.digest, "requester-12", "decision/951")
                .is_err()
        );
    }
}

#[test]
fn confirmation_needs_a_source_an_actor_and_wording() {
    let original = revision(CATALOG, PLAN);
    for (actor, source) in [
        (" ", "decision/951"),
        ("requester-12", ""),
        ("a\nb", "example"),
    ] {
        assert!(original.confirm(&original.digest, actor, source).is_err());
    }
    let receipt = original
        .confirm(&original.digest, "requester-12", "decision/951")
        .unwrap();
    assert!(
        original
            .verify(&receipt.replace("decision/951", ""))
            .is_err()
    );
    assert!(
        original
            .verify(&receipt.replace("confirmation/1", "confirmation/2"))
            .is_err()
    );
    let empty = revision(CATALOG, "plan discovery {}");
    assert!(
        empty
            .confirm(&empty.digest, "requester-12", "decision/951")
            .is_err()
    );
}
