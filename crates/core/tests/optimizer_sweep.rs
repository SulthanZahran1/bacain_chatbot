//! Optimizer sweep tests (§5 Stage 9) — the grid builder, the sweep's
//! utility math, rejection semantics, and the winner serialization.

use linkbot_core::optimizer::{build_grid, sweep, write_winner, SweepResult};
use linkbot_core::optimizer_policy::Policy;
use linkbot_core::scenario::{
    Scenario, ScenarioArticle, ScenarioExpectation, ScenarioOverrides, ScenarioSource,
};

/// Dense 12-article, 3-angle corpus so policies can actually differ.
fn mini_scenario(id: &str, min_angles: usize) -> Scenario {
    Scenario {
        id: id.into(),
        source: ScenarioSource {
            url: "https://unknown.example/story/1".into(),
            domain_bucket: "default".into(),
            is_ai_topic: false,
            ground_truth_angles: 3,
            title: "T".into(),
        },
        corpus: (0..12)
            .map(|i| ScenarioArticle {
                url: format!("https://news-{i}.example/a{i}"),
                angle: ["mechanism", "market", "history"][i % 3].into(),
                relevance: 0.9 - (i as f64) * 0.02,
                fetchable: true,
                published_date: Some("2026-07-30".into()),
                title: format!("A{i}"),
                snippet: "s".into(),
            })
            .collect(),
        expected: ScenarioExpectation {
            min_angles_covered: min_angles,
            max_wasted_fetches: 2,
        },
        overrides: ScenarioOverrides {
            seed_queries: vec!["mechanism".into()],
            ..Default::default()
        },
    }
}

#[test]
fn grid_has_972_cells() {
    let g = build_grid();
    assert_eq!(g.len(), 972, "3×3×4×3×3×3");
}

#[test]
fn grid_covers_policy_extremes() {
    let g = build_grid();
    let mins = g
        .iter()
        .map(|p| (p.initial_k, p.expansion_k, p.search_budget))
        .min()
        .unwrap();
    let maxs = g
        .iter()
        .map(|p| (p.initial_k, p.expansion_k, p.search_budget))
        .max()
        .unwrap();
    assert_eq!(mins, (3, 2, 10));
    assert_eq!(maxs, (7, 5, 20));
    assert!(g.iter().all(|p| (0.7..=0.9).contains(&p.coverage_target)));
}

#[test]
fn sweep_ranks_better_policies_first() {
    // Dead-heavy corpus: the aggressive cell wastes more fetches on dead
    // URLs, so the conservative cell must rank strictly higher.
    let mut scn = mini_scenario("s1", 1);
    for (i, a) in scn.corpus.iter_mut().enumerate() {
        if i >= 6 {
            a.fetchable = false;
        }
    }
    let scns = vec![scn];
    let grid = vec![
        Policy {
            initial_k: 7,
            expansion_k: 5,
            coverage_target: 0.9,
            min_new_articles: 0,
            max_rounds: 4,
            search_budget: 20,
        },
        Policy {
            initial_k: 3,
            expansion_k: 2,
            coverage_target: 0.7,
            min_new_articles: 0,
            max_rounds: 2,
            search_budget: 10,
        },
    ];
    let (results, rejected) = sweep(&scns, &grid);
    assert_eq!(results.len(), 2, "both cells pass");
    assert_eq!(rejected, 0);
    // Scores must be sorted descending AND differ (waste penalty bites).
    for w in results.windows(2) {
        assert!(w[0].score >= w[1].score, "descending scores");
    }
    assert!(
        results[0].score > results[1].score,
        "{} > {}",
        results[0].score,
        results[1].score
    );
    // The conservative cell (fewer dead fetches) must win.
    assert_eq!(
        results[0].policy.initial_k, 3,
        "conservative wins on dead-heavy corpus"
    );
}

#[test]
fn sweep_rejects_cells_missing_angles() {
    // min_angles=99 is unreachable → every cell rejected.
    let scns = vec![mini_scenario("s2", 99)];
    let grid = vec![Policy::default()];
    let (results, rejected) = sweep(&scns, &grid);
    assert!(results.is_empty());
    assert_eq!(rejected, 1);
}

#[test]
fn sweep_accumulates_angles_across_scenarios() {
    let scns = vec![mini_scenario("a", 1), mini_scenario("b", 1)];
    let (results, _) = sweep(&scns, &[Policy::default()]);
    let r = &results[0];
    assert!(
        r.angles >= 2,
        "two scenarios each contribute angles: {}",
        r.angles
    );
    assert!(r.score > 0.0, "positive utility: {}", r.score);
}

#[test]
fn sweep_utility_formula_is_exact() {
    // Pin the utility math precisely: score = angles_over_min*1.0
    // − wasted*0.25 − overshoot*0.5, where overshoot = corpus−budget (>0).
    use linkbot_core::scenario::run_scenario;
    let scn = mini_scenario("f", 1);
    let policy = Policy {
        initial_k: 5,
        expansion_k: 3,
        coverage_target: 0.85,
        min_new_articles: 1,
        max_rounds: 3,
        search_budget: 4,
    };
    let r = run_scenario(&scn, policy);
    let (results, _) = sweep(&[scn], &[policy]);
    let expected = (r.angles_covered as f64 - r.angles_expected as f64).max(0.0)
        - r.wasted_fetches as f64 * 0.25
        - (r.corpus_size as f64 - policy.search_budget as f64).max(0.0) * 0.5;
    assert!(
        (results[0].score - expected).abs() < 1e-9,
        "score {} != expected {} (angles={} wasted={} corpus={} budget={})",
        results[0].score,
        expected,
        r.angles_covered,
        r.wasted_fetches,
        r.corpus_size,
        policy.search_budget
    );
    // Overshoot component: corpus > budget must count the difference.
    assert_eq!(
        results[0].overshoot,
        r.corpus_size.saturating_sub(policy.search_budget)
    );
}

#[test]
fn write_winner_roundtrips_to_json() {
    let r = SweepResult {
        policy: Policy {
            initial_k: 5,
            expansion_k: 2,
            coverage_target: 0.9,
            min_new_articles: 0,
            max_rounds: 4,
            search_budget: 20,
        },
        score: 131.0,
        angles: 394,
        wasted: 56,
        overshoot: 0,
    };
    let dir = std::env::temp_dir().join("linkbot_opt_winner.json");
    write_winner(&r, dir.to_str().unwrap()).unwrap();
    let raw = std::fs::read_to_string(&dir).unwrap();
    let back: Policy = serde_json::from_str(&raw).unwrap();
    assert_eq!(back, r.policy);
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn sweep_empty_scenarios_yields_all_pass() {
    let (results, rejected) = sweep(&[], &[Policy::default()]);
    assert_eq!(results.len(), 1, "no scenarios → no constraint violations");
    assert_eq!(rejected, 0);
    assert_eq!(results[0].angles, 0);
    assert_eq!(results[0].score, 0.0);
}

#[test]
fn sweep_empty_grid_yields_nothing() {
    let scns = vec![mini_scenario("e", 1)];
    let (results, rejected) = sweep(&scns, &[]);
    assert!(results.is_empty());
    assert_eq!(rejected, 0);
}
