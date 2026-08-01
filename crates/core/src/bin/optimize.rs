//! Offline policy optimizer (§5 Stage 9).
//!
//! Sweeps the policy grid against the ~100 synthetic scenarios (mocked
//! search + scripted LLM), scores each cell with the utility function
//! `score = α·angles − β·wasted − γ·overshoot` (α=1.0, β=0.25, γ=0.5),
//! rejects cells that miss `min_angles_covered`, and writes the winner to
//! `optimized_policy.json`. Never spends API credits.

use std::path::PathBuf;

use linkbot_core::optimizer_policy::Policy;
use linkbot_core::scenario::{run_scenario, Scenario};

const SCENARIOS_DIR: &str = "crates/core/scenarios";
const OUTPUT: &str = "optimized_policy.json";

fn main() {
    let scenarios = Scenario::load_all(SCENARIOS_DIR);
    if scenarios.is_empty() {
        eprintln!("no scenarios found in {SCENARIOS_DIR}");
        std::process::exit(2);
    }
    println!("loaded {} scenarios", scenarios.len());

    // Policy grid from the spec.
    let mut grid: Vec<Policy> = Vec::new();
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
    println!(
        "sweeping {} policy cells × {} scenarios",
        grid.len(),
        scenarios.len()
    );

    // α=1.0 per angle over the minimum, β=0.25 per wasted fetch,
    // γ=0.5 per fetch past the budget. Missed min_angles → reject.
    let mut results: Vec<(Policy, f64, usize, usize, usize)> = Vec::new(); // (policy, score, angles, wasted, overshoot)
    let mut rejected = 0usize;
    for p in &grid {
        let mut total_score = 0.0_f64;
        let mut total_angles = 0usize;
        let mut total_wasted = 0usize;
        let mut total_overshoot = 0usize;
        let mut ok = true;
        for s in &scenarios {
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
            results.push((*p, total_score, total_angles, total_wasted, total_overshoot));
        }
    }
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "cells passing hard constraint: {} (rejected {})",
        results.len(),
        rejected
    );
    for (i, (p, score, angles, wasted, over)) in results.iter().take(10).enumerate() {
        println!(
            "#{i} score={score:.1} angles={angles} wasted={wasted} overshoot={over} policy={p:?}"
        );
    }

    if results.is_empty() {
        eprintln!("no policy passed the hard constraint — scenarios too strict");
        std::process::exit(1);
    }

    let (winner, score, _, _, _) = &results[0];
    let json = serde_json::to_string_pretty(winner).expect("serialize policy");
    std::fs::write(OUTPUT, json).expect("write optimized_policy.json");
    println!("winner: {winner:?} (score {score:.1}) → {OUTPUT}");
    println!("verify with: cargo test scenario_suite");
    let _ = PathBuf::new();
}
