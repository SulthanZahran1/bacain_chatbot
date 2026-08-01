//! Citation validation — the "legit" guarantee (§8), enforced mechanically:
//!
//! 1. The citation pool is exactly {source article} ∪ {successfully fetched
//!    search hits}. Nothing else can be cited.
//! 2. The system prompt forbids out-of-pool URLs.
//! 3. Post-generation validation prunes any URL not in the pool (backstop).

use crate::error::PipelineError;
use crate::normalize_url;
use crate::synthesizer::{Citation, Synthesis};

/// The pool of citable URLs: normalized forms of everything that was actually
/// fetched and read.
#[derive(Debug, Clone, Default)]
pub struct CitationPool {
    /// normalized URL → original URL (exact as fetched)
    entries: std::collections::HashMap<String, String>,
}

impl CitationPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, original_url: &str) -> bool {
        let Some(norm) = normalize_url(original_url) else {
            return false;
        };
        self.entries
            .entry(norm)
            .or_insert_with(|| original_url.to_string());
        true
    }

    pub fn contains(&self, url: &str) -> bool {
        let Some(norm) = normalize_url(url) else {
            return false;
        };
        self.entries.contains_key(&norm)
    }

    pub fn original(&self, url: &str) -> Option<&str> {
        let norm = normalize_url(url)?;
        self.entries.get(&norm).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn all_urls(&self) -> Vec<String> {
        self.entries.values().cloned().collect()
    }
}

/// Validate a synthesis against the pool. Returns (validated, rejected).
/// Out-of-pool citations are DROPPED, never passed through.
pub fn validate(s: &mut Synthesis, pool: &CitationPool) -> (Vec<Citation>, Vec<Citation>) {
    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    for c in s.citations.drain(..) {
        if pool.contains(&c.url) {
            // Replace with the exact pool URL (verbatim guarantee).
            let exact = pool.original(&c.url).unwrap_or(&c.url);
            kept.push(Citation {
                url: exact.to_string(),
                context: c.context,
            });
        } else {
            rejected.push(c);
        }
    }
    s.citations = kept.clone();
    (kept, rejected)
}

/// True if every citation in the synthesis is in the pool.
pub fn all_legit(s: &Synthesis, pool: &CitationPool) -> bool {
    s.citations.iter().all(|c| pool.contains(&c.url))
}

/// Build the pool from the source article + fetched related articles.
pub fn pool_from(
    source: &crate::fetcher::FetchedArticle,
    related: &[crate::fetcher::FetchedArticle],
) -> CitationPool {
    let mut p = CitationPool::new();
    p.insert(&source.url);
    for a in related {
        p.insert(&a.url);
    }
    p
}

/// Re-synthesize with a reduced pool after wholesale pruning:
/// returns the reduced citation list so the caller can re-run synthesis.
pub fn reduced_pool_error() -> PipelineError {
    PipelineError::SynthesisFailed("citations pruned to empty".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> CitationPool {
        let mut p = CitationPool::new();
        p.insert("https://real.example/a");
        p.insert("https://real.example/b?x=1");
        p
    }

    #[test]
    fn in_pool_citations_pass() {
        let mut s = Synthesis {
            summary: "".into(),
            deep_analysis: "".into(),
            critique: "".into(),
            citations: vec![
                Citation {
                    url: "https://real.example/a".into(),
                    context: "c1".into(),
                },
                Citation {
                    url: "https://real.example/b?x=1".into(),
                    context: "c2".into(),
                },
            ],
        };
        let p = pool();
        let (kept, rejected) = validate(&mut s, &p);
        assert_eq!(kept.len(), 2);
        assert!(rejected.is_empty());
        assert!(all_legit(&s, &p));
    }

    #[test]
    fn invented_url_is_pruned() {
        // The prompt-injection test from §13: model emits a fake URL.
        let mut s = Synthesis {
            summary: "".into(),
            deep_analysis: "".into(),
            critique: "".into(),
            citations: vec![
                Citation {
                    url: "https://real.example/a".into(),
                    context: "real".into(),
                },
                Citation {
                    url: "https://evil.example/hallucinated".into(),
                    context: "fake".into(),
                },
            ],
        };
        let p = pool();
        let (kept, rejected) = validate(&mut s, &p);
        assert_eq!(kept.len(), 1);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].url, "https://evil.example/hallucinated");
        assert!(all_legit(&s, &p));
    }

    #[test]
    fn url_mutation_is_not_allowed() {
        // "exact URL" rule: scheme/host/path mutation fails membership.
        let mut s = Synthesis {
            summary: "".into(),
            deep_analysis: "".into(),
            critique: "".into(),
            citations: vec![Citation {
                url: "http://real.example/a".into(),
                context: "scheme changed".into(),
            }],
        };
        let p = pool();
        let (kept, _) = validate(&mut s, &p);
        assert!(kept.is_empty());
    }

    #[test]
    fn normalization_does_not_break_legit_urls() {
        let mut p = CitationPool::new();
        p.insert("https://Example.com/A");
        assert!(p.contains("https://example.com/A"));
        assert!(p.contains("https://example.com/A#frag"));
    }

    #[test]
    fn source_article_is_citable() {
        let src = crate::fetcher::FetchedArticle {
            url: "https://src.example/1".into(),
            title: "".into(),
            published_date: None,
            author: None,
            language: None,
            text: "x".into(),
        };
        let p = pool_from(&src, &[]);
        assert!(p.contains("https://src.example/1"));
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn dead_fetches_never_enter_pool() {
        let src = crate::fetcher::FetchedArticle {
            url: "https://src.example/1".into(),
            title: "".into(),
            published_date: None,
            author: None,
            language: None,
            text: "x".into(),
        };
        // Only successfully-fetched articles are passed in by the pipeline;
        // a bot-blocked URL is not in `related` at all → not citable.
        let p = pool_from(&src, &[]);
        assert!(!p.contains("https://paywalled.example/dead"));
    }
}
