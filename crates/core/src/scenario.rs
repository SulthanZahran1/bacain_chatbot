//! Scenario schema + suite runner for the offline optimizer (§5 Stage 9).

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clock::FakeClock;
use crate::config::Config;
use crate::mock_providers::{AiAwareMockFetcher, MockSearchProvider, ScriptedLlm};
use crate::optimizer_policy::Policy;
use crate::pipeline::{analyze, AnalysisRequest, ChannelCtx, Deps};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioSource {
    pub url: String,
    pub domain_bucket: String,
    pub is_ai_topic: bool,
    pub ground_truth_angles: usize,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioArticle {
    pub url: String,
    pub angle: String,
    pub relevance: f64,
    pub fetchable: bool,
    pub published_date: Option<String>,
    pub title: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioExpectation {
    pub min_angles_covered: usize,
    pub max_wasted_fetches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScenarioOverrides {
    /// Round number → URLs to return regardless of relevance (adversarial cases).
    #[serde(default)]
    pub round_overrides: std::collections::HashMap<usize, Vec<String>>,
    /// Simulate a search rate limit after N calls.
    #[serde(default)]
    pub rate_limit_after: Option<usize>,
    /// Scripted LLM: coverage value per round (0-based).
    #[serde(default)]
    pub coverage_per_round: Vec<f64>,
    /// Scripted uncovered angles after each round.
    #[serde(default)]
    pub angles: Vec<String>,
    /// Scripted seed queries.
    #[serde(default)]
    pub seed_queries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub source: ScenarioSource,
    pub corpus: Vec<ScenarioArticle>,
    pub expected: ScenarioExpectation,
    #[serde(default)]
    pub overrides: ScenarioOverrides,
}

impl Scenario {
    /// Load all scenarios from a directory (`scenarios/*.json`).
    pub fn load_all(dir: &str) -> Vec<Scenario> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(raw) = std::fs::read_to_string(&p) {
                if let Ok(s) = serde_json::from_str::<Scenario>(&raw) {
                    out.push(s);
                }
            }
        }
        out
    }

    pub fn unique_angles(&self) -> HashSet<String> {
        self.corpus.iter().map(|a| a.angle.clone()).collect()
    }
}

// ---------------------------------------------------------------------------
// Suite runner — the regression gate
// ---------------------------------------------------------------------------

pub struct ScenarioResult {
    pub id: String,
    pub passed: bool,
    pub angles_covered: usize,
    pub angles_expected: usize,
    pub wasted_fetches: usize,
    pub wasted_max: usize,
    pub corpus_size: usize,
    pub rounds: usize,
    pub stop_reason: String,
    pub detail: String,
}

/// Run the REAL pipeline loop code against mocks with a scripted LLM.
pub fn run_scenario(scenario: &Scenario, policy: Policy) -> ScenarioResult {
    let fetcher = Arc::new(AiAwareMockFetcher::new(scenario));
    let searcher = Arc::new(MockSearchProvider::new(scenario));
    let llm = Arc::new(ScriptedLlm::new(
        scenario.overrides.coverage_per_round.clone(),
        scenario.overrides.angles.clone(),
        scenario.overrides.seed_queries.clone(),
        scenario.source.is_ai_topic,
        scenario.source.ground_truth_angles,
        &scenario.corpus,
    ));
    let clock: crate::clock::Clock = Arc::new(FakeClock::new(1_785_484_800));

    let config = Config {
        policy,
        ..Default::default()
    };
    let deps = Deps {
        fetcher: fetcher.clone(),
        searcher: searcher.clone(),
        llm,
        clock,
        config: Arc::new(config),
    };

    let req = AnalysisRequest {
        url: scenario.source.url.clone(),
        channel: ChannelCtx {
            id: "scenario".into(),
        },
    };

    let mut result = ScenarioResult {
        id: scenario.id.clone(),
        passed: false,
        angles_covered: 0,
        angles_expected: scenario.expected.min_angles_covered,
        wasted_fetches: 0,
        wasted_max: scenario.expected.max_wasted_fetches,
        corpus_size: 0,
        rounds: 0,
        stop_reason: String::new(),
        detail: String::new(),
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match runtime.block_on(analyze(req, &deps)) {
        Ok(a) => {
            result.corpus_size = a.meta.corpus_size;
            result.rounds = a.meta.rounds;
            result.stop_reason = a.meta.stop_reason.clone();

            // Distinct ground-truth angles covered by the final citations.
            let cited_urls: HashSet<String> = a.citations.iter().map(|c| c.url.clone()).collect();
            let unique_angles: HashSet<String> = scenario
                .corpus
                .iter()
                .filter(|art| cited_urls.contains(&art.url))
                .map(|art| art.angle.clone())
                .collect();
            result.angles_covered = unique_angles.len();

            // Wasted fetches = fetch ATTEMPTS that FAILED (bot-blocked/404).
            // Fetch count excludes the source article fetch.
            let total_fetches = fetcher.fetch_count();
            let fetchable_hits = scenario.corpus.iter().filter(|a| a.fetchable).count();
            let failed = total_fetches.saturating_sub(fetchable_hits + 1); // +1 source
            result.wasted_fetches = failed;

            result.passed = result.angles_covered >= result.angles_expected
                && result.wasted_fetches <= result.wasted_max;
            if !result.passed {
                result.detail = format!(
                    "angles {}/{} wasted {}/{} corpus {} rounds {} stop {}",
                    result.angles_covered,
                    result.angles_expected,
                    result.wasted_fetches,
                    result.wasted_max,
                    result.corpus_size,
                    result.rounds,
                    result.stop_reason
                );
            }
        }
        Err(e) => {
            result.detail = format!("pipeline error: {e}");
        }
    }
    result
}

/// Run the whole suite; returns (passed, total).
pub fn run_suite(scenarios: &[Scenario], policy: Policy) -> (usize, usize) {
    let mut passed = 0;
    for s in scenarios {
        let r = run_scenario(s, policy);
        if r.passed {
            passed += 1;
        } else {
            eprintln!("FAIL {}: {}", r.id, r.detail);
        }
    }
    (passed, scenarios.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_json_roundtrip() {
        let s = Scenario {
            id: "scn_001".into(),
            source: ScenarioSource {
                url: "https://x.example/1".into(),
                domain_bucket: "fast".into(),
                is_ai_topic: false,
                ground_truth_angles: 3,
                title: "T".into(),
            },
            corpus: vec![ScenarioArticle {
                url: "https://y.example/2".into(),
                angle: "mechanism".into(),
                relevance: 0.9,
                fetchable: true,
                published_date: Some("2026-07-30".into()),
                title: "Y".into(),
                snippet: "s".into(),
            }],
            expected: ScenarioExpectation {
                min_angles_covered: 1,
                max_wasted_fetches: 0,
            },
            overrides: ScenarioOverrides::default(),
        };
        let j = serde_json::to_string_pretty(&s).unwrap();
        let back: Scenario = serde_json::from_str(&j).unwrap();
        assert_eq!(back.id, "scn_001");
        assert_eq!(back.corpus[0].url, "https://y.example/2");
    }
}
