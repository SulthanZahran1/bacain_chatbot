//! Corpus assembly + token budgeting (§5 Stage 5).
//!
//! The source article always gets the largest share (default 50% of the
//! budget); the rest is split across related articles head+tail trimmed.

use crate::fetcher::FetchedArticle;

/// Rough chars-per-token heuristic (English ≈ 4 chars/token; spec uses
/// ~45 000 chars ≈ 60 000 tokens — i.e. 0.75 chars/token is *not* intended;
/// the spec's own example says 60k tokens ≈ 45k chars which is 0.75 token/char.
/// We keep the budget in chars to stay simple and testable: default 45 000.)
pub fn token_budget_chars(token_budget: usize) -> usize {
    // Spec: 60 000 tokens ≈ 45 000 chars → 0.75 chars per token.
    (token_budget as f64 * 0.75) as usize
}

/// Trim a single article's text head+tail to `limit` chars.
pub fn trim_head_tail(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head = limit / 2;
    let tail = limit - head;
    let mut out = String::with_capacity(limit + 8);
    out.push_str(&text.chars().take(head).collect::<String>());
    out.push_str("\n…[trimmed]…\n");
    out.push_str(
        &text
            .chars()
            .skip(text.chars().count() - tail)
            .collect::<String>(),
    );
    out
}

/// Build the corpus with the source article taking `source_share` (default 0.5).
/// Related articles are deduped by normalized URL before trimming.
pub fn assemble_corpus(
    source: &FetchedArticle,
    related: &[FetchedArticle],
    budget_chars: usize,
    source_share: f64,
) -> (String, Vec<FetchedArticle>) {
    let source_limit = ((budget_chars as f64) * source_share) as usize;
    let source_text = trim_head_tail(&source.text, source_limit);

    let mut seen = std::collections::HashSet::new();
    seen.insert(crate::normalize_url(&source.url).unwrap_or_else(|| source.url.clone()));
    let mut deduped: Vec<FetchedArticle> = Vec::new();
    for a in related {
        let key = crate::normalize_url(&a.url).unwrap_or_else(|| a.url.clone());
        if seen.insert(key) {
            deduped.push(a.clone());
        }
    }

    let remaining = budget_chars.saturating_sub(source_text.chars().count());
    let per_article = if deduped.is_empty() {
        0
    } else {
        remaining / deduped.len()
    };
    let related_trimmed: Vec<FetchedArticle> = deduped
        .into_iter()
        .map(|mut a| {
            a.text = trim_head_tail(&a.text, per_article);
            a
        })
        .collect();
    (source_text, related_trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn article(url: &str, text: &str) -> FetchedArticle {
        FetchedArticle {
            url: url.into(),
            title: String::new(),
            published_date: None,
            author: None,
            language: None,
            text: text.into(),
        }
    }

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(trim_head_tail("hello world", 100), "hello world");
    }

    #[test]
    fn long_text_is_head_tail_trimmed() {
        let long: String = "a".repeat(1000);
        let t = trim_head_tail(&long, 200);
        // head(100) + marker(13) + tail(100) = 213
        assert!(t.chars().count() <= 213);
        assert!(t.contains("[trimmed]"));
        assert!(t.starts_with("aaaa"));
        assert!(t.ends_with("aaaa"));
    }

    #[test]
    fn source_gets_half_budget() {
        let src = article("https://a.com/1", &"s".repeat(10_000));
        let rel = article("https://b.com/2", &"r".repeat(10_000));
        let (st, rt) = assemble_corpus(&src, &[rel], 10_000, 0.5);
        // 2500 + marker(13) + 2500 = 5013
        assert!(st.chars().count() <= 5_013);
        assert!(rt[0].text.chars().count() <= 5_013);
    }

    #[test]
    fn duplicates_dropped() {
        let src = article("https://a.com/1", "src");
        let rel = vec![
            article("https://b.com/2", "r1"),
            article("https://b.com/2#frag", "r1-dup"),
        ];
        let (_, rt) = assemble_corpus(&src, &rel, 10_000, 0.5);
        assert_eq!(rt.len(), 1);
    }

    #[test]
    fn empty_related_is_fine() {
        let src = article("https://a.com/1", "src text");
        let (st, rt) = assemble_corpus(&src, &[], 10_000, 0.5);
        assert_eq!(st, "src text");
        assert!(rt.is_empty());
    }
}
