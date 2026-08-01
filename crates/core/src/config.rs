//! Configuration — env-driven, with an optional optimized policy overlay.

use serde::{Deserialize, Serialize};

use crate::domain_speed::DomainSpeedTable;
use crate::optimizer_policy::Policy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // --- required secrets ---
    pub discord_token: String,
    pub tinyfish_api_key: String,
    pub exa_api_key: String,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,

    // --- gates ---
    pub analyze_channels: Vec<String>, // empty = ALLOW_ALL
    pub allow_all_channels: bool,
    pub cooldown_secs: i64,
    pub cache_ttl_hours: i64,

    // --- loop policy (env wins over optimized_policy.json) ---
    pub policy: Policy,

    pub corpus_token_budget: usize,
    pub reply_mode: ReplyMode,

    // --- domain speed table override (DOMAIN_SPEED_JSON) ---
    pub domain_speed: DomainSpeedTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyMode {
    Thread,
    Split,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            discord_token: String::new(),
            tinyfish_api_key: String::new(),
            exa_api_key: String::new(),
            llm_api_base: "https://ollama.com/v1".into(),
            llm_api_key: String::new(),
            llm_model: "deepseek-v4-flash:0731".into(),
            analyze_channels: vec![],
            allow_all_channels: true,
            cooldown_secs: 60,
            cache_ttl_hours: 24,
            policy: Policy::default(),
            corpus_token_budget: 60_000,
            reply_mode: ReplyMode::Thread,
            domain_speed: DomainSpeedTable::default(),
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let get = |k: &str| std::env::var(k).unwrap_or_default();
        let analyze_raw = get("ANALYZE_CHANNELS");
        let allow_all = analyze_raw.trim() == "*" || analyze_raw.is_empty();
        let channels: Vec<String> = if allow_all {
            vec![]
        } else {
            analyze_raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };

        let mut cfg = Config {
            discord_token: get("DISCORD_TOKEN"),
            tinyfish_api_key: get("TINYFISH_API_KEY"),
            exa_api_key: get("EXA_API_KEY"),
            llm_api_base: get("LLM_API_BASE"),
            llm_api_key: get("LLM_API_KEY"),
            llm_model: get("LLM_MODEL"),
            allow_all_channels: allow_all,
            analyze_channels: channels,
            cooldown_secs: parse_i64("COOLDOWN_SECS", 60),
            cache_ttl_hours: parse_i64("CACHE_TTL_HOURS", 24),
            policy: Policy::from_env(),
            corpus_token_budget: parse_usize("CORPUS_TOKEN_BUDGET", 60_000),
            reply_mode: match get("REPLY_MODE").as_str() {
                "split" => ReplyMode::Split,
                _ => ReplyMode::Thread,
            },
            domain_speed: DomainSpeedTable::default(),
        };

        if let Ok(json) = std::env::var("DOMAIN_SPEED_JSON") {
            if !json.trim().is_empty() {
                let t = serde_json::from_str::<DomainSpeedTable>(&json)
                    .map_err(|e| format!("DOMAIN_SPEED_JSON invalid: {e}"))?;
                cfg.domain_speed = t;
            }
        }
        Ok(cfg)
    }

    pub fn channel_allowed(&self, channel_id: &str) -> bool {
        self.allow_all_channels || self.analyze_channels.iter().any(|c| c == channel_id)
    }
}

fn parse_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn parse_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parse_env_overrides() {
        std::env::set_var("COOLDOWN_SECS", "120");
        std::env::set_var("REPLY_MODE", "split");
        let c = Config::from_env().unwrap();
        assert_eq!(c.cooldown_secs, 120);
        assert_eq!(c.reply_mode, ReplyMode::Split);
        std::env::remove_var("COOLDOWN_SECS");
        std::env::remove_var("REPLY_MODE");
    }

    #[test]
    fn channel_gate_allow_all() {
        let c = Config::default();
        assert!(c.channel_allowed("anything"));
    }
}
