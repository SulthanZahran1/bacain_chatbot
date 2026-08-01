//! The search-loop policy. Env vars win; otherwise `optimized_policy.json`
//! (from `cargo run --bin optimize`) is loaded at startup.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Policy {
    pub initial_k: usize,
    pub expansion_k: usize,
    pub coverage_target: f64,
    pub min_new_articles: usize,
    pub max_rounds: usize,
    pub search_budget: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            initial_k: 5,
            expansion_k: 3,
            coverage_target: 0.85,
            min_new_articles: 1,
            max_rounds: 3,
            search_budget: 15,
        }
    }
}

impl Policy {
    pub fn from_env() -> Self {
        let mut p = Policy::default();
        if let Some(v) = env_usize("INITIAL_K") {
            p.initial_k = v;
        }
        if let Some(v) = env_usize("EXPANSION_K") {
            p.expansion_k = v;
        }
        if let Some(v) = env_f64("COVERAGE_TARGET") {
            p.coverage_target = v;
        }
        if let Some(v) = env_usize("MIN_NEW_ARTICLES") {
            p.min_new_articles = v;
        }
        if let Some(v) = env_usize("MAX_ROUNDS") {
            p.max_rounds = v;
        }
        if let Some(v) = env_usize("SEARCH_BUDGET") {
            p.search_budget = v;
        }
        p
    }

    /// Load `optimized_policy.json` if present; env overrides still win.
    pub fn load_with_env_override(path: Option<&str>) -> Self {
        let mut p = Policy::default();
        if let Some(path) = path {
            if let Ok(raw) = std::fs::read_to_string(path) {
                if let Ok(loaded) = serde_json::from_str::<Policy>(&raw) {
                    p = loaded;
                }
            }
        }
        // env wins
        let env_p = Policy::from_env();
        if env_any_set(&[
            "INITIAL_K",
            "EXPANSION_K",
            "COVERAGE_TARGET",
            "MIN_NEW_ARTICLES",
            "MAX_ROUNDS",
            "SEARCH_BUDGET",
        ]) {
            p = env_p;
        }
        p
    }
}

fn env_any_set(keys: &[&str]) -> bool {
    keys.iter().any(|k| std::env::var(k).is_ok())
}
fn env_usize(k: &str) -> Option<usize> {
    std::env::var(k).ok().and_then(|v| v.parse().ok())
}
fn env_f64(k: &str) -> Option<f64> {
    std::env::var(k).ok().and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_over_default() {
        std::env::set_var("SEARCH_BUDGET", "22");
        let p = Policy::from_env();
        assert_eq!(p.search_budget, 22);
        assert_eq!(p.initial_k, 5); // untouched
        std::env::remove_var("SEARCH_BUDGET");
    }

    #[test]
    fn policy_json_roundtrip() {
        let p = Policy {
            initial_k: 7,
            expansion_k: 5,
            coverage_target: 0.9,
            min_new_articles: 2,
            max_rounds: 4,
            search_budget: 20,
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: Policy = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }
}
