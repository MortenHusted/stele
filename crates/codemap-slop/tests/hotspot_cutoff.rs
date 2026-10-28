//! The `hotspots` checker in `codemap-lemmaspec` cuts at the same
//! complexity this crate erodes at. Read from the checker text so neither
//! consumer crate depends on the other.

use codemap_slop::policy::HIGH_CC_CUTOFF;

const HOTSPOTS_CHECKER: &str = include_str!("../../codemap-lemmaspec/checkers/hotspots.stele");

#[test]
fn the_hotspots_checker_cuts_where_erosion_does() {
    assert!(
        HOTSPOTS_CHECKER.contains(&format!("\"Cc > {HIGH_CC_CUTOFF}\"")),
        "checkers/hotspots.stele must compare against {HIGH_CC_CUTOFF}"
    );
}
