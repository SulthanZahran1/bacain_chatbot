//! Scenario suite regression gate (§13, §15 DoD).
//!
//! Runs all ~100 synthetic scenarios through the REAL pipeline loop with
//! mocked search/fetch and a scripted LLM, using the shipped policy from
//! `optimized_policy.json` (or the default when absent). A policy change
//! that fails scenarios fails CI.

use linkbot_core::optimizer_policy::Policy;
use linkbot_core::scenario::{run_suite, Scenario};

const SCENARIOS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios");

#[test]
fn scenario_suite_passes_with_shipped_policy() {
    let scenarios = Scenario::load_all(SCENARIOS_DIR);
    assert!(
        scenarios.len() >= 90,
        "expected ~100 scenarios, found {}",
        scenarios.len()
    );

    // Load shipped policy if present; env overrides win; otherwise default.
    let policy_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../optimized_policy.json");
    let policy =
        Policy::load_with_env_override(std::fs::metadata(policy_path).map(|_| policy_path).ok());

    let (passed, total) = run_suite(&scenarios, policy);
    assert!(
        passed == total,
        "scenario suite: {passed}/{total} passed — policy regressed"
    );
}

#[test]
fn scenario_suite_has_valid_json() {
    let scenarios = Scenario::load_all(SCENARIOS_DIR);
    let ids: std::collections::HashSet<String> = scenarios.iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), scenarios.len(), "duplicate scenario ids");
    for s in &scenarios {
        assert!(!s.corpus.is_empty(), "{}: empty corpus", s.id);
        assert!(
            s.expected.min_angles_covered <= s.unique_angles().len(),
            "{}: min_angles_covered {} > available angles {}",
            s.id,
            s.expected.min_angles_covered,
            s.unique_angles().len()
        );
    }
}
