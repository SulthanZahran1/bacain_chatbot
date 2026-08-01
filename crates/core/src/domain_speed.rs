//! Domain-speed mechanics (§6) — the "speed" rule, precise.
//!
//! "Speed of a domain" = how fresh a related article must be to be worth
//! reading, encoded as a freshness window. Static, configurable table,
//! overridable via `DOMAIN_SPEED_JSON`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainSpeedTable {
    pub buckets: HashMap<String, Bucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub window_minutes: i64, // 0 = evergreen (no date filter)
    pub domains: Vec<String>,
}

impl Default for DomainSpeedTable {
    fn default() -> Self {
        let mut buckets = HashMap::new();
        buckets.insert(
            "breaking".into(),
            Bucket {
                window_minutes: 4320,
                domains: vec!["statuspage.io".into(), "incident.io".into()],
            },
        );
        buckets.insert(
            "fast".into(),
            Bucket {
                window_minutes: 10080,
                domains: vec![
                    "reuters.com".into(),
                    "theverge.com".into(),
                    "techcrunch.com".into(),
                    "arstechnica.com".into(),
                    "bbc.com".into(),
                ],
            },
        );
        buckets.insert(
            "standard".into(),
            Bucket {
                window_minutes: 43200,
                domains: vec![
                    "substack.com".into(),
                    "medium.com".into(),
                    "arxiv.org".into(),
                ],
            },
        );
        buckets.insert(
            "slow".into(),
            Bucket {
                window_minutes: 129600,
                domains: vec!["gov.uk".into(), "ec.europa.eu".into(), "fcc.gov".into()],
            },
        );
        buckets.insert(
            "evergreen".into(),
            Bucket {
                window_minutes: 0,
                domains: vec!["wikipedia.org".into(), "github.com".into()],
            },
        );
        DomainSpeedTable { buckets }
    }
}

/// A resolved freshness window: `recency_minutes` (None = no date filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub recency_minutes: Option<i64>,
    pub bucket: &'static str,
}

/// Resolve eTLD+1 (last-two-labels heuristic per spec §6) to a bucket window.
/// Resolution order: exact eTLD+1 → subdomain wildcard → default (30 days).
pub fn resolve_window(table: &DomainSpeedTable, url_str: &str, is_ai_topic: bool) -> Window {
    // The AI override sits on top of everything (§6 override rule).
    if is_ai_topic {
        return Window {
            recency_minutes: Some(43_200),
            bucket: "ai-override",
        };
    }
    let Some(etld1) = etld_plus_one(url_str) else {
        return Window {
            recency_minutes: Some(43_200),
            bucket: "default",
        };
    };
    for (name, b) in &table.buckets {
        for d in &b.domains {
            let d = d.to_lowercase();
            if d.starts_with("*.") {
                let base = d.trim_start_matches("*.");
                if etld1 == base || etld1.ends_with(&format!(".{base}")) {
                    return Window {
                        recency_minutes: (b.window_minutes > 0).then_some(b.window_minutes),
                        bucket: bucket_static(name),
                    };
                }
            } else if etld1 == d {
                return Window {
                    recency_minutes: (b.window_minutes > 0).then_some(b.window_minutes),
                    bucket: bucket_static(name),
                };
            }
        }
    }
    Window {
        recency_minutes: Some(43_200),
        bucket: "default",
    }
}

fn bucket_static(name: &str) -> &'static str {
    match name {
        "breaking" => "breaking",
        "fast" => "fast",
        "standard" => "standard",
        "slow" => "slow",
        "evergreen" => "evergreen",
        _ => "default",
    }
}

/// Last-two-labels eTLD+1 heuristic (spec-sanctioned for v1). Handles common
/// two-part TLDs (co.uk, com.au, …) via a small suffix list.
pub fn etld_plus_one(url_str: &str) -> Option<String> {
    let parsed =
        url::Url::parse(url_str.trim().trim_start_matches('<').trim_end_matches('>')).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    let multi = [
        "co.uk", "org.uk", "gov.uk", "ac.uk", "com.au", "net.au", "org.au", "co.jp", "co.nz",
        "com.br", "com.mx", "co.in", "com.sg", "com.hk", "co.kr", "com.tr", "com.pl", "com.ar",
        "com.tw", "com.vn", "co.za", "com.my", "com.ph", "co.id", "or.id", "web.id",
    ];
    if labels.len() >= 3 {
        let tail = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
        if multi.contains(&tail.as_str()) {
            return Some(format!(
                "{}.{}.{}",
                labels[labels.len() - 3],
                labels[labels.len() - 2],
                labels[labels.len() - 1]
            ));
        }
    }
    Some(format!(
        "{}.{}",
        labels[labels.len() - 2],
        labels[labels.len() - 1]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(url: &str, ai: bool) -> Window {
        resolve_window(&DomainSpeedTable::default(), url, ai)
    }

    #[test]
    fn ai_override_always_30d() {
        for u in [
            "https://reuters.com/x",
            "https://wikipedia.org/x",
            "https://unknown.example/x",
        ] {
            let w = w(u, true);
            assert_eq!(w.recency_minutes, Some(43_200), "{u}");
            assert_eq!(w.bucket, "ai-override");
        }
    }

    #[test]
    fn fast_domain_is_7d() {
        let w = w("https://www.techcrunch.com/2026/01/01/story", false);
        assert_eq!(w.recency_minutes, Some(10_080));
        assert_eq!(w.bucket, "fast");
    }

    #[test]
    fn unknown_domain_defaults_30d() {
        let w = w("https://some-random-blog.example/post", false);
        assert_eq!(w.recency_minutes, Some(43_200));
        assert_eq!(w.bucket, "default");
    }

    #[test]
    fn evergreen_has_no_date_filter() {
        let w = w("https://en.wikipedia.org/wiki/Foo", false);
        assert_eq!(w.recency_minutes, None);
        assert_eq!(w.bucket, "evergreen");
    }

    #[test]
    fn wildcard_subdomain_matches() {
        let mut t = DomainSpeedTable::default();
        t.buckets.insert(
            "standard".into(),
            Bucket {
                window_minutes: 43_200,
                domains: vec!["*.substack.com".into()],
            },
        );
        let w = resolve_window(&t, "https://foo.substack.com/p/x", false);
        assert_eq!(w.bucket, "standard");
        let w = resolve_window(&t, "https://substack.com/p/x", false);
        assert_eq!(w.bucket, "standard");
    }

    #[test]
    fn etld1_two_part_tlds() {
        assert_eq!(
            etld_plus_one("https://www.bbc.co.uk/news"),
            Some("bbc.co.uk".into())
        );
        assert_eq!(
            etld_plus_one("https://blog.example.com.au/x"),
            Some("example.com.au".into())
        );
        assert_eq!(
            etld_plus_one("https://reuters.com/x"),
            Some("reuters.com".into())
        );
    }
}
