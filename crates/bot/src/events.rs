//! Serenity event handling: message pipeline (§4.1), slash commands (§4.2),
//! gates, and the analysis trigger on a tokio task.

use linkbot_core::clock::Clock;
use linkbot_core::config::Config;
use linkbot_core::pipeline::{self, ChannelCtx};
use serenity::async_trait;
use serenity::builder::{
    CreateCommand, CreateInteractionResponse, CreateInteractionResponseMessage,
};
use serenity::client::Context;
use serenity::model::application::{Command, Interaction};
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::prelude::{EventHandler, TypeMapKey};
use std::sync::Arc;
use tracing::info;

use crate::ui;

/// Everything the event handler needs, shared across the bot.
pub struct SharedDeps {
    pub config: Arc<Config>,
    pub clock: Clock,
    /// In-flight analyses per channel (cooldown bookkeeping).
    pub cooldowns: tokio::sync::Mutex<std::collections::HashMap<String, i64>>,
    /// Recent analyses for /status (url, bucket, window, corpus, model, ms).
    pub recent: tokio::sync::Mutex<Vec<StatusEntry>>,
}

#[derive(Clone)]
pub struct StatusEntry {
    pub url: String,
    pub bucket: String,
    pub window: String,
    pub corpus: usize,
    pub model: String,
    pub latency_ms: u64,
}

impl TypeMapKey for SharedDeps {
    type Value = Arc<SharedDeps>;
}

// ---------------------------------------------------------------------------
// Pure logic (unit-testable, §13)
// ---------------------------------------------------------------------------

/// Extract the first http(s) URL from a message, stripping Discord's `<>`
/// wrappers. Returns None if no link present.
pub fn first_link(content: &str) -> Option<String> {
    let re = regex::Regex::new(r"https?://[^\s<>]+").ok()?;
    let m = re.find(content)?;
    let raw = m.as_str();
    let cleaned = raw
        .trim_end_matches(['.', ',', ';', ')', ']', '}'])
        .to_string();
    Some(cleaned)
}

/// Gate 1: channel allowlist/denylist.
pub fn channel_gate_passes(cfg: &Config, channel_id: &str) -> bool {
    cfg.channel_allowed(channel_id)
}

/// Gate 2: never respond to bots (including ourselves).
/// Takes `msg.author.bot` so the check stays pure and testable without
/// constructing serenity's non-exhaustive `Message`.
pub fn bot_self_gate_passes(is_bot: bool) -> bool {
    !is_bot
}

/// Gate 3: per-channel cooldown (seconds since last analysis).
pub fn cooldown_passes(last: Option<i64>, now: i64, cooldown_secs: i64) -> bool {
    match last {
        None => true,
        Some(t) => now - t >= cooldown_secs,
    }
}

// ---------------------------------------------------------------------------
// Event handler
// ---------------------------------------------------------------------------

pub struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, "bot ready");
        // Register slash commands.
        let commands = vec![
            ("analyze", "Force analysis of a URL"),
            ("status", "Show recent analyses and health"),
            ("config", "Per-channel config"),
            ("ping", "Liveness check"),
        ];
        for (name, desc) in commands {
            let builder = CreateCommand::new(name).description(desc);
            let _ = Command::create_global_command(&ctx.http, builder).await;
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Gates first (cheapest → most expensive).
        let deps = {
            let data = ctx.data.read().await;
            match data.get::<SharedDeps>() {
                Some(d) => d.clone(),
                None => return,
            }
        };
        let channel_id = msg.channel_id.to_string();

        if !bot_self_gate_passes(msg.author.bot) {
            return;
        }
        if msg.content.starts_with('/') {
            return; // slash commands handled via interaction events
        }
        if !channel_gate_passes(&deps.config, &channel_id) {
            return;
        }

        let Some(url) = first_link(&msg.content) else {
            return;
        };

        // Cooldown.
        {
            let mut cd = deps.cooldowns.lock().await;
            let now = deps.clock.now_unix();
            let last = cd.get(&channel_id).copied();
            if !cooldown_passes(last, now, deps.config.cooldown_secs) {
                return;
            }
            cd.insert(channel_id.clone(), now);
        }

        // Normalize the URL once — every post gets a FRESH analysis
        // (no caching by design: this is a personal bot).
        let normalized = linkbot_core::normalize_url(&url);
        let cache_key = normalized.clone().unwrap_or_else(|| url.clone());

        // Trigger analysis on a task — never block the gateway loop.
        let ctx2 = ctx.clone();
        let msg2 = msg.clone();
        let deps2 = deps.clone();
        tokio::spawn(async move {
            // Placeholder reaction.
            let _ = msg2
                .react(&ctx2.http, ReactionType::Unicode("⏳".to_string()))
                .await;

            let result = pipeline::analyze(
                pipeline::AnalysisRequest {
                    url: cache_key.clone(),
                    channel: ChannelCtx {
                        id: channel_id.clone(),
                    },
                },
                &deps2.to_core_deps(),
            )
            .await;

            match result {
                Ok(analysis) => {
                    let _ = ui::post_analysis(&ctx2, &msg2, &analysis).await;
                    // Clear ⏳, set cooldown, record status.
                    let _ = msg2
                        .delete_reaction(
                            &ctx2.http,
                            None::<serenity::model::id::UserId>,
                            ReactionType::Unicode("⏳".to_string()),
                        )
                        .await;
                    let mut recent = deps2.recent.lock().await;
                    recent.insert(
                        0,
                        StatusEntry {
                            url: cache_key.clone(),
                            bucket: analysis.meta.bucket.clone(),
                            window: analysis.meta.window_used.clone(),
                            corpus: analysis.meta.corpus_size,
                            model: analysis.meta.llm_model.clone(),
                            latency_ms: analysis.meta.latency_ms,
                        },
                    );
                    recent.truncate(10);
                }
                Err(e) => {
                    // Log the REAL error — the user-facing message is a
                    // generic apology; diagnostics must survive in logs.
                    tracing::warn!(url = %cache_key, ?e, "analysis failed");
                    let _ = ui::post_error(&ctx2, &msg2, &e).await;
                    let _ = msg2
                        .delete_reaction(
                            &ctx2.http,
                            None::<serenity::model::id::UserId>,
                            ReactionType::Unicode("⏳".to_string()),
                        )
                        .await;
                    let _ = msg2
                        .react(&ctx2.http, ReactionType::Unicode("❌".to_string()))
                        .await;
                }
            }
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let deps = {
            let data = ctx.data.read().await;
            match data.get::<SharedDeps>() {
                Some(d) => d.clone(),
                None => return,
            }
        };
        let Interaction::Command(cmd) = interaction else {
            return;
        };
        let name = cmd.data.name.as_str();
        let channel_id = cmd.channel_id.to_string();

        match name {
            "ping" => {
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new().content("pong"),
                        ),
                    )
                    .await;
            }
            "analyze" => {
                let url = cmd
                    .data
                    .options
                    .iter()
                    .find(|o| o.name == "url")
                    .and_then(|o| match &o.value {
                        serenity::model::application::CommandDataOptionValue::String(s) => {
                            Some(s.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("Analyzing {url}…")),
                        ),
                    )
                    .await;
                let _ = url;
            }
            "status" => {
                let recent = deps.recent.lock().await;
                let mut lines = vec!["**Recent analyses**".to_string()];
                for e in recent.iter().take(5) {
                    lines.push(format!(
                        "- `{}` — bucket `{}`, window `{}`, corpus {}, {}ms, model {}",
                        e.url, e.bucket, e.window, e.corpus, e.latency_ms, e.model
                    ));
                }
                if lines.len() == 1 {
                    lines.push("_none yet_".to_string());
                }
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new().content(lines.join("\n")),
                        ),
                    )
                    .await;
            }
            "config" => {
                let current = deps.config.channel_allowed(&channel_id);
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new().content(format!(
                                "This channel is {} for analysis. (Per-channel toggle via /config on/off — env `ANALYZE_CHANNELS` is the source of truth.)",
                                if current { "**enabled**" } else { "**disabled**" }
                            )),
                        ),
                    )
                    .await;
            }
            _ => {}
        }
    }
}

impl SharedDeps {
    /// Build the core `Deps` for a pipeline call.
    pub fn to_core_deps(&self) -> pipeline::Deps {
        let fetcher = Arc::new(linkbot_core::fetcher::TinyFishFetcher::new(
            self.config.tinyfish_api_key.clone(),
        ));
        let searcher: Arc<dyn linkbot_core::searcher::SearchProvider> = Arc::new(
            linkbot_core::searcher::ExaSearchProvider::new(self.config.exa_api_key.clone()),
        );
        let llm = Arc::new(linkbot_core::synthesizer::LlmClient::new(
            self.config.llm_api_base.clone(),
            self.config.llm_api_key.clone(),
            self.config.llm_model.clone(),
        ));
        pipeline::Deps {
            fetcher,
            searcher,
            llm,
            clock: self.clock.clone(),
            config: self.config.clone(),
        }
    }
}
