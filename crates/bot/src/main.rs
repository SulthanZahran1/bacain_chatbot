//! Bot bootstrap: config → tracing → shared deps → serenity gateway.

use std::sync::Arc;

use linkbot_bot::{Handler, SharedDeps};
use linkbot_core::cache::Cache;
use linkbot_core::clock;
use linkbot_core::config::Config;
use serenity::client::Client;
use serenity::model::gateway::GatewayIntents;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("config error: {e}");
            std::process::exit(1);
        }
    };

    if config.discord_token.is_empty() {
        eprintln!("DISCORD_TOKEN is required");
        std::process::exit(1);
    }

    let cache = Arc::new(match Cache::open(std::path::Path::new("data/linkbot.db")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cache open failed: {e}");
            std::process::exit(1);
        }
    });

    let shared = Arc::new(SharedDeps {
        config: config.clone(),
        cache,
        clock: clock::system(),
        cooldowns: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        recent: tokio::sync::Mutex::new(Vec::new()),
    });

    let mut client = Client::builder(&config.discord_token, GatewayIntents::all())
        .event_handler(Handler)
        .await
        .expect("failed to create client");

    {
        let mut data = client.data.write().await;
        data.insert::<SharedDeps>(shared);
    }

    tracing::info!("linkbot starting…");
    if let Err(e) = client.start().await {
        eprintln!("client error: {e}");
        std::process::exit(1);
    }
}
