//! Cache, clock, policy, and config tests — the stateful/serialization layer
//! the pipeline depends on. SQLite via bundled rusqlite (in-memory).
//!
//! Env-dependent tests set ALL policy vars explicitly to stay deterministic
//! under parallel test threads (env is process-global).

use std::sync::Arc;

use linkbot_core::cache::Cache;
use linkbot_core::clock::{Clock, FakeClock, Now, SystemClock};
use linkbot_core::config::{Config, ReplyMode};
use linkbot_core::error::PipelineError;
use linkbot_core::optimizer_policy::Policy;

// ---------------------------------------------------------------------------
// Cache (§5 Stage 8)
// ---------------------------------------------------------------------------

#[test]
fn cache_roundtrip_analysis() {
    let c = Cache::in_memory().unwrap();
    let now = 1_785_484_800;
    c.put(
        "https://example.com/a",
        "ch1",
        r#"{"summary":"s","meta":{"window_used":"30d","bucket":"default"}}"#,
        "30d",
        "default",
        now,
    )
    .unwrap();
    let got = c.get("https://example.com/a").unwrap().expect("cached");
    assert!(got.analysis_json.contains("\"summary\":\"s\""));
    assert_eq!(got.window_used, "30d");
    assert_eq!(got.bucket, "default");
    assert_eq!(got.created_at, now);
}

#[test]
fn cache_get_unknown_returns_none() {
    let c = Cache::in_memory().unwrap();
    assert!(c.get("https://nope.example/x").unwrap().is_none());
}

#[test]
fn cache_cooldown_roundtrip() {
    let c = Cache::in_memory().unwrap();
    assert_eq!(c.last_analysis_at("ch9").unwrap(), None);
    c.set_last_analysis_at("ch9", 42).unwrap();
    assert_eq!(c.last_analysis_at("ch9").unwrap(), Some(42));
}

#[test]
fn cache_config_persistence() {
    let c = Cache::in_memory().unwrap();
    assert_eq!(c.get_config("reply_mode").unwrap(), None);
    c.set_config("reply_mode", "split").unwrap();
    assert_eq!(c.get_config("reply_mode").unwrap(), Some("split".into()));
    let all = c.all_config().unwrap();
    assert!(all.contains(&("reply_mode".into(), "split".into())));
}

#[test]
fn cache_recent_orders_by_recency() {
    let c = Cache::in_memory().unwrap();
    c.put("https://a.example/1", "ch", "{}", "30d", "default", 100)
        .unwrap();
    c.put("https://b.example/2", "ch", "{}", "30d", "default", 200)
        .unwrap();
    let recent = c.recent(10).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].url, "https://b.example/2", "newest first");
}

#[test]
fn cache_recent_limit() {
    let c = Cache::in_memory().unwrap();
    for i in 0..5 {
        c.put(&format!("https://x.example/{i}"), "ch", "{}", "30d", "default", i)
            .unwrap();
    }
    assert_eq!(c.recent(2).unwrap().len(), 2);
}

#[test]
fn cache_clones_share_connection() {
    let c = Cache::in_memory().unwrap();
    let c2 = c.clone();
    c.put("https://s.example/1", "ch", "{}", "30d", "default", 1)
        .unwrap();
    assert!(
        c2.get("https://s.example/1").unwrap().is_some(),
        "clone sees data"
    );
}

#[test]
fn cache_put_overwrites_same_url() {
    let c = Cache::in_memory().unwrap();
    c.put("https://s.example/1", "ch", r#"{"v":1}"#, "30d", "default", 1)
        .unwrap();
    c.put("https://s.example/1", "ch", r#"{"v":2}"#, "7d", "fast", 2)
        .unwrap();
    let got = c.get("https://s.example/1").unwrap().unwrap();
    assert!(got.analysis_json.contains("\"v\":2"), "overwritten");
    assert_eq!(got.window_used, "7d");
    assert_eq!(got.bucket, "fast");
}

#[test]
fn cache_error_variant_roundtrip() {
    let e = PipelineError::CacheError("boom".into());
    assert!(e.to_string().contains("boom"));
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

#[test]
fn fake_clock_starts_at_epoch() {
    let c = FakeClock::new(1_785_484_800);
    assert_eq!(c.now_unix(), 1_785_484_800);
}

#[test]
fn fake_clock_advances_and_rewinds() {
    let c = FakeClock::new(1_000);
    c.advance(500);
    assert_eq!(c.now_unix(), 1_500);
    c.advance(-200);
    assert_eq!(c.now_unix(), 1_300);
}

#[test]
fn system_clock_is_recent() {
    let c = SystemClock;
    // 2026-08-01 = 1_785_542_400; anything later than 2024 is fine.
    assert!(c.now_unix() > 1_700_000_000);
}

#[test]
fn clock_used_via_trait_object() {
    let clock: Clock = Arc::new(FakeClock::new(99));
    assert_eq!(clock.now_unix(), 99);
}

#[test]
fn fake_clock_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FakeClock>();
    assert_send_sync::<SystemClock>();
}

// ---------------------------------------------------------------------------
// Policy (§5 Stage 9 / §9)
// ---------------------------------------------------------------------------

fn set_all_policy_env(
    ik: usize,
    ek: usize,
    ct: f64,
    mna: usize,
    mr: usize,
    sb: usize,
) {
    std::env::set_var("INITIAL_K", ik.to_string());
    std::env::set_var("EXPANSION_K", ek.to_string());
    std::env::set_var("COVERAGE_TARGET", ct.to_string());
    std::env::set_var("MIN_NEW_ARTICLES", mna.to_string());
    std::env::set_var("MAX_ROUNDS", mr.to_string());
    std::env::set_var("SEARCH_BUDGET", sb.to_string());
}

fn clear_all_policy_env() {
    for k in [
        "INITIAL_K",
        "EXPANSION_K",
        "COVERAGE_TARGET",
        "MIN_NEW_ARTICLES",
        "MAX_ROUNDS",
        "SEARCH_BUDGET",
    ] {
        std::env::remove_var(k);
    }
}

#[test]
fn policy_defaults_match_spec() {
    let p = Policy::default();
    assert_eq!(p.initial_k, 5);
    assert_eq!(p.expansion_k, 3);
    assert_eq!(p.coverage_target, 0.85);
    assert_eq!(p.min_new_articles, 1);
    assert_eq!(p.max_rounds, 3);
    assert_eq!(p.search_budget, 15);
}

#[test]
fn policy_json_roundtrip() {
    let p = Policy {
        initial_k: 7,
        expansion_k: 3,
        coverage_target: 0.8,
        min_new_articles: 2,
        max_rounds: 5,
        search_budget: 25,
    };
    let j = serde_json::to_string(&p).unwrap();
    let back: Policy = serde_json::from_str(&j).unwrap();
    assert_eq!(back, p);
}

#[test]
fn policy_from_env_all_fields() {
    set_all_policy_env(7, 4, 0.75, 2, 6, 30);
    let p = Policy::from_env();
    assert_eq!(p.initial_k, 7);
    assert_eq!(p.expansion_k, 4);
    assert_eq!(p.coverage_target, 0.75);
    assert_eq!(p.min_new_articles, 2);
    assert_eq!(p.max_rounds, 6);
    assert_eq!(p.search_budget, 30);
    clear_all_policy_env();
}

#[test]
fn policy_load_from_file_with_env_override() {
    set_all_policy_env(1, 1, 0.9, 0, 2, 9);
    let p = Policy {
        initial_k: 7,
        expansion_k: 4,
        coverage_target: 0.8,
        min_new_articles: 2,
        max_rounds: 5,
        search_budget: 25,
    };
    let j = serde_json::to_string(&p).unwrap();
    let dir = std::env::temp_dir().join("linkbot_policy_test.json");
    std::fs::write(&dir, j).unwrap();
    // env wins over the file.
    let loaded = Policy::load_with_env_override(Some(dir.to_str().unwrap()));
    assert_eq!(loaded.initial_k, 1, "env must win");
    assert_eq!(loaded.search_budget, 9, "env must win");
    let _ = std::fs::remove_file(&dir);
    clear_all_policy_env();
}

#[test]
fn policy_file_used_when_no_env() {
    clear_all_policy_env();
    let p = Policy {
        initial_k: 7,
        expansion_k: 4,
        coverage_target: 0.8,
        min_new_articles: 2,
        max_rounds: 5,
        search_budget: 25,
    };
    let j = serde_json::to_string(&p).unwrap();
    let dir = std::env::temp_dir().join("linkbot_policy_test2.json");
    std::fs::write(&dir, j).unwrap();
    let loaded = Policy::load_with_env_override(Some(dir.to_str().unwrap()));
    assert_eq!(loaded, p, "file used when no env vars set");
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn policy_missing_file_falls_back_to_default() {
    clear_all_policy_env();
    let p = Policy::load_with_env_override(Some("/nonexistent/policy.json"));
    assert_eq!(p, Policy::default());
}

#[test]
fn policy_bad_file_falls_back_to_default() {
    clear_all_policy_env();
    let dir = std::env::temp_dir().join("linkbot_policy_bad.json");
    std::fs::write(&dir, "not json").unwrap();
    let p = Policy::load_with_env_override(Some(dir.to_str().unwrap()));
    assert_eq!(p, Policy::default());
    let _ = std::fs::remove_file(&dir);
}

// ---------------------------------------------------------------------------
// Config (§9)
// ---------------------------------------------------------------------------

#[test]
fn config_defaults() {
    let c = Config::default();
    assert_eq!(c.cooldown_secs, 60);
    assert_eq!(c.cache_ttl_hours, 24);
    assert!(c.allow_all_channels);
    assert_eq!(c.reply_mode, ReplyMode::Thread);
    assert_eq!(c.llm_api_base, "https://ollama.com/v1");
    assert_eq!(c.llm_model, "deepseek-v4-flash:0731");
    assert_eq!(c.corpus_token_budget, 60_000);
}

#[test]
fn config_channel_allowlist() {
    let c = Config {
        allow_all_channels: false,
        analyze_channels: vec!["123".into(), "456".into()],
        ..Default::default()
    };
    assert!(c.channel_allowed("123"));
    assert!(c.channel_allowed("456"));
    assert!(!c.channel_allowed("789"));
}

#[test]
fn config_allow_all_channels() {
    let c = Config {
        allow_all_channels: true,
        analyze_channels: vec![],
        ..Default::default()
    };
    assert!(c.channel_allowed("anything"));
}

#[test]
fn config_from_env_full() {
    std::env::set_var("DISCORD_TOKEN", "tok");
    std::env::set_var("TINYFISH_API_KEY", "tf");
    std::env::set_var("EXA_API_KEY", "exa");
    std::env::set_var("LLM_API_BASE", "https://ollama.com/v1");
    std::env::set_var("LLM_API_KEY", "llm");
    std::env::set_var("LLM_MODEL", "deepseek-v4-flash:0731");
    std::env::set_var("ANALYZE_CHANNELS", "111, 222 ,333");
    std::env::set_var("COOLDOWN_SECS", "120");
    std::env::set_var("CACHE_TTL_HOURS", "48");
    std::env::set_var("CORPUS_TOKEN_BUDGET", "12345");
    std::env::set_var("REPLY_MODE", "split");
    let c = Config::from_env().unwrap();
    assert_eq!(c.discord_token, "tok");
    assert!(!c.allow_all_channels);
    assert_eq!(
        c.analyze_channels,
        vec!["111".to_string(), "222".to_string(), "333".to_string()]
    );
    assert_eq!(c.cooldown_secs, 120);
    assert_eq!(c.cache_ttl_hours, 48);
    assert_eq!(c.corpus_token_budget, 12345);
    assert_eq!(c.reply_mode, ReplyMode::Split);
    for k in [
        "DISCORD_TOKEN",
        "TINYFISH_API_KEY",
        "EXA_API_KEY",
        "LLM_API_KEY",
        "LLM_MODEL",
        "ANALYZE_CHANNELS",
        "COOLDOWN_SECS",
        "CACHE_TTL_HOURS",
        "CORPUS_TOKEN_BUDGET",
        "REPLY_MODE",
    ] {
        std::env::remove_var(k);
    }
}

#[test]
fn config_from_env_star_means_all() {
    std::env::set_var("ANALYZE_CHANNELS", "*");
    let c = Config::from_env().unwrap();
    assert!(c.allow_all_channels);
    assert!(c.analyze_channels.is_empty());
    std::env::remove_var("ANALYZE_CHANNELS");
}

#[test]
fn config_from_env_garbage_numbers_keep_defaults() {
    std::env::set_var("COOLDOWN_SECS", "not-a-number");
    std::env::set_var("CACHE_TTL_HOURS", "abc");
    let c = Config::from_env().unwrap();
    assert_eq!(c.cooldown_secs, 60, "unparseable → default");
    assert_eq!(c.cache_ttl_hours, 24, "unparseable → default");
    std::env::remove_var("COOLDOWN_SECS");
    std::env::remove_var("CACHE_TTL_HOURS");
}

#[test]
fn config_from_env_negative_numbers_pass_through() {
    // parse_i64 has no sign clamp — a valid negative is accepted as-is.
    std::env::set_var("COOLDOWN_SECS", "-3");
    let c = Config::from_env().unwrap();
    assert_eq!(c.cooldown_secs, -3);
    std::env::remove_var("COOLDOWN_SECS");
}

#[test]
fn config_invalid_domain_speed_json_errors() {
    std::env::set_var("DOMAIN_SPEED_JSON", "{{{");
    assert!(Config::from_env().is_err());
    std::env::remove_var("DOMAIN_SPEED_JSON");
}

#[test]
fn config_reply_mode_thread_default() {
    assert_eq!(ReplyMode::Thread, ReplyMode::Thread);
    assert_ne!(ReplyMode::Thread, ReplyMode::Split);
    // Serialization contract: lowercase.
    let j = serde_json::to_string(&ReplyMode::Split).unwrap();
    assert_eq!(j, "\"split\"");
}
