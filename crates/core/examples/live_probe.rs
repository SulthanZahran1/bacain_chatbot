//! Live pipeline probe — runs the REAL pipeline (TinyFish fetch, Exa/TinyFish
//! search, Ollama Cloud LLM) against a real URL using env config. Prints the
//! analysis meta (window used, rounds, stop reason, citations) as evidence
//! for the §15 live-verification checklist.
//!
//! Usage: cargo run --release --example live_probe -- <url> [channel_id]

use std::sync::Arc;

use linkbot_core::clock;
use linkbot_core::config::Config;
use linkbot_core::pipeline::{analyze, AnalysisRequest, ChannelCtx, Deps};

#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).expect("usage: live_probe <url> [channel_id]");
    let channel = std::env::args().nth(2).unwrap_or_else(|| "probe".to_string());

    let config = Arc::new(Config::from_env().expect("config from env"));
    let deps = Deps {
        fetcher: Arc::new(linkbot_core::fetcher::TinyFishFetcher::new(
            config.tinyfish_api_key.clone(),
        )),
        searcher: Arc::new(linkbot_core::searcher::ExaSearchProvider::new(
            config.exa_api_key.clone(),
        )),
        llm: Arc::new(linkbot_core::synthesizer::LlmClient::new(
            config.llm_api_base.clone(),
            config.llm_api_key.clone(),
            config.llm_model.clone(),
        )),
        clock: clock::system(),
        config: config.clone(),
    };

    println!("URL: {url}");
    let t0 = std::time::Instant::now();
    // Minimal tracing init so pipeline phase logs (classified / window /
    // search round / coverage) surface with timestamps.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .try_init();
    let analysis = match analyze(
        AnalysisRequest { url: url.clone(), channel: ChannelCtx { id: channel } },
        &deps,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ANALYSIS ERROR: {e}");
            std::process::exit(1);
        }
    };
    let elapsed = t0.elapsed();

    println!("--- RESULT ({}ms) ---", elapsed.as_millis());
    println!("title: {}", analysis.title);
    println!("bucket: {}", analysis.meta.bucket);
    println!("window_used: {}", analysis.meta.window_used);
    println!("recency_minutes: {:?}", analysis.meta.recency_minutes);
    println!("corpus_size: {}", analysis.meta.corpus_size);
    println!("rounds: {}", analysis.meta.rounds);
    println!("stop_reason: {}", analysis.meta.stop_reason);
    println!("citations_rejected: {}", analysis.meta.citations_rejected);
    println!("llm_model: {}", analysis.meta.llm_model);
    println!("summary (first 160): {}", analysis.summary.chars().take(160).collect::<String>());
    println!("--- CITATIONS ---");
    for (i, c) in analysis.citations.iter().enumerate() {
        println!("{i}: {}", c.url);
    }
    println!("--- DEEP (first 160): {}", analysis.deep_analysis.chars().take(160).collect::<String>());
    println!("--- CRITIQUE (first 160): {}", analysis.critique.chars().take(160).collect::<String>());
}
