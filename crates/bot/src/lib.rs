//! linkbot-bot — the Discord surface (frontend) for the bacain chatbot.
//!
//! Thin glue over `linkbot-core`: gate checks, ⏳ reaction + thread-per-
//! analysis, slash commands, rendering. All logic worth testing lives here
//! as pure functions (link extraction, gates, chunking).

pub mod events;
pub mod ui;

pub use events::{Handler, SharedDeps};
