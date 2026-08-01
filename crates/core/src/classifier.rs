//! Topic classification — keyword scoring + optional LLM disambiguation
//! (§5 Stage 2). High/low scores skip the LLM call; the ambiguous band
//! (1–3 keyword hits) triggers a cheap LLM disambiguation.

use serde::{Deserialize, Serialize};

/// Weighted term list. Position multiplier: title hits ×3.
const TERMS: &[(&str, f64)] = &[
    ("llm", 2.0),
    ("gpt", 2.0),
    ("agent", 1.0),
    ("agentic", 2.0),
    ("model", 1.0),
    ("neural", 2.0),
    ("transformer", 2.0),
    ("rlhf", 2.0),
    ("fine-tun", 2.0),
    ("finetun", 2.0),
    ("hallucinat", 2.0),
    ("embedding", 2.0),
    ("token", 1.0),
    ("inference", 1.5),
    ("prompt", 1.0),
    ("rag", 2.0),
    ("diffusion", 1.5),
    ("multimodal", 2.0),
    ("openai", 2.0),
    ("anthropic", 2.0),
    ("deepmind", 2.0),
    ("meta ai", 2.0),
    ("mistral", 2.0),
    ("xai", 1.5),
    ("hugging face", 2.0),
    ("chatbot", 1.0),
    ("copilot", 1.5),
    ("claude", 1.5),
    ("gemini", 1.5),
    ("deepseek", 1.5),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub is_ai_topic: bool,
    pub score: u32,
    pub ambiguous: bool,
}

impl Classification {
    pub fn decided(&self) -> bool {
        !self.ambiguous
    }
}

/// Keyword pass. Returns score and whether the score falls in the ambiguous
/// band (score ≥ AMBIGUOUS_MIN and ≤ AMBIGUOUS_MAX → needs LLM).
pub fn keyword_score(title: &str, text: &str) -> (u32, bool) {
    let title_l = title.to_lowercase();
    let text_l = text.to_lowercase();
    let mut score = 0.0_f64;
    for (term, weight) in TERMS {
        if title_l.contains(term) {
            score += weight * 3.0;
        }
        if text_l.contains(term) {
            score += weight;
        }
    }
    let score = score.round() as u32;
    let ambiguous = (1..=3).contains(&score);
    (score, ambiguous)
}

/// Disambiguation hook signature (LLM-backed, optional).
type Disambiguator = dyn Fn(&str, &str) -> bool;

/// Full classification with optional LLM disambiguation hook.
///
/// `llm_disambiguate` is called only when the keyword score is ambiguous.
/// When None is passed, ambiguous scores resolve to `false` (conservative,
/// deterministic — used by the optimizer and scenario suite where the LLM is
/// scripted/mocked).
pub fn classify(
    title: &str,
    text: &str,
    llm_disambiguate: Option<&Disambiguator>,
) -> Classification {
    let (score, ambiguous) = keyword_score(title, text);
    let is_ai = if ambiguous {
        match llm_disambiguate {
            Some(f) => f(title, text),
            None => false,
        }
    } else {
        score > 0
    };
    Classification {
        is_ai_topic: is_ai,
        score,
        ambiguous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AI_TEXT: &str =
        "the model was fine-tuned with RLHF to reduce hallucination; the agent used RAG embeddings";
    const NON_AI_TEXT: &str =
        "the company reported quarterly earnings and opened a new factory in the region";

    #[test]
    fn strong_ai_signal_is_decided_ai() {
        let (s, amb) = keyword_score("OpenAI launches new LLM agent", AI_TEXT);
        assert!(s > 3, "score {s}");
        assert!(!amb);
        let c = classify("OpenAI launches new LLM agent", AI_TEXT, None);
        assert!(c.is_ai_topic);
        assert!(!c.ambiguous);
    }

    #[test]
    fn clear_non_ai_is_decided() {
        let (s, amb) = keyword_score("Factory expansion in Ohio", NON_AI_TEXT);
        assert_eq!(s, 0);
        assert!(!amb);
        let c = classify("Factory expansion in Ohio", NON_AI_TEXT, None);
        assert!(!c.is_ai_topic);
    }

    #[test]
    fn ambiguous_band_triggers_llm() {
        let (s, amb) = keyword_score("A model chat", "plain text with no keyword hits");
        assert!((1..=3).contains(&s), "score {s}");
        assert!(amb);
        // LLM says yes
        let c = classify("A model chat", "x", Some(&|_, _| true));
        assert!(c.is_ai_topic);
        // LLM says no
        let c = classify("A model chat", "x", Some(&|_, _| false));
        assert!(!c.is_ai_topic);
        // No LLM → conservative false
        let c = classify("A model chat", "x", None);
        assert!(!c.is_ai_topic);
    }

    #[test]
    fn title_hits_count_three_times() {
        let (s1, _) = keyword_score("LLM", "");
        let (s2, _) = keyword_score("", "llm");
        assert!(s1 >= s2 * 2, "{s1} vs {s2}");
    }

    #[test]
    fn vendor_names_score() {
        let (s, _) = keyword_score("Anthropic", "x");
        assert!(s >= 6);
    }
}
