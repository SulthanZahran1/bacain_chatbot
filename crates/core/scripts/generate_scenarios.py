#!/usr/bin/env python3
"""Generate ~100 synthetic scenarios for the link-analysis bot optimizer suite.

Each scenario describes: a source article, the universe of related articles
that EXIST in the mocked search engine (angle + relevance — the mock's
ranking derives from these), which are fetchable, and expectations on angle
coverage and wasted fetches.

Dimensions covered (spec §5 Stage 9):
  - AI vs non-AI topics
  - every domain bucket (fast/standard/slow/evergreen/default/breaking)
  - narrow vs sprawling topics
  - sparse vs dense corpora
  - dead-link clusters, paywalled/bot-blocked hits
  - duplicate-heavy results
  - adversarial: junk round 1 + gold round 2, rate limits, diminishing returns

Deterministic: seeded RNG. Output: crates/core/scenarios/scn_*.json
"""

import json
import os
import random

rng = random.Random(20260801)

ANGLES = [
    "mechanism", "industry-reaction", "market-impact", "regulation",
    "history", "technical-details", "comparison", "future-outlook",
    "security-implications", "adoption", "criticism", "funding",
]

BUCKETS = ["fast", "standard", "slow", "evergreen", "default", "breaking"]

DOMAINS = {
    "fast": "reuters.com",
    "standard": "medium.com",
    "slow": "fcc.gov",
    "evergreen": "wikipedia.org",
    "default": "unknown-blog.example",
    "breaking": "statuspage.io",
}


def make_article(i, angle, relevance, fetchable=True, date="2026-07-28"):
    return {
        "url": f"https://news-{i}.example/article/{i}",
        "angle": angle,
        "relevance": round(relevance, 2),
        "fetchable": fetchable,
        "published_date": date,
        "title": f"Related article {i} about {angle}",
        "snippet": f"Snippet covering {angle}.",
    }


def make_scenario(scn_id, bucket, is_ai, n_angles, n_articles, dead_frac=0.0,
                  dup_frac=0.0, min_angles=None, max_wasted=None, **extra):
    """Base scenario builder.

    n_angles: ground-truth angles in the corpus
    n_articles: total related articles
    dead_frac: fraction of articles that are bot-blocked/404
    dup_frac: fraction of articles that are duplicate URLs of earlier ones
    """
    angles = rng.sample(ANGLES, min(n_angles, len(ANGLES)))
    corpus = []
    for i in range(n_articles):
        angle = angles[i % len(angles)]
        relevance = rng.uniform(0.55, 0.98)
        fetchable = rng.random() > dead_frac
        art = make_article(i, angle, relevance, fetchable)
        if dup_frac > 0 and i > 0 and rng.random() < dup_frac:
            art["url"] = corpus[rng.randrange(len(corpus))]["url"]
        # Breaking bucket: 3d window — articles must be fresh.
        if bucket == "breaking":
            art["published_date"] = "2026-07-31"
        corpus.append(art)

    # A corpus that only has 1-2 angles can't cover 5 — tune expectation.
    if min_angles is None:
        min_angles = max(1, min(n_angles, 3))
    if max_wasted is None:
        max_wasted = max(1, int(n_articles * 0.35))

    # Dead-cluster guarantee: with heavy dead_frac, force the first
    # min_angles articles (one per distinct angle slot) to be fetchable so
    # the expectation is actually reachable.
    if dead_frac >= 0.5:
        for i in range(min(min_angles, len(corpus))):
            corpus[i]["fetchable"] = True

    # Realistic loop plumbing: seed queries are the first 1-2 angles; the
    # coverage assessor dynamically reports angles NOT yet in the corpus each
    # round (ScriptedLlm.uncovered_angles), so later rounds keep searching
    # fresh angles. `overrides.angles` stays EMPTY for the dynamic path —
    # only explicitly-scripted scenarios (adversarial, rate-limit) set it.
    seed_queries = angles[:2] if len(angles) >= 2 else angles[:1]

    return {
        "id": scn_id,
        "source": {
            "url": f"https://{DOMAINS[bucket]}/story/{scn_id}",
            "domain_bucket": bucket,
            "is_ai_topic": is_ai,
            "ground_truth_angles": n_angles,
            "title": ("New LLM agent framework" if is_ai else "Quarterly industry report"),
        },
        "corpus": corpus,
        "expected": {
            "min_angles_covered": min_angles,
            "max_wasted_fetches": max_wasted,
        },
        "overrides": {
            "seed_queries": seed_queries,
        },
        **extra,
    }


def main():
    out_dir = os.path.join(os.path.dirname(__file__), "..", "scenarios")
    os.makedirs(out_dir, exist_ok=True)

    scenarios = []

    # 1. Base sweep: AI vs non-AI × every bucket (12 scenarios)
    for bucket in BUCKETS:
        for is_ai in (True, False):
            scn = make_scenario(f"scn_{len(scenarios):03d}", bucket, is_ai, 4, 14)
            scenarios.append(scn)

    # 2. Dense corpora (sprawling topics) — many angles, many articles (8)
    # Reachability: the mock returns ONE angle per query; with 2 seed queries
    # (round 1) + 3 expansion queries (round 2) and budget ≤20, at most 5
    # angles are reachable. Expect 4 (guaranteed: 2 + 2 within budget 15).
    for n_angles in (6, 8, 10, 12):
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "standard", False, n_angles, 30, min_angles=4))
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "standard", True, n_angles, 30, min_angles=4))

    # 3. Sparse corpora — few articles total (6)
    for n in (2, 3, 4):
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "default", False, 2, n, min_angles=1, max_wasted=1))
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "default", True, 2, n, min_angles=1, max_wasted=1))

    # 4. Dead-link clusters — heavy bot-block/404 (6)
    for dead in (0.5, 0.7, 0.9):
        # When most of the corpus is dead, 2 angles is a realistic target.
        min_a = 2 if dead >= 0.7 else 3
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "fast", False, 4, 20, dead_frac=dead, max_wasted=int(20 * dead), min_angles=min_a))
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "fast", True, 4, 20, dead_frac=dead, max_wasted=int(20 * dead), min_angles=min_a))

    # 5. Duplicate-heavy results (4)
    for dup in (0.3, 0.6):
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "standard", False, 4, 20, dup_frac=dup))
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "standard", True, 4, 20, dup_frac=dup))

    # 6. Narrow topics — 1-2 angles only (4)
    for n in (1, 2):
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "slow", False, n, 12, min_angles=1))
        scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "slow", True, n, 12, min_angles=1))

    # 7. Adversarial: junk round 1, gold round 2 (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 4, 16)
        junk = [a["url"] for a in scn["corpus"][:2]]
        gold = [a["url"] for a in scn["corpus"][2:]]
        scn["overrides"].update({
            "round_overrides": {1: junk, 2: gold},
        })
        scenarios.append(scn)

    # 8. Rate-limit simulation (2) — only round 1 completes → 2 angles max.
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "fast", is_ai, 4, 12, min_angles=2)
        scn["overrides"]["rate_limit_after"] = 2
        scenarios.append(scn)

    # 9. Diminishing returns: coverage reaches target fast (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 4, 12, min_angles=2)
        scn["overrides"]["coverage_per_round"] = [0.95]
        scenarios.append(scn)

    # 10. Never reaches target: coverage stays low (2)
    for is_ai in (True, False):
        # The assessor only ever suggests 2 angles, so 2 is the realistic
        # expectation — the loop keeps trying but can't find more.
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 4, 12, min_angles=2)
        scn["overrides"]["coverage_per_round"] = [0.3, 0.4, 0.5]
        scn["overrides"]["angles"] = ["mechanism", "market-impact"]
        scenarios.append(scn)

    # 11. Fast news with old dates (window filter matters) (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "fast", is_ai, 4, 15)
        for i, art in enumerate(scn["corpus"]):
            if i % 3 == 0:
                # Outside the 7d window; for AI the override is 30d so use
                # a date inside 30d but outside 7d to keep angles reachable.
                art["published_date"] = "2026-07-20" if is_ai else "2026-01-01"
        scenarios.append(scn)

    # 12. Evergreen with no date filter (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "evergreen", is_ai, 4, 12)
        for art in scn["corpus"]:
            # AI override forces a 30d window even on evergreen — so AI
            # variants need recent dates; non-AI evergreen has no filter.
            art["published_date"] = "2026-07-20" if is_ai else "2019-05-05"
        scenarios.append(scn)

    # 13. Mixed-date corpora (partial staleness) (4)
    for bucket in ("fast", "standard"):
        for is_ai in (True, False):
            # Half the corpus is outside the window → fewer reachable angles.
            min_a = 2 if bucket == "fast" else 3
            scn = make_scenario(f"scn_{len(scenarios):03d}", bucket, is_ai, 4, 16, min_angles=min_a)
            for i, art in enumerate(scn["corpus"]):
                if i % 2 == 0:
                    art["published_date"] = "2026-07-15"
            scenarios.append(scn)

    # 14. High-relevance junk: irrelevant-but-fetchable first (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 4, 16)
        # First 3 articles: junk angle, high relevance (search noise)
        for i in range(3):
            scn["corpus"][i]["angle"] = "unrelated-noise"
            scn["corpus"][i]["relevance"] = 0.99
        scenarios.append(scn)

    # 15. All-fetchable dense (best case) (4)
    for n_angles in (5, 7, 9, 11):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", False, n_angles, 24, dead_frac=0.0)
        scenarios.append(scn)

    # 16. All-dead except source (worst case) (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 3, 10, dead_frac=1.0, min_angles=0, max_wasted=10)
        scenarios.append(scn)

    # 17. Breaking bucket (incident trackers) (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "breaking", is_ai, 3, 10, min_angles=2)
        for art in scn["corpus"]:
            # Breaking window is 3d (4320 min); AI override is 30d. Use dates
            # inside the 3d window so hits are findable either way.
            art["published_date"] = "2026-07-31"
        scenarios.append(scn)

    # 18. Slow/regulatory buckets (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "slow", is_ai, 4, 12)
        for art in scn["corpus"]:
            # 90d window (non-AI): May 15 is 78 days back — inside. AI
            # override (30d) needs July.
            art["published_date"] = "2026-07-15" if is_ai else "2026-05-15"
        scenarios.append(scn)

    # 19. Mixed buckets in corpus (4)
    for bucket in ("fast", "standard", "slow", "evergreen"):
        scn = make_scenario(f"scn_{len(scenarios):03d}", bucket, False, 4, 14)
        scenarios.append(scn)

    # 20. Adversarial: rate limit mid-loop (2) — round 3 dies → 2-3 angles.
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "fast", is_ai, 4, 14, min_angles=2)
        scn["overrides"]["rate_limit_after"] = 5
        scenarios.append(scn)

    # 21. Sparse + dead-heavy (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 3, 8, dead_frac=0.5, min_angles=1, max_wasted=7)
        scenarios.append(scn)

    # 22. Adversarial: gold round 1, junk round 2 (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 4, 16)
        gold = [a["url"] for a in scn["corpus"][:8]]
        junk = [a["url"] for a in scn["corpus"][8:10]]
        scn["overrides"].update({
            "round_overrides": {1: gold, 2: junk},
        })
        scenarios.append(scn)

    # 23. High angle-count with dead cluster (2) — 30% dead → 3 angles reachable.
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 8, 26, dead_frac=0.3, min_angles=3)
        scenarios.append(scn)

    # 24. Duplicate + dead mix (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 4, 18, dup_frac=0.3, dead_frac=0.3)
        scenarios.append(scn)

    # 25. Single-query narrow AI topics (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 1, 6, min_angles=1, max_wasted=2)
        scenarios.append(scn)

    # 26. Two-query broad non-AI (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 3, 15, min_angles=2)
        scenarios.append(scn)

    # 27. Adversarial: junk first round (fetchable but low-value), gold round 2 (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 4, 14, min_angles=3)
        junk = [a["url"] for a in scn["corpus"][:3]]
        gold = [a["url"] for a in scn["corpus"][3:9]]
        scn["overrides"].update({"round_overrides": {1: junk, 2: gold}})
        scenarios.append(scn)

    # 28. High relevance but stale (window excludes everything) (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "fast", is_ai, 3, 10, min_angles=0, max_wasted=0)
        for art in scn["corpus"]:
            art["published_date"] = "2026-01-10"  # stale for both windows
        scenarios.append(scn)

    # 29. Very sparse single article (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 1, 1, min_angles=1, max_wasted=0)
        scenarios.append(scn)

    # 30. Duplicate-only corpus (1)
    scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", False, 2, 8, dup_frac=1.0, min_angles=1, max_wasted=2)
    scenarios.append(scn)

    # 31. Large budget needed (many angles, high K) (2) — 4 angles reachable
    # within the spec's grid (max budget 20, max rounds 4).
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 10, 35, min_angles=4)
        scenarios.append(scn)

    # 32. Mixed fetch outcomes per angle (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "standard", is_ai, 5, 20)
        for i, art in enumerate(scn["corpus"]):
            if i % 4 == 0:
                art["fetchable"] = False  # one dead per angle group
        scenarios.append(scn)

    # 33. Breaking + AI (fast-moving agentic incidents) (1)
    scenarios.append(make_scenario(f"scn_{len(scenarios):03d}", "breaking", True, 4, 14, min_angles=3))

    # 34. Evergreen + AI (docs/guides about agents) (1)
    scn = make_scenario(f"scn_{len(scenarios):03d}", "evergreen", True, 4, 12, min_angles=3)
    for art in scn["corpus"]:
        art["published_date"] = "2026-07-25"
    scenarios.append(scn)

    # 35. Adversarial: junk every round (2)
    for is_ai in (True, False):
        scn = make_scenario(f"scn_{len(scenarios):03d}", "default", is_ai, 4, 14)
        junk = [a["url"] for a in scn["corpus"][:3]]
        scn["overrides"] = {
            "round_overrides": {1: junk, 2: junk, 3: junk},
            "seed_queries": ["seed"],
        }
        scenarios.append(scn)

    # Write files
    written = 0
    for scn in scenarios:
        path = os.path.join(out_dir, f"{scn['id']}.json")
        with open(path, "w") as f:
            json.dump(scn, f, indent=2)
        written += 1

    print(f"wrote {written} scenarios to {out_dir}")
    # Sanity: every scenario id unique
    ids = [s["id"] for s in scenarios]
    assert len(ids) == len(set(ids)), "duplicate scenario ids"


if __name__ == "__main__":
    main()
