//! Bot bootstrap: config → tracing → shared deps → serenity gateway.

use std::sync::Arc;

use linkbot_bot::{Handler, SharedDeps};
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

    // SQLite telemetry + dedupe + cooldown store (ticket #4).
    // Dedupe is opt-in: DEDUPE_TTL_HOURS=0 (default) disables the gate.
    let store = match linkbot_core::Store::open(
        &config.db_path,
        clock::system(),
        config.dedupe_ttl_hours * 3600,
        config.retention_days,
    ) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("failed to open store at {}: {e}", config.db_path);
            std::process::exit(1);
        }
    };
    tracing::info!(db_path = %config.db_path, "sqlite store open");

    let shared = Arc::new(SharedDeps {
        config: config.clone(),
        clock: clock::system(),
        store,
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
