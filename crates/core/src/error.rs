//! Error taxonomy — every variant maps 1:1 to a user-facing message (§10 of goal.md).

use thiserror::Error;

/// Terminal pipeline failures. Each maps to a specific Discord-facing message.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PipelineError {
    #[error("invalid url")]
    InvalidUrl,
    #[error("dead link: page not found")]
    PageNotFound,
    #[error("dead link: target http error")]
    TargetHttpError,
    #[error("site blocks automated readers")]
    BotBlocked,
    #[error("page has no extractable content")]
    EmptyContent,
    #[error("could not reach target (timeout)")]
    Timeout,
    #[error("target unreachable")]
    TargetUnreachable,
    #[error("proxy error upstream")]
    ProxyError,
    #[error("search provider failed: {0}")]
    SearchFailed(String),
    #[error("llm synthesis failed: {0}")]
    SynthesisFailed(String),
    #[error("internal: {0}")]
    Internal(String),
    #[error("deadline exceeded")]
    DeadlineExceeded,
}

/// The exact user-facing strings for each error — used by the bot crate and
/// tested in §13. Kept in one place so UI and tests can't drift.
pub fn user_message(e: &PipelineError) -> UserMessage {
    use PipelineError::*;
    match e {
        InvalidUrl => UserMessage::error("That doesn't look like a valid link."),
        PageNotFound | TargetHttpError => {
            UserMessage::error("That link is dead (HTTP 404 or server error).")
        }
        BotBlocked => UserMessage::error("The site blocks automated readers — can't retrieve it."),
        EmptyContent => UserMessage::error("Page has no extractable content."),
        Timeout | TargetUnreachable => {
            UserMessage::error("Couldn't reach it right now — try again in a bit.")
        }
        ProxyError => UserMessage::error("Upstream proxy hiccup — try again shortly."),
        SearchFailed(_) => {
            UserMessage::error("Search backend is unhappy right now — try again shortly.")
        }
        SynthesisFailed(_) => UserMessage::error(
            "The analysis engine failed to produce a response. Sorry about that!",
        ),
        Internal(_) => UserMessage::error("Something went wrong internally."),
        DeadlineExceeded => UserMessage::error("Analysis took too long and was cut off."),
    }
}

/// A message destined for Discord (in-thread or plain reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserMessage {
    Error(String),
    Info(String),
}

impl UserMessage {
    pub fn error(s: impl Into<String>) -> Self {
        UserMessage::Error(s.into())
    }
    pub fn info(s: impl Into<String>) -> Self {
        UserMessage::Info(s.into())
    }
    pub fn text(&self) -> &str {
        match self {
            UserMessage::Error(s) | UserMessage::Info(s) => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_variant_maps_to_a_message() {
        let cases = vec![
            PipelineError::InvalidUrl,
            PipelineError::PageNotFound,
            PipelineError::TargetHttpError,
            PipelineError::BotBlocked,
            PipelineError::EmptyContent,
            PipelineError::Timeout,
            PipelineError::TargetUnreachable,
            PipelineError::ProxyError,
            PipelineError::SearchFailed("x".into()),
            PipelineError::SynthesisFailed("x".into()),
            PipelineError::Internal("x".into()),
            PipelineError::DeadlineExceeded,
        ];
        for e in cases {
            let m = user_message(&e);
            assert!(!m.text().is_empty(), "no message for {e:?}");
        }
    }
}
