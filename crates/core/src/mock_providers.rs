//! Mock providers for the scenario suite and tests (§5 Stage 9).
//!
//! The search engine is MOCKED per user directive: ranking derives from the
//! scenario's relevance scores, the window filter honors published dates, and
//! fault injection simulates dead links, rate limits, and junk results. The
//! LLM is scripted (no network).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::PipelineError;
use crate::fetcher::{FetchedArticle, Fetcher};
use crate::scenario::{Scenario, ScenarioArticle, ScenarioSource};
use crate::searcher::{FreshnessWindow, SearchHit, SearchProvider};
use crate::synthesizer::{Llm, Synthesis};

/// In-memory mock search engine built from a scenario.
///
/// Search derives ranking from each article's `relevance` to the current
/// query angle; articles whose published date falls outside the window are
/// excluded unless the window is evergreen.
#[derive(Debug)]
pub struct MockSearchProvider {
    articles: Vec<ScenarioArticle>,
    /// Per-URL fetch outcomes, injected by the scenario (`fetchable`).
    /// Used by MockFetcher; kept here for parity assertions.
    #[allow(dead_code)]
    fetchable: HashMap<String, bool>,
    /// Scripted search results per round (for adversarial scenarios).
    round_results: HashMap<usize, Vec<String>>,
    round: Mutex<usize>,
    /// Simulated rate-limit on a given call count.
    rate_limit_after: Option<usize>,
    calls: Mutex<usize>,
    pub queries_seen: Mutex<Vec<String>>,
}

impl MockSearchProvider {
    pub fn new(scenario: &Scenario) -> Self {
        let fetchable = scenario
            .corpus
            .iter()
            .map(|a| (a.url.clone(), a.fetchable))
            .collect::<HashMap<_, _>>();
        let round_results = scenario
            .overrides
            .round_overrides
            .iter()
            .map(|(r, urls)| (*r, urls.clone()))
            .collect::<HashMap<_, _>>();
        MockSearchProvider {
            articles: scenario.corpus.clone(),
            fetchable,
            round_results,
            round: Mutex::new(0),
            rate_limit_after: scenario.overrides.rate_limit_after,
            calls: Mutex::new(0),
            queries_seen: Mutex::new(Vec::new()),
        }
    }

    fn next_round(&self) -> usize {
        let mut r = self.round.lock().unwrap();
        *r += 1;
        *r
    }

    fn maybe_rate_limit(&self) -> Result<(), PipelineError> {
        if let Some(after) = self.rate_limit_after {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c > after {
                return Err(PipelineError::SearchFailed("mock 429".into()));
            }
        }
        Ok(())
    }

    fn hit(&self, a: &ScenarioArticle, window: FreshnessWindow) -> Option<SearchHit> {
        // Window filter honors published_date (proper epoch-day arithmetic).
        if let Some(recency) = window.recency_minutes {
            if let Some(d) = &a.published_date {
                let now_day = 20_666_i64; // 2026-08-01
                let cutoff_day = now_day - recency / 1440;
                if date_to_epoch_day(d) < cutoff_day {
                    return None;
                }
            }
        }
        Some(SearchHit {
            url: a.url.clone(),
            title: a.title.clone(),
            snippet: a.snippet.clone(),
            published_date: a.published_date.clone(),
        })
    }

    fn rank(&self, window: FreshnessWindow, k: usize) -> Vec<SearchHit> {
        let mut ranked: Vec<&ScenarioArticle> = self.articles.iter().collect();
        ranked.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ranked
            .into_iter()
            .filter_map(|a| self.hit(a, window))
            .take(k)
            .collect()
    }
}

#[async_trait]
impl SearchProvider for MockSearchProvider {
    async fn search(
        &self,
        queries: &[String],
        window: FreshnessWindow,
        k: usize,
        _now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        self.maybe_rate_limit()?;
        self.queries_seen
            .lock()
            .unwrap()
            .extend(queries.iter().cloned());
        let round = self.next_round();

        // Adversarial round override wins if present.
        if let Some(urls) = self.round_results.get(&round) {
            let hits: Vec<SearchHit> = urls
                .iter()
                .filter_map(|u| {
                    self.articles
                        .iter()
                        .find(|a| &a.url == u)
                        .and_then(|a| self.hit(a, window))
                })
                .take(k)
                .collect();
            return Ok(hits);
        }

        let mut all = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in queries {
            let ql = q.to_lowercase();
            let mut relevant: Vec<&ScenarioArticle> = self
                .articles
                .iter()
                .filter(|a| {
                    a.angle.to_lowercase().contains(&ql)
                        || a.title.to_lowercase().contains(&ql)
                        || a.url.to_lowercase().contains(&ql)
                })
                .collect();
            relevant.sort_by(|a, b| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for h in relevant
                .into_iter()
                .filter_map(|a| self.hit(a, window))
                .take(k)
            {
                if seen.insert(h.url.clone()) {
                    all.push(h);
                }
            }
        }
        if all.is_empty() {
            // Fall back to top-ranked (sparse-topic scenarios).
            return Ok(self.rank(window, k));
        }
        Ok(all)
    }

    async fn find_similar(
        &self,
        url: &str,
        window: FreshnessWindow,
        k: usize,
        _now_unix: i64,
    ) -> Result<Vec<SearchHit>, PipelineError> {
        let src = self.articles.iter().find(|a| a.url == url);
        let Some(src) = src else {
            return Ok(vec![]);
        };
        let mut similar: Vec<&ScenarioArticle> = self
            .articles
            .iter()
            .filter(|a| a.url != url && a.angle == src.angle)
            .collect();
        similar.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(similar
            .into_iter()
            .filter_map(|a| self.hit(a, window))
            .take(k)
            .collect())
    }
}

/// Mock fetcher: succeeds iff the scenario marks the article fetchable.
#[derive(Debug)]
pub struct MockFetcher {
    fetchable: HashMap<String, bool>,
    pub fetch_calls: Mutex<usize>,
}

impl MockFetcher {
    pub fn new(scenario: &Scenario) -> Self {
        let mut fetchable = scenario
            .corpus
            .iter()
            .map(|a| (a.url.clone(), a.fetchable))
            .collect::<HashMap<_, _>>();
        // Source is always fetchable in scenarios (it was already read).
        fetchable.insert(scenario.source.url.clone(), true);
        MockFetcher {
            fetchable,
            fetch_calls: Mutex::new(0),
        }
    }

    pub fn fetch_count(&self) -> usize {
        *self.fetch_calls.lock().unwrap()
    }
}

#[async_trait]
impl Fetcher for MockFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedArticle, PipelineError> {
        *self.fetch_calls.lock().unwrap() += 1;
        match self.fetchable.get(url) {
            Some(true) => Ok(FetchedArticle {
                url: url.to_string(),
                title: format!("Title of {url}"),
                published_date: Some("2026-07-30".to_string()),
                author: None,
                language: Some("en".to_string()),
                text: format!("Full fetched text for {url}. {}", "content ".repeat(50)),
            }),
            Some(false) => Err(PipelineError::BotBlocked),
            None => Err(PipelineError::PageNotFound),
        }
    }
}

/// Mock fetcher with AI-aware source text: when the scenario's source is an
/// AI topic, the generated text contains LLM/agent keywords so the
/// classifier's keyword pass fires (deterministic, no LLM needed).
#[derive(Debug)]
pub struct AiAwareMockFetcher {
    fetchable: HashMap<String, bool>,
    source_is_ai: bool,
    source_url: String,
    pub fetch_calls: Mutex<usize>,
}

impl AiAwareMockFetcher {
    pub fn new(scenario: &Scenario) -> Self {
        let mut fetchable = scenario
            .corpus
            .iter()
            .map(|a| (a.url.clone(), a.fetchable))
            .collect::<HashMap<_, _>>();
        fetchable.insert(scenario.source.url.clone(), true);
        AiAwareMockFetcher {
            fetchable,
            source_is_ai: scenario.source.is_ai_topic,
            source_url: scenario.source.url.clone(),
            fetch_calls: Mutex::new(0),
        }
    }

    pub fn fetch_count(&self) -> usize {
        *self.fetch_calls.lock().unwrap()
    }
}

#[async_trait]
impl Fetcher for AiAwareMockFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedArticle, PipelineError> {
        *self.fetch_calls.lock().unwrap() += 1;
        match self.fetchable.get(url) {
            Some(true) => {
                let text = if self.source_is_ai && url == self.source_url {
                    // Source article: keyword-rich so AI classification fires.
                    format!(
                        "The new LLM agent framework uses transformer models with RAG embeddings. \
                         OpenAI, Anthropic and DeepSeek all fine-tune with RLHF to reduce hallucination. {}",
                        "content ".repeat(50)
                    )
                } else {
                    format!("Full fetched text for {url}. {}", "content ".repeat(50))
                };
                Ok(FetchedArticle {
                    url: url.to_string(),
                    title: format!("Title of {url}"),
                    published_date: Some("2026-07-30".to_string()),
                    author: None,
                    language: Some("en".to_string()),
                    text,
                })
            }
            Some(false) => Err(PipelineError::BotBlocked),
            None => Err(PipelineError::PageNotFound),
        }
    }
}

/// Scripted LLM for the scenario suite: returns fixed coverage/angles per
/// round, fixed seed queries, and a canned synthesis. Never touches network.
/// Routes by system-prompt marker so each pipeline LLM call gets the right
/// scripted payload.
///
/// Coverage default (when `coverage_per_round` is empty): derived from the
/// corpus actually listed in the prompt — `min(1.0, corpus_size / ground_truth)`.
/// This models a realistic assessor and lets the optimizer tune loop
/// mechanics deterministically.
#[derive(Debug)]
pub struct ScriptedLlm {
    /// Explicit scripted coverage per round (index 0 = round 1). Empty →
    /// corpus-derived default.
    pub coverage_per_round: Vec<f64>,
    pub angles: Vec<String>,
    pub seed_queries: Vec<String>,
    pub is_ai: bool,
    pub ground_truth_angles: usize,
    /// url → angle map for corpus-derived coverage.
    url_angles: HashMap<String, String>,
    /// Round counter for coverage lookups (only coverage calls increment).
    round: std::sync::atomic::AtomicUsize,
}

impl ScriptedLlm {
    pub fn new(
        coverage_per_round: Vec<f64>,
        angles: Vec<String>,
        seed_queries: Vec<String>,
        is_ai: bool,
        ground_truth_angles: usize,
        articles: &[ScenarioArticle],
    ) -> Self {
        let url_angles = articles
            .iter()
            .map(|a| (a.url.clone(), a.angle.clone()))
            .collect();
        ScriptedLlm {
            coverage_per_round,
            angles,
            seed_queries,
            is_ai,
            ground_truth_angles,
            url_angles,
            round: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn coverage_for(&self, user_prompt: &str) -> f64 {
        if !self.coverage_per_round.is_empty() {
            let round = self.round.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return self.coverage_per_round.get(round).copied().unwrap_or(1.0);
        }
        // Corpus-derived: coverage = distinct covered angles / ground truth.
        let covered = self.corpus_angles(user_prompt);
        if self.ground_truth_angles == 0 {
            return 1.0;
        }
        (covered.len() as f64 / self.ground_truth_angles as f64).min(1.0)
    }

    /// Distinct angles present in the corpus lines of the prompt.
    fn corpus_angles<'a>(&'a self, user_prompt: &'a str) -> std::collections::HashSet<&'a str> {
        let mut covered: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in user_prompt.lines() {
            if let Some(rest) = line.strip_prefix("- ") {
                if let Some(open) = rest.rfind('(') {
                    if rest.ends_with(')') {
                        let url = &rest[open + 1..rest.len() - 1];
                        if let Some(angle) = self.url_angles.get(url) {
                            covered.insert(angle.as_str());
                        }
                    }
                }
            }
        }
        covered
    }

    /// Uncovered angles — the realistic assessor's output: every ground-truth
    /// angle NOT yet in the corpus (scripted `angles` overrides this).
    fn uncovered_angles(&self, user_prompt: &str) -> Vec<String> {
        if !self.angles.is_empty() {
            return self.angles.clone();
        }
        let covered = self.corpus_angles(user_prompt);
        let mut out: Vec<String> = self
            .url_angles
            .values()
            .filter(|a| !covered.contains(a.as_str()))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn chat_json(&self, system: &str, user: &str) -> Result<String, PipelineError> {
        if system.contains("classif") {
            return Ok(format!(
                r#"{{"is_ai": {}, "reason": "scripted"}}"#,
                self.is_ai
            ));
        }
        if system.contains("extract") {
            let qs = self
                .seed_queries
                .iter()
                .map(|q| format!("\"{q}\""))
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(format!("[{qs}]"));
        }
        // Coverage assessment — the only call type that advances the round.
        let angles = self.uncovered_angles(user);
        let angles_json = angles
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            r#"{{"coverage": {}, "angles": [{}]}}"#,
            self.coverage_for(user),
            angles_json
        ))
    }

    async fn synthesize(
        &self,
        source: &FetchedArticle,
        related: &[FetchedArticle],
    ) -> Result<Synthesis, PipelineError> {
        // Cite EVERY fetched related article (plus source) so the suite
        // runner can measure true angle coverage from the citation set.
        let mut citations: Vec<crate::synthesizer::Citation> = related
            .iter()
            .map(|a| crate::synthesizer::Citation {
                url: a.url.clone(),
                context: "scripted support".into(),
            })
            .collect();
        citations.push(crate::synthesizer::Citation {
            url: source.url.clone(),
            context: "source".into(),
        });
        Ok(Synthesis {
            title: "scripted title".into(),
            summary: "scripted summary".into(),
            deep_analysis: "scripted deep analysis".into(),
            critique: "scripted critique".into(),
            citations,
        })
    }
}

/// "YYYY-MM-DD" → days since 1970-01-01 (Howard Hinnant's days-from-civil).
fn date_to_epoch_day(d: &str) -> i64 {
    let b = d.as_bytes();
    if b.len() < 10 {
        return 0;
    }
    let parse = |s: &str| s.parse::<i64>().unwrap_or(0);
    let y = parse(&d[0..4]);
    let m = parse(&d[5..7]);
    let day = parse(&d[8..10]);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Scenario source article helper.
pub fn source_article(src: &ScenarioSource) -> FetchedArticle {
    FetchedArticle {
        url: src.url.clone(),
        title: src.title.clone(),
        published_date: Some("2026-07-31".to_string()),
        author: None,
        language: Some("en".to_string()),
        text: format!(
            "Source text for {} with plenty of content to read. {}",
            src.url,
            "body ".repeat(200)
        ),
    }
}
