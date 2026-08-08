//! Bot-crate unit tests — pure functions only (§13: "serenity client logic
//! covered via unit tests on the gate/rendering functions").

use linkbot_bot::events::{
    bot_self_gate_passes, channel_gate_passes, cooldown_passes, first_link, media_gate_passes,
};
use linkbot_bot::ui::{split_chunks, window_footer, MAX_MSG_CHARS};
use linkbot_core::config::Config;
use linkbot_core::pipeline::{Analysis, AnalysisMeta};

#[test]
fn extracts_first_link() {
    assert_eq!(
        first_link("check this https://example.com/article"),
        Some("https://example.com/article".to_string())
    );
}

#[test]
fn extracts_link_from_angle_brackets() {
    // Discord wraps URLs in <> when the message has no text.
    assert_eq!(
        first_link("<https://example.com/a>"),
        Some("https://example.com/a".to_string())
    );
}

#[test]
fn strips_trailing_punctuation() {
    assert_eq!(
        first_link("see https://example.com/a."),
        Some("https://example.com/a".to_string())
    );
    assert_eq!(
        first_link("see (https://example.com/a),"),
        Some("https://example.com/a".to_string())
    );
}

#[test]
fn no_link_returns_none() {
    assert_eq!(first_link("no links here"), None);
    assert_eq!(first_link(""), None);
}

#[test]
fn http_link_also_matches() {
    assert_eq!(
        first_link("http://example.com/x"),
        Some("http://example.com/x".to_string())
    );
}

#[test]
fn first_of_many_links_wins() {
    assert_eq!(
        first_link("https://a.com/1 then https://b.com/2"),
        Some("https://a.com/1".to_string())
    );
}

#[test]
fn media_gate_rejects_gif_picker_hosts() {
    // Discord GIF picker injects Tenor URLs into message content.
    assert!(!media_gate_passes(
        "https://tenor.com/view/cat-dance-gif-12345"
    ));
    assert!(!media_gate_passes("https://media.tenor.com/abc123/cat.gif"));
    assert!(!media_gate_passes("https://giphy.com/gifs/xyz-123"));
    assert!(!media_gate_passes("https://gph.is/2abc"));
    assert!(!media_gate_passes(
        "https://media.giphy.com/media/abc/giphy.gif"
    ));
    assert!(!media_gate_passes(
        "https://cdn.discordapp.com/attachments/1/2/img.png"
    ));
    assert!(!media_gate_passes("https://imgur.com/a/abc123"));
    assert!(!media_gate_passes("https://i.imgur.com/abc123.gif"));
}

#[test]
fn media_gate_allows_article_hosts() {
    assert!(media_gate_passes("https://example.com/article"));
    assert!(media_gate_passes("https://news.ycombinator.com/item?id=1"));
    assert!(media_gate_passes(
        "https://subdomain.tenor.com.evil.com/phish"
    ));
    assert!(media_gate_passes("https://tenor.com.evil.com/phish"));
}

#[test]
fn media_gate_rejects_malformed() {
    assert!(!media_gate_passes("not a url"));
    assert!(!media_gate_passes(""));
}

#[test]
fn channel_gate_respects_allowlist() {
    let cfg = Config {
        allow_all_channels: true,
        ..Default::default()
    };
    assert!(channel_gate_passes(&cfg, "anything"));

    let cfg = Config {
        allow_all_channels: false,
        analyze_channels: vec!["123".to_string(), "456".to_string()],
        ..Default::default()
    };
    assert!(channel_gate_passes(&cfg, "123"));
    assert!(channel_gate_passes(&cfg, "456"));
    assert!(!channel_gate_passes(&cfg, "789"));
}
#[test]
fn bot_self_gate_rejects_bots() {
    assert!(!bot_self_gate_passes(true));
    assert!(bot_self_gate_passes(false));
}

#[test]
fn cooldown_passes_edge_cases() {
    // Never analyzed → pass.
    assert!(cooldown_passes(None, 1000, 60));
    // Exactly at boundary → pass.
    assert!(cooldown_passes(Some(940), 1000, 60));
    // Just inside → blocked.
    assert!(!cooldown_passes(Some(941), 1000, 60));
    // Far in the past → pass.
    assert!(cooldown_passes(Some(0), 1000, 60));
    // Zero cooldown → always pass.
    assert!(cooldown_passes(Some(999), 1000, 0));
}

#[test]
fn split_chunks_respects_limit_exactly() {
    let text = format!("{}\n\n{}", "a".repeat(1200), "b".repeat(1200));
    let chunks = split_chunks(&text, MAX_MSG_CHARS);
    for c in &chunks {
        assert!(
            c.chars().count() <= MAX_MSG_CHARS,
            "chunk too long: {}",
            c.chars().count()
        );
    }
    assert!(chunks.len() >= 2);
}

#[test]
fn split_chunks_joins_small_paragraphs() {
    let text = format!("{}\n\n{}", "a".repeat(100), "b".repeat(100));
    let chunks = split_chunks(&text, MAX_MSG_CHARS);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].contains("a".repeat(100).as_str()));
    assert!(chunks[0].contains("b".repeat(100).as_str()));
}

#[test]
fn split_chunks_empty_input() {
    assert_eq!(split_chunks("", MAX_MSG_CHARS), vec!["".to_string()]);
}

#[test]
fn split_chunks_exact_boundary_single_chunk() {
    let text = "x".repeat(MAX_MSG_CHARS);
    assert_eq!(split_chunks(&text, MAX_MSG_CHARS), vec![text.clone()]);
}

#[test]
fn window_footer_includes_all_meta() {
    let a = Analysis {
        title: "t".into(),
        summary: "s".into(),
        deep_analysis: "d".into(),
        critique: "c".into(),
        citations: vec![],
        meta: AnalysisMeta {
            bucket: "ai-override".into(),
            window_used: "30d".into(),
            recency_minutes: Some(43_200),
            corpus_size: 7,
            rounds: 3,
            stop_reason: "coverage(0.90)".into(),
            latency_ms: 4321,
            llm_model: "deepseek-v4-flash:0731".into(),
            citations_rejected: 0,
        },
    };
    let f = window_footer(&a);
    assert!(f.contains("30d"));
    assert!(f.contains("ai-override"));
    assert!(f.contains("7 related"));
    assert!(f.contains("3 rounds"));
    assert!(f.contains("4321ms"));
    assert!(f.contains("deepseek-v4-flash:0731"));
}
