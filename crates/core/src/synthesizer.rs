//! LLM synthesis — one OpenAI-compatible chat completion with strict JSON
//! output, one repair retry on parse failure (§5 Stage 6).

use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Citation {
    pub url: String,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Synthesis {
    /// 3-4 word plain title (used for the embed title + thread name).
    #[serde(default)]
    pub title: String,
    pub summary: String,
    pub deep_analysis: String,
    pub critique: String,
    pub citations: Vec<Citation>,
}

/// Fallback when the model omits `title`: first 3-4 words of the summary.
pub fn title_from_summary(summary: &str) -> String {
    let words: Vec<&str> = summary.split_whitespace().collect();
    let n = words.len().min(4);
    let t = words[..n].join(" ");
    if t.is_empty() { "Link analysis".to_string() } else { t }
}

pub const SYSTEM_PROMPT: &str = r#"You are a rigorous technology analyst. You will be given:
1. A source article (full text) and its metadata.
2. A corpus of related articles fetched from search results (with their URLs).

Produce a JSON object with exactly this schema:
{
  "title":          "<3-4 word plain title: what the work is, no fluff>",
  "summary":        "<2–4 sentences, PLAIN language>",
  "deep_analysis":  "<4-6 scannable bullet points — NO paragraphs>",
  "critique":       "<1-3 short bullets or one 2-sentence verdict>",
  "citations":      [{"url": "...", "context": "<one line: what claim it supports>"}]
}

SECTION GUIDANCE (hard constraints):
- TITLE: 3-4 words, plain nouns/phrases, no articles or filler ("Lookahead Sparse
  Attention", "Windows 8GB RAM Plan", "EU AI Act Deadline"). Not a sentence.
- SUMMARY: Short, plain, effective. What the work is and why it matters,
  understandable by a technical reader who does NOT follow this domain.
  No jargon, no model names, no metrics unless essential. The summary must
  be SHORTER and LESS technical than deep_analysis.
- DEEP_ANALYSIS: 4-6 bullet points, each starting with "- ". Each bullet is
  ONE scannable key point (mechanism, key evidence/numbers, why it matters,
  tensions). MAX 2 lines per bullet. NO paragraphs, NO prose, NO fluff.
  The reader should grasp the whole piece in 30 seconds of skimming.
  If the work has an acronym, explain it once in the first bullet.
- CRITIQUE: 1-3 short bullets OR one 2-sentence verdict. Substantive
  weaknesses only — unsubstantiated claims, missing evidence, internal
  contradictions, over-generalization, flawed comparisons, conflicts with
  the cited sources. If nothing major: one bullet saying the claims hold
  up. Do NOT critique genre conventions: a technical report or preprint
  not being peer-reviewed is not a flaw; an announcement not containing
  experiments is not a flaw; a paper being technical is not a flaw.

CITATION RULES (hard constraints):
- You may ONLY cite URLs from the provided corpus. Never invent, guess, or reconstruct URLs.
- Every citation must actually support the claim it is attached to.
- The source article itself may be cited as [source].
- If nothing in the corpus supports a claim, make the claim without a citation.
- Use the exact URLs as given — do not alter protocol, host, or path."#;

/// Build the user message: source article + related corpus with URLs.
pub fn build_prompt(
    source: &crate::fetcher::FetchedArticle,
    related: &[crate::fetcher::FetchedArticle],
) -> String {
    let mut s = String::new();
    s.push_str("## SOURCE ARTICLE\n");
    s.push_str(&format!("URL: {}\n", source.url));
    s.push_str(&format!("TITLE: {}\n", source.title));
    if let Some(d) = &source.published_date {
        s.push_str(&format!("PUBLISHED: {d}\n"));
    }
    s.push_str(&format!("TEXT:\n{}\n", source.text));
    s.push_str("\n## RELATED ARTICLES (corpus)\n");
    for (i, a) in related.iter().enumerate() {
        s.push_str(&format!("\n[{i}] URL: {}\n", a.url));
        s.push_str(&format!("TITLE: {}\n", a.title));
        if let Some(d) = &a.published_date {
            s.push_str(&format!("PUBLISHED: {d}\n"));
        }
        s.push_str(&format!("TEXT:\n{}\n", a.text));
    }
    s
}

/// LLM abstraction — the pipeline depends on this trait so the scenario
/// suite can script the LLM (coverage assessor, query extractor, synthesis)
/// without any network (§5 Stage 9: "The LLM is also mocked at this level").
#[async_trait::async_trait]
pub trait Llm: Send + Sync {
    async fn chat_json(&self, system: &str, user: &str) -> Result<String, PipelineError>;
    async fn synthesize(
        &self,
        source: &crate::fetcher::FetchedArticle,
        related: &[crate::fetcher::FetchedArticle],
    ) -> Result<Synthesis, PipelineError>;
}

/// OpenAI-compatible chat completion client (thin reqwest client; the spec's
/// `async-openai` equivalent without the heavy dependency surface).
#[derive(Debug, Clone)]
pub struct LlmClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        LlmClient {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .expect("reqwest client"),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
        }
    }

    /// One chat completion with `response_format: {"type": "json_object"}`.
    pub async fn chat_json(&self, system: &str, user: &str) -> Result<String, PipelineError> {
        #[derive(Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: String,
        }
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'a str,
            messages: Vec<Msg<'a>>,
            temperature: f64,
            max_tokens: u32,
            #[serde(skip_serializing_if = "Option::is_none")]
            think: Option<bool>,
            response_format: serde_json::Value,
        }
        let req = Req {
            model: &self.model,
            messages: vec![
                Msg {
                    role: "system",
                    content: system.to_string(),
                },
                Msg {
                    role: "user",
                    content: user.to_string(),
                },
            ],
            temperature: 0.3,
            // Summary + 3-4 para deep analysis + critique routinely exceeds
            // 2000 tokens on real articles; truncation cut the JSON mid-string
            // ("EOF while parsing a string"). 4000 measured sufficient.
            max_tokens: 4000,
            // deepseek-v4-flash on Ollama Cloud burns its whole budget on
            // hidden reasoning and returns empty content unless thinking is
            // disabled. Measured: 42s+empty → 7.6s+valid JSON.
            think: Some(false),
            response_format: serde_json::json!({"type": "json_object"}),
        };
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| PipelineError::SynthesisFailed(format!("llm transport: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(PipelineError::SynthesisFailed(format!("llm http {status}")));
        }
        #[derive(Deserialize)]
        struct Resp {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: MsgOut,
        }
        #[derive(Deserialize)]
        struct MsgOut {
            content: Option<String>,
        }
        let body: Resp = resp
            .json()
            .await
            .map_err(|e| PipelineError::SynthesisFailed(format!("llm decode: {e}")))?;
        body.choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| PipelineError::SynthesisFailed("empty llm content".into()))
    }

    /// Parse synthesis JSON tolerantly: model output can contain duplicate
    /// keys ("duplicate field `deep_analysis`") which serde's struct parser
    /// rejects hard. Value-parsing first (last-wins on dupes) makes the
    /// output resilient.
    fn parse_tolerant(raw: &str) -> Result<Synthesis, PipelineError> {
        let v: serde_json::Value = serde_json::from_str(&extract_json(raw))
            .map_err(|e| PipelineError::SynthesisFailed(format!("json parse: {e}")))?;
        let mut s: Synthesis = serde_json::from_value(v)
            .map_err(|e| PipelineError::SynthesisFailed(format!("json shape: {e}")))?;
        if s.title.trim().is_empty() {
            s.title = title_from_summary(&s.summary);
        }
        Ok(s)
    }

    /// Full synthesis with one repair retry on JSON parse failure.
    pub async fn synthesize(
        &self,
        source: &crate::fetcher::FetchedArticle,
        related: &[crate::fetcher::FetchedArticle],
    ) -> Result<Synthesis, PipelineError> {
        let prompt = build_prompt(source, related);
        let raw = self.chat_json(SYSTEM_PROMPT, &prompt).await?;
        match Self::parse_tolerant(&raw) {
            Ok(s) => Ok(s),
            Err(first) => {
                // Repair retry: "return valid JSON only".
                let repair = format!("{prompt}\n\nYour previous response was not valid JSON: {first}\nReturn valid JSON only.");
                let raw2 = self.chat_json(SYSTEM_PROMPT, &repair).await?;
                Self::parse_tolerant(&raw2)
            }
        }
    }
}

#[async_trait::async_trait]
impl Llm for LlmClient {
    async fn chat_json(&self, system: &str, user: &str) -> Result<String, PipelineError> {
        LlmClient::chat_json(self, system, user).await
    }

    async fn synthesize(
        &self,
        source: &crate::fetcher::FetchedArticle,
        related: &[crate::fetcher::FetchedArticle],
    ) -> Result<Synthesis, PipelineError> {
        LlmClient::synthesize(self, source, related).await
    }
}

/// Some models wrap JSON in ```json fences or prose — extract the first
/// balanced {...} block.
pub fn extract_json(raw: &str) -> String {
    let start = raw.find('{');
    let end = raw.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => raw[s..=e].to_string(),
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_balanced_json_from_fence() {
        let raw = "```json\n{\"summary\": \"hi\"}\n```";
        assert_eq!(extract_json(raw), "{\"summary\": \"hi\"}");
    }

    #[test]
    fn extract_leaves_pure_json_alone() {
        let raw = "{\"a\": 1}";
        assert_eq!(extract_json(raw), raw);
    }

    #[test]
    fn prompt_contains_corpus_urls() {
        let src = crate::fetcher::FetchedArticle {
            url: "https://src.example/1".into(),
            title: "T".into(),
            published_date: None,
            author: None,
            language: None,
            text: "source text".into(),
        };
        let rel = vec![crate::fetcher::FetchedArticle {
            url: "https://rel.example/2".into(),
            title: "R".into(),
            published_date: None,
            author: None,
            language: None,
            text: "related text".into(),
        }];
        let p = build_prompt(&src, &rel);
        assert!(p.contains("https://src.example/1"));
        assert!(p.contains("https://rel.example/2"));
    }

    #[test]
    fn synthesis_roundtrip() {
        let s = Synthesis {
            title: "t".into(),
            summary: "s".into(),
            deep_analysis: "d".into(),
            critique: "c".into(),
            citations: vec![Citation {
                url: "https://x".into(),
                context: "claim".into(),
            }],
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: Synthesis = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
