//! Rendering (§4.3): thread-per-analysis, 2000-char split, embeds, footers.

use linkbot_core::error::PipelineError;
use linkbot_core::pipeline::Analysis;
use serenity::builder::{CreateEmbed, CreateEmbedFooter, CreateMessage, CreateThread};
use serenity::client::Context;
use serenity::model::channel::Message;
use tracing::warn;

/// Discord hard limit.
pub const MAX_MSG_CHARS: usize = 2000;

/// Split text at paragraph boundaries so each chunk ≤ 2000 chars (§10).
pub fn split_chunks(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        if current.chars().count() + para.chars().count() + 2 > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            // A single paragraph longer than limit: hard-split.
            if para.chars().count() > limit {
                let mut rest = para.to_string();
                while rest.chars().count() > limit {
                    let take: String = rest.chars().take(limit).collect();
                    chunks.push(take);
                    rest = rest.chars().skip(limit).collect();
                }
                current = rest;
            } else {
                current = para.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Human-readable freshness footer (§4.3 ⏱ line).
pub fn window_footer(analysis: &Analysis) -> String {
    format!(
        "⏱ window used: {} (bucket `{}`, {} related read, {} rounds, {}ms, model {})",
        analysis.meta.window_used,
        analysis.meta.bucket,
        analysis.meta.corpus_size,
        analysis.meta.rounds,
        analysis.meta.latency_ms,
        analysis.meta.llm_model
    )
}

/// Post the full analysis into a thread anchored to the original message.
pub async fn post_analysis(
    ctx: &Context,
    original: &Message,
    analysis: &Analysis,
) -> serenity::Result<()> {
    // Create thread anchored to the original message (it becomes the
    // thread's start message → the thread "replies" to the message).
    let mut thread_builder = CreateThread::new("📚 Link analysis");
    thread_builder = thread_builder
        .auto_archive_duration(serenity::model::channel::AutoArchiveDuration::OneHour);
    let thread = original
        .channel_id
        .create_thread_from_message(&ctx.http, original.id, thread_builder)
        .await;

    let thread_id = match thread {
        Ok(t) => t.id,
        Err(e) => {
            warn!(?e, "thread creation failed; falling back to reply");
            original.channel_id
        }
    };

    // Msg 1 — Summary embed.
    let embed = CreateEmbed::new()
        .title(analysis.summary.chars().take(200).collect::<String>())
        .description(&analysis.summary)
        .footer(CreateEmbedFooter::new(window_footer(analysis)));
    let _ = thread_id
        .send_message(
            &ctx.http,
            CreateMessage::new().content("## Summary").embed(embed),
        )
        .await;

    // Msg 2 — Deep analysis (split on paragraphs).
    for chunk in split_chunks(&analysis.deep_analysis, MAX_MSG_CHARS) {
        let _ = thread_id
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!("## Deep Analysis\n\n{chunk}")),
            )
            .await;
    }

    // Msg 3 — Critique.
    let _ = thread_id
        .send_message(
            &ctx.http,
            CreateMessage::new().content(format!("## Critique\n\n{}", analysis.critique)),
        )
        .await;

    // Msg 4 — Sources (numbered, verified only).
    if !analysis.citations.is_empty() {
        let mut sources = String::from("## Sources\n");
        for (i, c) in analysis.citations.iter().enumerate() {
            sources.push_str(&format!("[{}] {} — {}\n", i + 1, c.context, c.url));
        }
        for chunk in split_chunks(&sources, MAX_MSG_CHARS) {
            let _ = thread_id
                .send_message(&ctx.http, CreateMessage::new().content(chunk))
                .await;
        }
    }

    // Rename thread to the article title (best effort).
    let title = analysis.summary.chars().take(60).collect::<String>();
    let edit = serenity::builder::EditThread::new().name(format!("📚 {title}"));
    let _ = thread_id.edit_thread(&ctx.http, edit).await;

    Ok(())
}

/// Post a user-facing error (§10 delivery rules).
pub async fn post_error(
    ctx: &Context,
    original: &Message,
    err: &PipelineError,
) -> serenity::Result<()> {
    let msg = linkbot_core::error::user_message(err);
    original
        .channel_id
        .send_message(
            &ctx.http,
            CreateMessage::new().content(msg.text().to_string()),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_unchunked() {
        assert_eq!(
            split_chunks("hello", MAX_MSG_CHARS),
            vec!["hello".to_string()]
        );
    }

    #[test]
    fn splits_at_paragraph_boundaries() {
        let para = "a".repeat(1500);
        let text = format!("{para}\n\n{para}\n\n{para}");
        let chunks = split_chunks(&text, MAX_MSG_CHARS);
        assert_eq!(chunks.len(), 3);
        for c in &chunks {
            assert!(c.chars().count() <= MAX_MSG_CHARS);
        }
    }

    #[test]
    fn hard_splits_overlong_paragraph() {
        let text = "x".repeat(5000);
        let chunks = split_chunks(&text, MAX_MSG_CHARS);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.chars().count() <= MAX_MSG_CHARS);
        }
    }

    #[test]
    fn footer_shows_window() {
        let a = Analysis {
            summary: "s".into(),
            deep_analysis: "d".into(),
            critique: "c".into(),
            citations: vec![],
            meta: linkbot_core::pipeline::AnalysisMeta {
                bucket: "fast".into(),
                window_used: "7d".into(),
                recency_minutes: Some(10_080),
                corpus_size: 6,
                rounds: 2,
                stop_reason: "coverage(0.90)".into(),
                latency_ms: 1234,
                llm_model: "deepseek-v4-flash:0731".into(),
                citations_rejected: 0,
            },
        };
        let f = window_footer(&a);
        assert!(f.contains("7d"));
        assert!(f.contains("fast"));
        assert!(f.contains("deepseek-v4-flash:0731"));
    }
}
