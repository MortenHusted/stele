//! The shipped intent checker proves its own rules on its fixture, and
//! decides minimal catalogs its fixture cannot hold.

use lemmaspec::{check_artifact, mutate_artifact, walk_artifact};
use stele_plan::INTENT_CHECKER;

#[test]
fn the_checker_proves_every_rule_and_condition_on_its_fixture() {
    let walk = walk_artifact(INTENT_CHECKER).unwrap();
    let unmet: Vec<_> = walk.expectations.iter().filter(|e| !e.satisfied).collect();
    assert_eq!(walk.status, "clean", "{unmet:#?}");

    let report = mutate_artifact(INTENT_CHECKER).unwrap();
    let survivors: Vec<_> = report
        .mutations
        .iter()
        .filter(|m| m.status.as_str() == "survived")
        .map(|m| &m.id)
        .collect();
    assert_eq!(report.status, "clean", "{survivors:#?}");
    assert_eq!(report.summary.survived, 0);
    assert!(report.summary.killed > 0);
    assert!(
        report.policies.iter().all(|p| p.summary.executed > 0),
        "{:#?}",
        report.policies
    );
}

// One catalog holds a single approved intent and nothing else, the other has
// no approved channel at all. The checker's fixture shares its approved
// channels across cases, so it can only scope them out.
const LONE_INTENT: &str = r#"
spec lone_intent {
  fact item_0 { relation: item args: [follow_up, intent] }
  fact current_digest_0 { relation: current_digest args: [follow_up, h_follow_up] }
  fact log_entry_0 { relation: log_entry args: [1, follow_up, intent, approve, h_follow_up] }
  fact target_identity_0 { relation: target_identity args: [head] }
  expect intent_is_unverified { query: "standing(follow_up, unverified)" count: 1 }
  expect intent_has_no_definition { query: "reason(follow_up, no_definition, follow_up)" count: 1 }
  expect nothing_awaits_approval { query: "awaiting_approval(I)" count: 0 }
}
"#;

const NO_CHANNEL: &str = r#"
spec no_channel {
  fact item_0 { relation: item args: [follow_up, intent] }
  fact item_1 { relation: item args: [next_task, definition] }
  fact current_digest_0 { relation: current_digest args: [follow_up, h_follow_up] }
  fact current_digest_1 { relation: current_digest args: [next_task, h_next_task] }
  fact refines_0 { relation: refines args: [next_task, follow_up] }
  fact log_entry_0 { relation: log_entry args: [1, follow_up, intent, approve, h_follow_up] }
  fact log_entry_1 { relation: log_entry args: [2, next_task, definition, approve, h_next_task] }
  fact log_link_0 { relation: log_link args: [2, follow_up, h_follow_up] }
  fact target_identity_0 { relation: target_identity args: [head] }
  expect definition_is_unverified { query: "standing(next_task, unverified)" count: 1 }
  expect definition_applies_on_no_channel { query: "reason(next_task, no_channel, next_task)" count: 1 }
  expect definition_has_one_reason { query: "reason(next_task, R, S)" count: 1 }
  expect intent_names_the_definition { query: "reason(follow_up, unverified_member, next_task)" count: 1 }
  expect nothing_awaits_approval { query: "awaiting_approval(I)" count: 0 }
}
"#;

#[test]
fn minimal_catalogs_report_unverified_standings() {
    for evidence in [LONE_INTENT, NO_CHANNEL] {
        let report = check_artifact(INTENT_CHECKER, evidence).unwrap();
        let unmet: Vec<_> = report
            .expectations
            .iter()
            .filter(|e| !e.satisfied)
            .collect();
        assert_eq!(report.status, "clean", "{}: {unmet:#?}", report.spec);
    }
}
