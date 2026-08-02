//! Rendering (§4.3): thread-per-analysis, 2000-char split, embeds, footers.

use linkbot_core::error::PipelineError;
use linkbot_core::pipeline::Analysis;
use serenity::builder::{CreateEmbed, CreateEmbedFooter, CreateMessage, CreateThread};
use serenity::client::Context;
use serenity::model::channel::Message;
use tracing::warn;

/// Discord hard limit.
pub const MAX_MSG_CHARS: usize = 2000;

/// Strip leftover `[cite x]` / `[1]`-style inline markers the model may
/// emit (citations render in the Sources section instead). Defense in
/// depth — the prompt also forbids them.
pub fn strip_cite_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("[cite") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 5..];
        // Find the closing ']' — drop through it, keep everything after.
        match after.find(']') {
            Some(idx) => {
                rest = &after[idx + 1..];
            }
            None => {
                // Unclosed marker — drop the rest of the string.
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

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
            // A single paragraph longer than limit: hard-split at the last
            // whitespace before the boundary — never mid-URL/word (a
            // truncated <url renders as plain text, breaking the link).
            if para.chars().count() > limit {
                let mut rest = para.to_string();
                while rest.chars().count() > limit {
                    let candidate: String = rest.chars().take(limit).collect();
                    let cut = match candidate.rfind(char::is_whitespace) {
                        Some(idx) if idx > 0 => idx,
                        _ => limit, // no whitespace — unavoidable hard cut
                    };
                    let take: String = candidate.chars().take(cut).collect();
                    chunks.push(take);
                    rest = rest.chars().skip(cut).collect();
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

    // Msg 1 — Summary embed. Title is the short 3-4 word title; the summary
    // lives ONLY in the description (never duplicate it in the title).
    let embed = CreateEmbed::new()
        .title(&analysis.title)
        .description(&analysis.summary)
        .footer(CreateEmbedFooter::new(window_footer(analysis)));
    let _ = thread_id
        .send_message(
            &ctx.http,
            CreateMessage::new().content("## Summary").embed(embed),
        )
        .await;

    // Msg 2 — Deep analysis (split on paragraphs).
    for chunk in split_chunks(&strip_cite_markers(&analysis.deep_analysis), MAX_MSG_CHARS) {
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

    // Msg 4 — Sources (numbered, verified only). Wrap URLs in <> so Discord
    // renders them as clickable links even after an em-dash.
    if !analysis.citations.is_empty() {
        let mut sources = String::from("## Sources\n");
        for (i, c) in analysis.citations.iter().enumerate() {
            sources.push_str(&format!("[{}] {} — <{}>\n", i + 1, c.context, c.url));
        }
        for chunk in split_chunks(&sources, MAX_MSG_CHARS) {
            let _ = thread_id
                .send_message(&ctx.http, CreateMessage::new().content(chunk))
                .await;
        }
    }

    // Rename thread to the short title (best effort).
    let edit = serenity::builder::EditThread::new().name(format!("📚 {}", analysis.title));
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
    fn split_never_cuts_urls() {
        // 2000 'a's, then a long URL with no following whitespace — the
        // URL must never be split mid-string.
        let text = format!("{} <https://example.com/{}/>", "a".repeat(1980), "b".repeat(400));
        for c in split_chunks(&text, MAX_MSG_CHARS) {
            // Each chunk must not contain a broken '<' without '>'.
            assert_eq!(c.matches('<').count(), c.matches('>').count(), "chunk: {c:?}");
        }
    }

    #[test]
    fn strip_cite_markers_removes_inline_citations() {
        let text = "- Mutation testing injects bugs. [cite testmuai]\n- Second point [cite src] here";
        let cleaned = strip_cite_markers(text);
        assert!(!cleaned.contains("[cite"));
        assert!(cleaned.contains("Mutation testing injects bugs."));
        assert!(cleaned.contains("Second point"));
    }

    #[test]
    fn strip_cite_markers_handles_unclosed() {
        let text = "text [cite broken";
        assert_eq!(strip_cite_markers(text), "text ");
    }

    #[test]
    fn footer_shows_window() {
        let a = Analysis {
            title: "t".into(),
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
