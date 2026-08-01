//! Offline policy optimizer core (§5 Stage 9) — the grid sweep as a
//! testable library function. `bin/optimize.rs` is a thin CLI over this.
//!
//! Utility: `score = α·angles_over_min − β·wasted − γ·overshoot`
//! (α=1.0, β=0.25, γ=0.5). Cells that miss `min_angles_covered` on ANY
//! scenario are rejected. Never spends API credits.

use crate::optimizer_policy::Policy;
use crate::scenario::{run_scenario, Scenario};

/// One evaluated policy cell.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepResult {
    pub policy: Policy,
    pub score: f64,
    pub angles: usize,
    pub wasted: usize,
    pub overshoot: usize,
}

/// The full policy grid from the spec: 3×3×4×3×3×3 = 972 cells.
pub fn build_grid() -> Vec<Policy> {
    let mut grid: Vec<Policy> = Vec::with_capacity(972);
    for initial_k in [3usize, 5, 7] {
        for expansion_k in [2usize, 3, 5] {
            for coverage_target in [0.7_f64, 0.8, 0.85, 0.9] {
                for min_new in [0usize, 1, 2] {
                    for max_rounds in [2usize, 3, 4] {
                        for budget in [10usize, 15, 20] {
                            grid.push(Policy {
                                initial_k,
                                expansion_k,
                                coverage_target,
                                min_new_articles: min_new,
                                max_rounds,
                                search_budget: budget,
                            });
                        }
                    }
                }
            }
        }
    }
    grid
}

/// Evaluate every cell against the scenarios.
/// Returns (ranked results, number of rejected cells).
pub fn sweep(scenarios: &[Scenario], grid: &[Policy]) -> (Vec<SweepResult>, usize) {
    let mut results: Vec<SweepResult> = Vec::new();
    let mut rejected = 0usize;
    for p in grid {
        let mut total_score = 0.0_f64;
        let mut total_angles = 0usize;
        let mut total_wasted = 0usize;
        let mut total_overshoot = 0usize;
        let mut ok = true;
        for s in scenarios {
            let r = run_scenario(s, *p);
            if r.angles_covered < r.angles_expected {
                ok = false;
                rejected += 1;
                break;
            }
            total_angles += r.angles_covered;
            total_wasted += r.wasted_fetches;
            total_overshoot += r.corpus_size.saturating_sub(p.search_budget);
            let angles_over_min = r.angles_covered.saturating_sub(r.angles_expected);
            total_score += angles_over_min as f64 * 1.0
                - r.wasted_fetches as f64 * 0.25
                - r.corpus_size.saturating_sub(p.search_budget) as f64 * 0.5;
        }
        if ok {
            results.push(SweepResult {
                policy: *p,
                score: total_score,
                angles: total_angles,
                wasted: total_wasted,
                overshoot: total_overshoot,
            });
        }
    }
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    (results, rejected)
}

/// Serialize the winning policy to JSON at `output` (repo-root default).
pub fn write_winner(result: &SweepResult, output: &str) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(&result.policy).expect("serialize policy");
    std::fs::write(output, json)
}

/// Load scenarios; empty dir → error message for the CLI.
pub fn load_scenarios(dir: &str) -> Vec<Scenario> {
    let scenarios = Scenario::load_all(dir);
    if scenarios.is_empty() {
        eprintln!("no scenarios found in {dir}");
        std::process::exit(2);
    }
    scenarios
}
