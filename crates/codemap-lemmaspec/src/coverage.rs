//! Admission to conditional absence checks over the observed graph.
//! This policy never establishes parser or runtime completeness.

use codemap_graph::schema::{CallSiteCategory, Language, Snapshot};

pub const POLICY: &str = "observed-call-coverage/v1";
pub const DEFAULT_MIN_PER_MILLE: u16 = 1000;
pub const CATEGORIES: [CallSiteCategory; 8] = [
    CallSiteCategory::Observed,
    CallSiteCategory::Resolved,
    CallSiteCategory::ExternalCrate,
    CallSiteCategory::StandardLibrary,
    CallSiteCategory::Ambiguous,
    CallSiteCategory::Unmapped,
    CallSiteCategory::Failed,
    CallSiteCategory::Candidate,
];

pub fn categories(language: Language) -> impl Iterator<Item = CallSiteCategory> {
    CATEGORIES.into_iter().chain(
        [
            CallSiteCategory::Unsupported,
            CallSiteCategory::Inactive,
            CallSiteCategory::Unprofiled,
        ]
        .into_iter()
        .filter(move |category| {
            language == Language::Cpp
                || (language == Language::JsTs && *category != CallSiteCategory::Inactive)
                || (language == Language::Python && *category == CallSiteCategory::Unsupported)
        }),
    )
}

/// Counts only recognized sites, not all possible runtime dispatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub resolved: usize,
    pub recognized: usize,
    /// Absent for an empty census; never interpreted as complete coverage.
    pub per_mille: Option<u16>,
    pub semantic: bool,
    pub parse_errors: usize,
    pub files_skipped: usize,
}

impl Coverage {
    /// Call after validating the snapshot. The report is checked against
    /// the actual call sites by the canonical validator.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let resolved = snapshot
            .report
            .call_sites_by_category
            .get("Resolved")
            .copied()
            .unwrap_or(0);
        let recognized = snapshot.call_sites.len();
        Self {
            resolved,
            recognized,
            per_mille: (recognized > 0)
                .then(|| ((resolved as u128 * 1000) / recognized as u128) as u16),
            semantic: snapshot.identity.is_semantic(),
            parse_errors: snapshot.report.parse_errors,
            files_skipped: snapshot.report.files_skipped,
        }
    }

    pub fn admitted(&self, minimum: u16) -> bool {
        minimum <= 1000
            && self.semantic
            && self.parse_errors == 0
            && self.files_skipped == 0
            && self.per_mille.is_some_and(|ratio| ratio >= minimum)
    }
}
