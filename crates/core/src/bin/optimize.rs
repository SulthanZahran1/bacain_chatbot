//! Offline policy optimizer CLI (§5 Stage 9).
//!
//! Thin wrapper over `linkbot_core::optimizer::sweep`: builds the 972-cell
//! grid, runs it against the synthetic scenarios, prints the top-10 table,
//! and writes the winner to `optimized_policy.json`. Never spends API
//! credits. All logic lives in the library (unit-tested).

use linkbot_core::optimizer::{build_grid, load_scenarios, sweep, write_winner};

const SCENARIOS_DIR: &str = "crates/core/scenarios";
const OUTPUT: &str = "optimized_policy.json";

fn main() {
    let scenarios = load_scenarios(SCENARIOS_DIR);
    println!("loaded {} scenarios", scenarios.len());

    let grid = build_grid();
    println!(
        "sweeping {} policy cells × {} scenarios",
        grid.len(),
        scenarios.len()
    );

    let (results, rejected) = sweep(&scenarios, &grid);
    println!(
        "cells passing hard constraint: {} (rejected {})",
        results.len(),
        rejected
    );
    for (i, r) in results.iter().take(10).enumerate() {
        println!(
            "#{i} score={:.1} angles={} wasted={} overshoot={} policy={:?}",
            r.score, r.angles, r.wasted, r.overshoot, r.policy
        );
    }

    if results.is_empty() {
        eprintln!("no policy passed the hard constraint — scenarios too strict");
        std::process::exit(1);
    }

    let winner = &results[0];
    write_winner(winner, OUTPUT).expect("write optimized_policy.json");
    println!(
        "winner: {:?} (score {:.1}) → {OUTPUT}",
        winner.policy, winner.score
    );
    println!("verify with: cargo test scenario_suite");
}
