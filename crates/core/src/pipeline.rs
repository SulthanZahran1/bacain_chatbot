//! The pipeline orchestrator — the coverage-feedback search loop (§5 Stage 4)
//! wired into fetch → classify → search → read → synthesize → validate.
//!
//! The loop is an *agent* in the spec's sense: it decides how many rounds to
//! run based on a coverage signal from the LLM, with hard caps (MAX_ROUNDS,
//! SEARCH_BUDGET) and a 60 s deadline (§7 SLA).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::cache::Cache;
use crate::citations::{self, CitationPool};
use crate::classifier;
use crate::clock::Clock;
use crate::config::Config;
use crate::domain_speed::{self, Window};
use crate::error::PipelineError;
use crate::fetcher::{FetchedArticle, Fetcher};
use crate::optimizer_policy::Policy;
use crate::reader;
use crate::searcher::{FreshnessWindow, SearchHit, SearchProvider};
use crate::synthesizer::Llm;

// ---------------------------------------------------------------------------
// Public types (the crate's single async entry point)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChannelCtx {
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct AnalysisRequest {
    pub url: String,
    pub channel: ChannelCtx,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    pub url: String,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisMeta {
    pub bucket: String,
    pub window_used: String, // human-readable, e.g. "30d" / "7d" / "evergreen"
    pub recency_minutes: Option<i64>,
    pub corpus_size: usize,
    pub rounds: usize,
    pub stop_reason: String,
    pub latency_ms: u64,
    pub llm_model: String,
    pub citations_rejected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub summary: String,
    pub deep_analysis: String,
    pub critique: String,
    pub citations: Vec<Citation>,
    pub meta: AnalysisMeta,
}

/// Everything the pipeline needs — constructed once at startup, shared across
/// analyses. All providers are trait objects so tests can inject mocks.
pub struct Deps {
    pub fetcher: Arc<dyn Fetcher>,
    pub searcher: Arc<dyn SearchProvider>,
    pub llm: Arc<dyn Llm>,
    pub cache: Arc<Cache>,
    pub clock: Clock,
    pub config: Arc<Config>,
}

// ---------------------------------------------------------------------------
// Coverage assessor (LLM) contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub coverage: f64,
    pub angles: Vec<String>,
}

/// The pipeline entry point.
pub async fn analyze(req: AnalysisRequest, deps: &Deps) -> Result<Analysis, PipelineError> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);

    let url = crate::normalize_url(&req.url).ok_or(PipelineError::InvalidUrl)?;
    let now = deps.clock.now_unix();

    // ---- Stage 1 · Fetch the source --------------------------------------
    let source = deps.fetcher.fetch(&url).await?;
    check_deadline(deadline)?;

    // ---- Stage 2 · Classify ----------------------------------------------
    let title_for_class = if source.title.is_empty() {
        source.url.clone()
    } else {
        source.title.clone()
    };
    let text_head: String = source.text.chars().take(2_000).collect();
    let mut classification = classifier::classify(&title_for_class, &text_head, None);
    if classification.ambiguous {
        // LLM disambiguation; failure falls back to keyword verdict (false).
        classification = match disambiguate_via_llm(deps, &title_for_class, &text_head).await {
            Ok(c) => c,
            Err(e) => {
                warn!(?e, "llm disambiguation failed; using keyword verdict");
                classification
            }
        };
    }
    info!(
        is_ai = classification.is_ai_topic,
        score = classification.score,
        "classified"
    );

    // ---- Stage 3 · Domain speed + window ----------------------------------
    let w = domain_speed::resolve_window(
        &deps.config.domain_speed,
        &source.url,
        classification.is_ai_topic,
    );
    let freshness = FreshnessWindow {
        recency_minutes: w.recency_minutes,
        bucket: w.bucket,
    };
    info!(bucket = w.bucket, recency = ?w.recency_minutes, "window resolved");
    check_deadline(deadline)?;

    // ---- Stage 4 · Search loop ---------------------------------------------
    let mut corpus: Vec<FetchedArticle> = Vec::new();
    let mut fetched_urls: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stop_reason = String::new();
    let mut rounds = 0usize;
    let policy = deps.config.policy;

    // Seed queries: LLM extraction with title fallback (§5 Stage 4.1).
    let seed_queries = seed_queries(deps, &source, &w, policy).await;
    let mut queries = seed_queries.clone();

    let mut current_coverage: f64;
    let mut angles: Vec<String> = Vec::new();

    while rounds < policy.max_rounds {
        rounds += 1;
        let k = if rounds == 1 {
            policy.initial_k
        } else {
            policy.expansion_k
        };
        info!(round = rounds, k, "search round");

        // Search (exclude source domain so we don't re-find the article).
        let hits = match deps
            .searcher
            .search(&queries, freshness, k, deps.clock.now_unix())
            .await
        {
            Ok(h) => h,
            Err(e) => {
                // §10: search backend failure degrades gracefully — stop the
                // loop, synthesize from whatever corpus was gathered.
                warn!(?e, "search failed; stopping loop gracefully");
                stop_reason = "search-error".into();
                break;
            }
        };
        check_deadline(deadline)?;

        // Fetch hits in one batch (≤10 per call), drop failures.
        let mut new_articles = 0usize;
        let mut batch: Vec<SearchHit> = Vec::new();
        for h in hits {
            if fetched_urls.contains(&h.url) {
                continue;
            }
            if fetched_urls.len() >= policy.search_budget {
                stop_reason = format!("budget({})", policy.search_budget);
                break;
            }
            batch.push(h);
        }
        for h in batch {
            if fetched_urls.len() >= policy.search_budget {
                if stop_reason.is_empty() {
                    stop_reason = format!("budget({})", policy.search_budget);
                }
                break;
            }
            // Mark attempted BEFORE fetching so failed URLs are never
            // re-fetched in later rounds (real search wouldn't re-serve
            // dead links we already tried).
            fetched_urls.insert(h.url.clone());
            match deps.fetcher.fetch(&h.url).await {
                Ok(a) => {
                    // Drop empty/failed fetches from corpus entirely.
                    if a.text.trim().is_empty() {
                        continue;
                    }
                    new_articles += 1;
                    corpus.push(a);
                }
                Err(e) => {
                    warn!(url = %h.url, ?e, "hit fetch failed, dropped");
                }
            }
            check_deadline(deadline)?;
        }
        if stop_reason.starts_with("budget") {
            break;
        }

        // ---- Coverage assessment (LLM) ------------------------------------
        let assess = assess_coverage(deps, &source, &corpus).await;
        match assess {
            Ok(c) => {
                current_coverage = c.coverage;
                angles = c.angles;
                info!(coverage = current_coverage, "coverage assessed");
            }
            Err(e) => {
                warn!(?e, "coverage assessment failed; treating as sufficient");
                current_coverage = 1.0; // don't loop forever on LLM failure
                stop_reason = "coverage-llm-failed".into();
            }
        }

        // ---- Stop conditions ----------------------------------------------
        if current_coverage >= policy.coverage_target {
            stop_reason = format!("coverage({current_coverage:.2})");
            break;
        }
        if new_articles < policy.min_new_articles {
            stop_reason = format!("diminishing({new_articles})");
            break;
        }
        if rounds >= policy.max_rounds {
            stop_reason = format!("max_rounds({rounds})");
            break;
        }
        // Next round: search each uncovered angle.
        queries = angles
            .iter()
            .take(3)
            .map(|a| a.to_string())
            .collect::<Vec<_>>();
        if queries.is_empty() {
            queries = seed_queries.clone();
        }
    }
    if stop_reason.is_empty() {
        stop_reason = format!("max_rounds({rounds})");
    }

    // ---- Stage 5 · Reader: token budget trim -------------------------------
    let budget_chars = reader::token_budget_chars(deps.config.corpus_token_budget);
    let (source_text, related_trimmed) =
        reader::assemble_corpus(&source, &corpus, budget_chars, 0.5);
    let mut source_trimmed = source.clone();
    source_trimmed.text = source_text;

    // ---- Stage 6+7 · Synthesize + validate ---------------------------------
    let synthesis = deps
        .llm
        .synthesize(&source_trimmed, &related_trimmed)
        .await?;
    let mut pool = CitationPool::new();
    pool.insert(&source.url);
    for a in &corpus {
        pool.insert(&a.url);
    }
    let mut synth = synthesis;
    let (_, rejected) = citations::validate(&mut synth, &pool);
    let rejected_count = rejected.len();

    // If a section has zero citations after pruning, one regeneration pass
    // with the reduced pool (§5 Stage 7.3).
    let mut synthesis = synth;
    if !synthesis.citations.is_empty() && rejected_count > 0 {
        // Regenerate once with the pruned citation list as guidance.
        if let Ok(regenerated) = deps.llm.synthesize(&source_trimmed, &related_trimmed).await {
            let mut reg = regenerated;
            let (_, rejected2) = citations::validate(&mut reg, &pool);
            if !reg.citations.is_empty() {
                synthesis = reg;
                let _ = rejected2;
            }
        }
    }

    // ---- Analysis assembly -------------------------------------------------
    let latency = started.elapsed().as_millis() as u64;
    let analysis = Analysis {
        summary: synthesis.summary,
        deep_analysis: synthesis.deep_analysis,
        critique: synthesis.critique,
        citations: synthesis
            .citations
            .into_iter()
            .map(|c| Citation {
                url: c.url,
                context: c.context,
            })
            .collect(),
        meta: AnalysisMeta {
            bucket: w.bucket.to_string(),
            window_used: window_label(w),
            recency_minutes: w.recency_minutes,
            corpus_size: corpus.len(),
            rounds,
            stop_reason,
            latency_ms: latency,
            llm_model: deps.config.llm_model.clone(),
            citations_rejected: rejected_count,
        },
    };

    // ---- Stage 8 · Cache write ----------------------------------------------
    if let Ok(json) = serde_json::to_string(&analysis) {
        let _ = deps.cache.put(
            &url,
            &req.channel.id,
            &json,
            &analysis.meta.window_used,
            &analysis.meta.bucket,
            now,
        );
    }

    Ok(analysis)
}

fn check_deadline(deadline: Instant) -> Result<(), PipelineError> {
    if Instant::now() >= deadline {
        return Err(PipelineError::DeadlineExceeded);
    }
    Ok(())
}

fn window_label(w: Window) -> String {
    match w.recency_minutes {
        None => "evergreen".to_string(),
        Some(43_200) => "30d".to_string(),
        Some(10_080) => "7d".to_string(),
        Some(4_320) => "3d".to_string(),
        Some(129_600) => "90d".to_string(),
        Some(m) => format!("{m}m"),
    }
}

// ---------------------------------------------------------------------------
// LLM helpers
// ---------------------------------------------------------------------------

async fn disambiguate_via_llm(
    deps: &Deps,
    title: &str,
    text: &str,
) -> Result<classifier::Classification, PipelineError> {
    let prompt = format!(
        "Is this article primarily about AI/LLM/agentic systems? Answer JSON only: {{\"is_ai\": bool, \"reason\": str}}\nTITLE: {title}\nTEXT: {}",
        text.chars().take(1500).collect::<String>()
    );
    let raw = deps
        .llm
        .chat_json("You classify articles. JSON only.", &prompt)
        .await?;
    #[derive(Deserialize)]
    struct Out {
        is_ai: bool,
    }
    let parsed: Out = serde_json::from_str(&raw)
        .map_err(|e| PipelineError::SynthesisFailed(format!("disambiguation parse: {e}")))?;
    Ok(classifier::Classification {
        is_ai_topic: parsed.is_ai,
        score: 1,
        ambiguous: false,
    })
}

async fn seed_queries(
    deps: &Deps,
    source: &FetchedArticle,
    w: &Window,
    _policy: Policy,
) -> Vec<String> {
    let _ = w;
    // LLM extraction with title fallback.
    let prompt = format!(
        "Extract 2-3 short search queries to find related articles about this topic. Return JSON array of strings only.\nTITLE: {}\nURL: {}\nTEXT: {}",
        source.title,
        source.url,
        source.text.chars().take(1200).collect::<String>()
    );
    if let Ok(raw) = deps
        .llm
        .chat_json("You extract search queries. JSON only.", &prompt)
        .await
    {
        if let Ok(queries) = serde_json::from_str::<Vec<String>>(&raw) {
            let qs: Vec<String> = queries
                .into_iter()
                .filter(|q| !q.trim().is_empty())
                .take(3)
                .collect();
            if !qs.is_empty() {
                return qs;
            }
        }
    }
    // Fallback: title alone (spec §5 Stage 4.1).
    let mut t = source.title.trim().to_string();
    if t.is_empty() {
        t = source.url.clone();
    }
    vec![t]
}

async fn assess_coverage(
    deps: &Deps,
    source: &FetchedArticle,
    corpus: &[FetchedArticle],
) -> Result<Coverage, PipelineError> {
    if corpus.is_empty() {
        return Ok(Coverage {
            coverage: 0.0,
            angles: vec!["the main topic".to_string()],
        });
    }
    let corpus_summary: Vec<String> = corpus
        .iter()
        .map(|a| format!("- {} ({})", a.title, a.url))
        .collect();
    let prompt = format!(
        "You assess research coverage. Given the source article and the corpus of related articles already gathered, score coverage 0.0-1.0 and list 2-3 uncovered angles worth searching. Return JSON only: {{\"coverage\": float, \"angles\": [str]}}.\nSOURCE TITLE: {}\nCORPUS:\n{}",
        source.title,
        corpus_summary.join("\n")
    );
    let raw = deps
        .llm
        .chat_json("You assess coverage. JSON only.", &prompt)
        .await?;
    let c: Coverage = serde_json::from_str(&raw)
        .map_err(|e| PipelineError::SynthesisFailed(format!("coverage parse: {e}")))?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_labels() {
        let w = Window {
            recency_minutes: Some(43_200),
            bucket: "ai-override",
        };
        assert_eq!(window_label(w), "30d");
        let w = Window {
            recency_minutes: Some(10_080),
            bucket: "fast",
        };
        assert_eq!(window_label(w), "7d");
        let w = Window {
            recency_minutes: None,
            bucket: "evergreen",
        };
        assert_eq!(window_label(w), "evergreen");
    }
}
