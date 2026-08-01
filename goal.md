# goal.md — Discord Link Analysis Bot

**Status:** Design complete — ready for implementation
**Stack:** Rust (locked) · TinyFish (Fetch + Search) · any OpenAI-compatible LLM · SQLite · Docker
**Generated:** 2026-08-01

---

## 1. Goal (the destination)

A Discord bot that, whenever someone drops a link in a channel, autonomously:

1. **Detects** the link in the message.
2. **Retrieves** the article content via **TinyFish Fetch** (clean markdown + metadata).
3. **Classifies** the topic: if it is agentic/LLM/AI-related → search the **latest 30 days**; otherwise → search with a freshness window derived from the **"speed" of the source domain**.
4. **Reads** all corresponding articles (search results fetched through TinyFish).
5. **Responds** with a structured analysis:
   - **(a) Summary** — 1 paragraph
   - **(b) Deep Analysis** — 3–4 paragraphs
   - **(c) Critique** — 1–2 paragraphs
6. **Cites** other sources where possible — with a hard guarantee that every cited source is **real and verified** (see §8).

The outcome of this document is a complete frontend + backend architecture, the end-to-end flow, and an unambiguous **Definition of Done** (§15).

---

## 2. Architecture overview

The bot is a **Cargo workspace with two crates**, enforcing a clean boundary between the Discord-facing layer and the testable business core:

```
discord-link-bot/
├── Cargo.toml                  # workspace
├── crates/
│   ├── core/                   # BACKEND — pure pipeline, no Discord dependency
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── pipeline.rs     # orchestrator: stages, timeouts, cancellation
│   │   │   ├── fetcher.rs      # TinyFish Fetch client (reqwest)
│   │   │   ├── classifier.rs   # AI/agentic/LLM topic detection
│   │   │   ├── searcher.rs     # SearchProvider trait + Exa impl (agentic loop)
│   │   │   ├── domain_speed.rs # domain → freshness bucket table
│   │   │   ├── reader.rs       # corpus assembly + token budgeting
│   │   │   ├── synthesizer.rs  # LLM chat completion, structured JSON output
│   │   │   ├── citations.rs    # citation pool + validator (legit-only)
│   │   │   ├── cache.rs        # SQLite dedupe + cooldown
│   │   │   ├── config.rs       # env parsing, domain-speed table, prompts, loop policy
│   │   │   ├── optimizer.rs    # offline policy sweep on the scenario suite (§5 Stage 9)
│   │   │   ├── mock_providers.rs # MockSearchProvider/MockFetcher + fault injection (§5 Stage 9)
│   │   │   ├── clock.rs        # injectable now() — deterministic windows in tests
│   │   │   └── error.rs        # error taxonomy (maps to user-facing messages)
│   │   ├── scenarios/          # ~100 synthetic scenario JSONs + generator script
│   │   └── tests/              # unit + mocked integration + scenario suite runner
│   └── bot/                    # FRONTEND — Discord surface
│       └── src/
│           ├── main.rs         # bootstrap: config, tracing, serenity client
│           ├── events/
│           │   ├── message.rs  # link detection, gates (allowlist/cooldown/dedupe)
│           │   └── interaction.rs  # slash commands: /analyze /status /config
│           └── ui/
│               ├── render.rs   # analysis → Discord messages/embeds, 2000-char split
│               └── threads.rs  # thread-per-analysis management
├── Dockerfile                  # multi-stage: rust build → debian-slim runtime
├── docker-compose.yml
└── README.md
```

### ASCII flow

```
┌───────────────────────── FRONTEND (Discord surface) ─────────────────────────┐
│  serenity gateway → message events → link detector → gates (channel/cooldown)│
│  slash commands (/analyze /status /config)      UI renderer → threads/splits │
└───────────────────────────────────┬───────────────────────────────────────────┘
                                    │ trigger                │ rendered output
┌───────────────────────────────────▼───────────────────────────────────────────┐
│                           BACKEND (pipeline core)                             │
│  ┌──────────┐   ┌──────────────┐   ┌───────────┐   ┌────────────┐   ┌───────────────┐
│  │ Fetcher  │ → │ Classifier   │ → │ Searcher  │ → │ Reader     │ → │ Synthesizer   │
│  │ tinyfish │   │ AI-topic?    │   │ Exa·loop  │   │ corpus     │   │ LLM → 3-part  │
│  │ fetch    │   │ keyword+LLM  │   │ windows   │   │ build+trim │   │ structured    │
│  └──────────┘   └──────────────┘   └───────────┘   └────────────┘   └───────┬───────┘
│  ┌───────────────────────────────────────────────────────────────────────────▼─────┐
│  │                      Citation Validator (legit-only, §8)                          │
│  └───────────────────────────────────────────────────────────────────────────────────┘
│  cache (SQLite) · domain-speed table · config · tracing · error taxonomy            │
└──────────────────────────────────────────────────────────────────────────────────────┘
(loop: Reader → Searcher — the coverage assessor decides expand or stop, §5 Stage 4;
 policy tuned offline by the optimizer on ~100 mocked scenarios, §5 Stage 9.)
```

**Why "frontend/backend" framing:** the bot has no web UI — its frontend *is* the Discord surface (event listeners, gates, rendering). The backend is the pipeline core, fully unit-testable without a Discord connection. The two crates communicate only through one async entry point:

```rust
// crates/core/src/pipeline.rs
pub struct AnalysisRequest { pub url: String, pub channel: ChannelCtx }
pub struct Analysis {
    pub summary: String,
    pub deep_analysis: String,
    pub critique: String,
    pub citations: Vec<Citation>,   // every entry verified (§8)
    pub meta: AnalysisMeta,         // bucket used, window used, corpus size, latency
}
pub async fn analyze(req: AnalysisRequest, deps: &Deps) -> Result<Analysis, PipelineError>;
```

---

## 3. Key decisions (locked, with rationale)

| # | Decision | Choice | Alternative (flagged) |
|---|----------|--------|----------------------|
| D1 | Language | **Rust** (user-locked) | — |
| D2 | Discord lib | **serenity 0.12** (current, MSRV 1.74) | poise (command framework on top of serenity — optional nicety) |
| D3 | Retrieval | **TinyFish Fetch** (mandated by brief) — 10 URLs/req, markdown, per-URL error codes, JS rendering | — |
| D4 | Search | **Exa (default)** — `startPublishedDate`/`endPublishedDate` native date windows, `category=news` for fast/breaking buckets, `findSimilar` as an expansion signal; no official Rust SDK → reqwest + serde | TinyFish Search (`recency_minutes`/`after_date`/`before_date`, same vendor as Fetch) or Tavily — behind `SearchProvider` trait, swap = config |
| D5 | LLM | **Any OpenAI-compatible endpoint** via `async-openai` `OpenAIConfig::with_api_base()` (env: `LLM_API_BASE`, `LLM_API_KEY`, `LLM_MODEL`) | Works with DeepSeek, OpenRouter, local vLLM/Ollama — no code change |
| D6 | AI-topic window | **Fixed 30 days** (`recency_minutes=43200`) per spec | — |
| D7 | Non-AI window | **Domain-speed buckets** (§6) | — |
| D8 | Reply mode | **Thread per analysis, anchored to the original message** (`⏳` emoji reaction as placeholder; the thread's start message *is* the original message, so it reads as a reply) | `split` mode (`⏳` reaction + sequential reply messages) — config switch |
| D9 | State | **SQLite** (`rusqlite` bundled): URL dedupe, cooldown, analysis cache | — |
| D10 | Deploy | **Docker Compose** (multi-stage build) | systemd unit |
| D11 | Topic detection | Keyword scoring + LLM disambiguation for ambiguous scores (§5) | pure keyword, pure LLM |

---

## 4. Frontend — Discord layer (crate `bot`)

### 4.1 Message pipeline

1. **Link detection** — regex over message content for `https?://` (unfurl-safe: also strip `<>` wrappers Discord adds). First URL per message wins; `/analyze` command overrides.
2. **Gates** (all checked before any network call, cheapest first):
   - **Channel gate** — config allowlist/denylist (`ANALYZE_CHANNELS`, or `ALLOW_ALL=true`).
   - **Bot-self gate** — never respond to bot messages (including our own thread posts and reactions).
   - **Cooldown** — per-channel, default 60 s between analyses (`COOLDOWN_SECS`).
   - **Dedupe** — SQLite: same URL analyzed within `CACHE_TTL_HOURS` (default 24) → re-post cached analysis instead of re-running.
3. **Trigger** — call `core::pipeline::analyze(...)` on a tokio task; **never block the gateway event loop**.
4. **Placeholder (emoji, not literal)** — react to the original message with the `⏳` emoji (`Message::react`). **No text placeholder is ever posted.** The reaction state *is* the status: `⏳` = in progress; removed when done; replaced with `❌` on terminal failure.
5. **Thread (replies the original message)** — create a thread anchored to the original message via `create_thread_from_message` (serenity 0.12 `ChannelId`): the original message becomes the thread's start message, so the thread is literally attached to — i.e. replies to — the message. Neutral name `📚 Link analysis` at creation; renamed to `📚 <article title>` once fetch returns the title (best effort). All analysis output posts into this thread.

### 4.2 Slash commands (serenity interactions)

| Command | Behavior |
|---|---|
| `/analyze <url>` | Force analysis regardless of message link (bypasses cooldown for the *link* but still dedupes) |
| `/status` | Latency, last N analyses (URL, bucket, window, corpus size, LLM model, duration) |
| `/config` | Per-channel on/off, reply mode (thread/split) — persisted to SQLite |
| `/ping` | Liveness (for uptime checks) |

### 4.3 Rendering (`ui/render.rs`)

Discord hard limit: **2000 chars/message**. Analysis structure → one thread:

- **Thread:** created anchored to the original message (it is the thread's start message), so the thread reads as a reply to it. Title: neutral `📚 Link analysis` at creation, renamed to `📚 <article title>` (truncate ~80 chars) once fetched.
- **Msg 1 — Summary:** title + source link + 1-paragraph summary (embed: `description`, color-coded by sentiment/topic).
- **Msg 2 — Deep Analysis:** 3–4 paragraphs (split on paragraph boundaries if >2000 chars).
- **Msg 3 — Critique:** 1–2 paragraphs.
- **Msg 4 — Sources:** numbered list `[1] title — url` for every citation (§8). Only verified URLs.

Each section carries a small header (bold `## Summary` etc.) for scannability. A `⏱` footer line shows the freshness window actually used (proves the 30-day/domain-speed rule fired).

---

## 5. Backend — pipeline core (crate `core`)

### Stage 1 · Fetch the source (`fetcher.rs`)

POST `https://api.fetch.tinyfish.ai` (10 URLs max/request, but stage 1 is a single URL):

```json
{ "urls": ["https://…"], "format": "markdown", "ttl": 3600 }
```

- Response gives `title`, `published_date`, `author`, `language`, `text` (clean markdown).
- **Per-URL error codes** map directly into the user-facing error taxonomy (§10): `page_not_found`, `target_unreachable`, `bot_blocked`, `empty_content`, `timeout`, `invalid_url`, `target_http_error`, `proxy_error`.
- **SSRF is TinyFish's problem, not ours** — the bot never fetches a raw URL itself; TinyFish rejects private IPs/localhost/metadata endpoints. State this in the README as a security property.
- Retry: transient errors (`timeout`, `proxy_error`, 5xx) → 1 retry with backoff; `per_url_timeout_ms: 45000`.

### Stage 2 · Classify the topic (`classifier.rs`)

Input: title + first ~2 KB of text. Output: `is_ai_topic: bool` (+ confidence).

- **Pass 1 (keyword scoring):** weighted term list — `LLM`, `GPT`, `agent`, `agentic`, `model`, `neural`, `transformer`, `RLHF`, `fine-tun`, `hallucinat`, `embedding`, `token`, `inference`, `prompt`, `RAG`, `diffusion`, `multimodal`, vendor names (`OpenAI`, `Anthropic`, `Google DeepMind`, `Meta AI`, `Mistral`, `xAI`, `Hugging Face`)… Score = hits weighted by position (title hits ×3).
- **Pass 2 (LLM disambiguation):** only when score is in the ambiguous band (configurable, e.g. 1–3 hits) — one cheap LLM call: `"Is this article primarily about AI/LLM/agentic systems? Answer JSON: {\"is_ai\": bool, \"reason\": str}"`.
- Clear high/low scores skip the LLM call (cost + latency).

### Stage 3 · Domain speed + search window (`domain_speed.rs`)

Resolve `eTLD+1` of the source URL (use the `url` crate + a public-suffix list, or just last-two-labels heuristic for v1) → look up bucket:

| Bucket | Window (`recency_minutes`) | Example domains (default config, editable) |
|---|---|---|
| `breaking` | 3 days | live incident trackers, status pages |
| `fast` | 7 days | reuters.com, theverge.com, techcrunch.com, arstechnica.com, bbc.com |
| `standard` | 30 days | dev blogs, substack.com, medium.com, arxiv.org (non-AI), HN-linked blogs |
| `slow` | 90 days | company announcements, government/regulatory, standards bodies |
| `evergreen` | **no date filter** | wikipedia.org, official docs, academic journals, github.com |
| `default` (unknown) | 30 days | anything not in the table |

**Override rule (the spec's core rule):** `is_ai_topic == true` → **always 30 days** (`recency_minutes = 43200`), regardless of bucket. Log which rule fired — it is part of `AnalysisMeta` and the `⏱` footer.

### Stage 4 · Search (agentic loop — `searcher.rs`)

`SearchProvider` trait (swap = config change; default impl is **Exa**):

```rust
pub trait SearchProvider: Send + Sync {
    async fn search(&self, queries: &[String], window: FreshnessWindow, k: usize) -> Result<Vec<SearchHit>, SearchError>;
    async fn find_similar(&self, url: &str, window: FreshnessWindow, k: usize) -> Result<Vec<SearchHit>, SearchError>;
}
pub struct SearchHit { pub url: String, pub title: String, pub snippet: String, pub published_date: Option<String> }
```

**Default impl — Exa (`POST https://api.exa.ai/search`):** native `startPublishedDate`/`endPublishedDate` (ISO 8601) for the freshness window, `category=news` for `fast`/`breaking` buckets, `numResults=k`, `type=auto`. No official Rust SDK → thin `reqwest` + `serde` client. Rate limits honored with backoff (429 → honor `Retry-After`). Secondary impl (TinyFish Search, `recency_minutes`/`after_date`/`before_date` + `domain_type=news`) ships for fallback; both share the trait.

**The loop is a coverage-feedback loop, not a fixed plan.** Policy parameters (all env-tunable, §9) come from the optimizer (§5 Stage 9) — the agent (the loop logic, not the LLM) decides how many rounds to run:

1. **Seed queries** — LLM extracts 2–3 search queries from the article (batched into the synthesis stage's LLM call if timing allows); fallback: article title alone. `-site:<source_domain>` excluded so we don't re-find the article.
2. **Round 1** — search all queries, `k = INITIAL_K` (default 5). Fetch the hits (§5 Stage 5) — **only fetched-and-read articles count toward coverage**.
3. **Coverage assessment** — an LLM call scores the current corpus against the article: `{"coverage": 0.0–1.0, "angles": ["…"]}` (angles = uncovered aspects of the article that related articles could illuminate).
4. **Expand while it pays** — if `coverage < COVERAGE_TARGET` (default 0.85) and `round < MAX_ROUNDS` (default 3) and the previous round **added** at least `MIN_NEW_ARTICLES` (default 1) *distinct, relevant* articles: search each uncovered angle (from step 3) with `k = EXPANSION_K` (default 3), optionally seeded by `findSimilar` on the best corpus article; loop back to step 3.
5. **Stop conditions** (whichever first): coverage ≥ target · max rounds reached · a round added nothing new (diminishing returns — **the "not oversearch" guard**) · `SEARCH_BUDGET` (default 15) distinct URLs fetched (hard cap).
6. **Bias toward over-search, not under** (user directive): the loop's default posture reads a bit more than strictly needed — the cap and stop conditions bound it, but MIN_NEW_ARTICLES and the coverage target are tuned so a typical analysis reads 6–12 articles rather than 3–5, and the optimizer (Stage 9) is scored to prefer extra reads over missed angles (see the utility function there). "Too much is better than too little."

**Budgeting:** total URLs fetched across all rounds ≤ `SEARCH_BUDGET`; total rounds ≤ `MAX_ROUNDS`; the 60 s SLA (§7) remains the outer bound — the loop aborts early if the deadline is at risk.

### Stage 5 · Read everything (`reader.rs`)
- Hits arrive per search round; each round's hits are batch-fetched with **one** TinyFish Fetch call per ≤10 URLs (`format=markdown`, `ttl=3600`).
- Per-URL failures are recorded and **dropped from the corpus** — they must not become citations (§8).
- Corpus = source article + successfully fetched hits. Dedupe by normalized URL.
- **Token budget:** trim each article head+tail to fit `CORPUS_TOKEN_BUDGET` (default 60 000 tokens, ~45 000 chars) — the source article always gets the largest share (e.g. 50%).

### Stage 6 · Synthesize (`synthesizer.rs`)

`async-openai` chat completion against `LLM_API_BASE` with `response_format: json_object`. Single call, system prompt contract:

```
You are a rigorous technology analyst. You will be given:
1. A source article (full text) and its metadata.
2. A corpus of related articles fetched from search results (with their URLs).

Produce a JSON object with exactly this schema:
{
  "summary":        "<1 paragraph, 80–140 words>",
  "deep_analysis":  "<3–4 paragraphs: context, mechanism/claims, implications, tensions>",
  "critique":       "<1–2 paragraphs: weaknesses, unsubstantiated claims, missing context>",
  "citations":      [{"url": "...", "context": "<one line: what claim it supports>"}]
}

CITATION RULES (hard constraints):
- You may ONLY cite URLs from the provided corpus. Never invent, guess, or reconstruct URLs.
- Every citation must actually support the claim it is attached to.
- The source article itself may be cited as [source].
- If nothing in the corpus supports a claim, make the claim without a citation.
- Use the exact URLs as given — do not alter protocol, host, or path.
```

- Temperature 0.2–0.4 (analytical, low drift); `max_tokens` ~2000.
- Parse with `serde_json`; on parse failure → one repair retry ("return valid JSON only"); second failure → `PipelineError::SynthesisFailed` → user-facing apology.

### Stage 7 · Citation validation (`citations.rs`) — the "legit" guarantee

Three-layer enforcement that every cited place is real:

1. **Pool construction:** the citation pool is exactly `{source article} ∪ {search hits that fetched successfully}`. Nothing else. A URL cannot be cited unless it survived TinyFish Fetch with extracted content — i.e., it was actually read.
2. **Prompt constraint:** the system prompt (§5 Stage 6) forbids out-of-pool URLs.
3. **Post-generation validation (the backstop):** after the LLM returns, extract every URL from `citations[]` and verify set-membership in the pool. Any URL **not in the pool is dropped** (logged as `citation_rejected`). If a section then has zero citations, re-run once with the pruned citation list and an instruction to re-cite from the reduced pool. This is unit-tested (§13) — including a prompt-injection test where the model is tricked into emitting a fake URL, and the validator must prune it.

### Stage 8 · Cache & cooldown (`cache.rs`)

SQLite tables:

```sql
CREATE TABLE analyses (
  url_hash TEXT PRIMARY KEY,          -- sha256 of normalized URL
  url TEXT, channel_id TEXT, created_at INTEGER,
  analysis_json TEXT,                 -- cached result for re-posts
  window_used TEXT, bucket TEXT
);
CREATE TABLE cooldowns (channel_id TEXT PRIMARY KEY, last_analysis_at INTEGER);
CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT);  -- /config persistence
```

- Same URL within TTL → re-post cached analysis (footer: "cached from <time>").
- Cache is per-URL, not per-guild — a link is a link.

### Stage 9 · Offline optimization on the synthetic scenario suite (`optimizer.rs`)

The search-loop policy (and by extension the freshness windows) is **not hand-tuned — it is tuned offline against ~100 synthetic scenarios**, with the search engine mocked (user directive: "this should be left to the optimizer"; "just mock the search engine").

**The scenario suite (`scenarios/`, ~100 JSON files + a generator script):**

```json
{
  "id": "scn_042",
  "source": { "url": "https://fastnews.example.com/…", "domain_bucket": "fast", "is_ai_topic": false, "ground_truth_angles": 7, "title": "…" },
  "corpus": [
    { "url": "…", "angle": "mechanism", "relevance": 0.9, "fetchable": true },
    { "url": "…", "angle": "industry-reaction", "relevance": 0.7, "fetchable": false }
  ],
  "expected": { "min_angles_covered": 5, "max_wasted_fetches": 3 }
}
```

Each scenario declares: the source article, the universe of related articles that *exist* in the mocked search engine (each with its angle and relevance — the mock's search ranking derives from these), which of them are fetchable (others return `bot_blocked`/`404`/`timeout`), and an expectation: how many of the ground-truth angles the final corpus must cover (`min_angles_covered`) and how many wasted fetches are tolerable (`max_wasted_fetches`). The suite spans: AI vs non-AI, every domain bucket, narrow vs sprawling topics, sparse vs dense corpora, dead-link clusters, paywalled/bot-blocked hits, duplicate-heavy results, and adversarial cases (e.g. a search engine that returns irrelevant junk on round 1 and gold on round 2 — tests whether the loop keeps going).

**Mock providers (`mock_providers.rs`):** `MockSearchProvider` (ranking = relevance score, honors window by filtering on `published_date`, supports fault injection: per-scenario dead links, rate-limit simulation, junk results) and `MockFetcher` (succeeds iff `fetchable`, else returns the scenario's error code). The LLM is **also mocked** at this level (coverage assessor and query extractor return scripted values per scenario) — the optimizer tunes the *loop mechanics*, not the LLM's judgment.

**Optimizer procedure (offline, `cargo run --bin optimize`):**

1. Sweep the policy grid: `INITIAL_K ∈ {3,5,7}`, `EXPANSION_K ∈ {2,3,5}`, `COVERAGE_TARGET ∈ {0.7,0.8,0.85,0.9}`, `MIN_NEW_ARTICLES ∈ {0,1,2}`, `MAX_ROUNDS ∈ {2,3,4}`, `SEARCH_BUDGET ∈ {10,15,20}` — each cell runs the full suite through the real loop code with mocks. Deterministic (injectable `clock.rs`; fixed RNG seed).
2. **Utility function (the over-search bias, made concrete):**

   `score = α·angles_covered − β·wasted_fetches − γ·budget_overshoot`, with `α = 1.0` per angle over the minimum, `β = 0.25` per wasted fetch, `γ = 0.5` per fetch past the budget. Hitting `min_angles_covered` is a hard constraint (a policy that misses it is rejected, whatever the score). **Because a missed angle costs 4× a wasted fetch, the optimizer itself prefers reading too much over too little** — the user's directive is enforced in the objective, not as a comment.
3. **Output** — the winning policy is written to `optimized_policy.json`; the bot loads it at startup (§9 env overrides still win). The optimization run, its score table, and the winning cell are recorded in the PR (and `cargo test` re-runs the suite with the shipped policy as a regression gate — a policy change that fails scenarios fails CI).
4. Re-run the sweep whenever the loop mechanics or the scenario suite change (documented in `README`); it is offline and mocked, so it never spends API credits.

---

## 6. Domain-speed mechanics (the "speed" rule, precise)

The "speed of that domain" is defined as **how fresh a related article must be to be worth reading**, encoded as a freshness window. It is a **static, configurable table** (`config.rs`, overridable via env `DOMAIN_SPEED_JSON`):

```json
{
  "fast":       {"window_minutes": 10080, "domains": ["reuters.com","theverge.com","techcrunch.com","arstechnica.com","bbc.com"]},
  "standard":   {"window_minutes": 43200, "domains": ["substack.com","medium.com","arxiv.org"]},
  "slow":       {"window_minutes": 129600, "domains": ["gov.uk","ec.europa.eu","fcc.gov"]},
  "evergreen":  {"window_minutes": 0,      "domains": ["wikipedia.org","github.com"]}
}
```

Resolution order: exact eTLD+1 match → subdomain wildcard (`*.substack.com`) → `default` (30 days). `breaking` bucket (3 days) is reserved for domains the operator adds explicitly.

Rationale: a news wire story ages in days; a standards-body ruling stays relevant for a year; a Wikipedia page is evergreen. The AI override (30 days) sits on top because LLM/agentic news moves at a fixed, known cadence per the spec.

---

## 7. End-to-end flow (numbered)

| # | Layer | Step |
|---|-------|------|
| 1 | frontend | Message arrives; regex finds `https://…`; gates pass (channel on, cooldown clear, not cached) |
| 2 | frontend | React `⏳` to the original message; create a thread anchored to it (the original message becomes the thread's start message → the thread replies to it) |
| 3 | core | `fetcher.fetch(url)` → markdown + `title`/`published_date`; on error → §10 message |
| 4 | core | `classifier.classify(title, text)` → `is_ai_topic` |
| 5 | core | `domain_speed.bucket(domain)` → window; if AI → force 30 days |
| 6 | core | `searcher` loop round 1: queries → Exa → fetch hits (mocked in tests) |
| 7 | core | Coverage assessment; expand rounds until target/stop conditions; reader assembles corpus (failures dropped, token budget trimmed) |
| 8 | core | If corpus is empty (0 related articles): respond with analysis of source alone + note "no related articles found in window" — do **not** fail |
| 9 | core | `synthesizer.generate(corpus)` → structured JSON |
| 10 | core | `citations.validate()` → prune out-of-pool URLs |
| 11 | frontend | `ui.render()` → thread: Summary / Deep Analysis / Critique / Sources (+ `⏱ window used` footer) |
| 12 | core | Cache write (URL → analysis, TTL 24 h); cooldown set |
| 13 | — | `⏳` reaction removed. Done. Total budget: **≤ 60 s** (SLA); typical ~15–30 s |

---

## 8. Citation integrity — what "legit" means, mechanically

A citation is legit iff **all** hold:

1. **Real** — the URL came from the search provider's live results (or is the source article). Never LLM-memorized.
2. **Read** — the URL was fetched by TinyFish Fetch and returned extracted content (2xx + non-empty text). Bot-blocked/paywalled/404 sources never enter the pool, so they can never be cited.
3. **Exact** — validator compares against the pool verbatim; no scheme/host/path mutation.
4. **Supporting** — the LLM is instructed (and the analysis reviewer is the human) that each citation must back the adjacent claim; the `context` field of each citation names the claim.

Failure modes covered: invented URLs (validator prunes — tested), hallucinated but plausible-looking sources (prompt constraint + pool), dead sources (fetch error codes), self-citation spam (search excludes source domain).

---

## 9. Configuration (env vars, `config.rs`)

| Var | Default | Meaning |
|---|---|---|
| `DISCORD_TOKEN` | — (required) | Bot token |
| `TINYFISH_API_KEY` | — (required) | TinyFish key (Search + Fetch) |
| `LLM_API_BASE` | `https://api.openai.com/v1` | Any OpenAI-compatible endpoint |
| `LLM_API_KEY` | — (required) | Key for that endpoint |
| `LLM_MODEL` | `gpt-4o-mini` | Model id |
| `ANALYZE_CHANNELS` | `*` | Allowlist (comma-separated ids) or `*` |
| `COOLDOWN_SECS` | `60` | Per-channel cooldown |
| `CACHE_TTL_HOURS` | `24` | Analysis cache TTL |
| `SEARCH_RESULTS` | `5` | *(legacy)* initial search hits per query — superseded by the loop policy |
| `INITIAL_K` / `EXPANSION_K` | `5` / `3` | Loop policy: hits per seed query / per angle expansion (Stage 4) |
| `COVERAGE_TARGET` | `0.85` | Loop stop: corpus coverage threshold (Stage 4) |
| `MIN_NEW_ARTICLES` | `1` | Loop stop: min distinct new articles for a round to count (Stage 4) |
| `MAX_ROUNDS` / `SEARCH_BUDGET` | `3` / `15` | Loop caps: max rounds / max distinct URLs fetched (Stage 4) |
| `OPTIMIZED_POLICY_JSON` | — | Path to `optimized_policy.json` from Stage 9; loads at startup (env overrides win) |
| `CORPUS_TOKEN_BUDGET` | `60000` | Total corpus trim budget |
| `REPLY_MODE` | `thread` | `thread` or `split` |
| `DOMAIN_SPEED_JSON` | built-in table | Override domain buckets |
| `RUST_LOG` | `info` | `tracing` verbosity |

---

## 10. Error handling & edge cases

*Delivery:* all error messages are posted inside the analysis thread (or as a plain reply if thread creation failed); on terminal failure the `⏳` reaction is replaced with `❌`.

| Case | Behavior |
|---|---|
| Fetch: `page_not_found` / `target_http_error` | "That link is dead (HTTP 404)" |
| Fetch: `bot_blocked` | "The site blocks automated readers — can't retrieve it" |
| Fetch: `empty_content` | "Page has no extractable content" |
| Fetch: `timeout` / `target_unreachable` | Retry once, then "Couldn't reach it right now" |
| Search: 0 results in window | Analyze source alone, say so, suggest widening |
| Corpus: 0 related fetched | Same as above |
| LLM: rate-limited / down | Retry once w/ backoff, then apology + offer raw corpus? No — keep it simple: apology |
| LLM JSON parse failure ×2 | `SynthesisFailed` apology |
| Citation validator prunes all citations | One regeneration pass with reduced pool |
| Message > 2000 chars | Split at paragraph boundary (thread mode mostly avoids) |
| 429 from TinyFish | Semaphore + backoff + `Retry-After` |
| Long pipeline > 15 min | Impossible by budget (§7); if hit, post partial + "timed out" |
| Bot in DMs | Gate: only guild channels (v1) |

---

## 11. Observability

- `tracing` + `tracing-subscriber`; JSON logs in Docker (`RUST_LOG`).
- Per-analysis span: url, bucket, window, corpus size, citation count, stage latencies, LLM model.
- `/status` surfaces the last N spans to the channel (operator feature).
- Counters (metrics later, out of scope now): analyses, fetch errors by code, citations rejected.

---

## 12. Milestones

| M | Scope | Exit criterion |
|---|---|---|
| **M1** | Workspace skeleton; serenity gateway; link regex; `⏳` reaction + thread anchor; `/ping` | Bot joins guild; posting a link yields a `⏳` reaction and a thread that replies to the message |
| **M2** | Fetcher, classifier, domain-speed, searcher loop (Exa), reader — no LLM synthesis yet; **scenario suite + optimizer offline** | `/analyze` prints fetched summary + related-article list with windows logged; `cargo run --bin optimize` completes a sweep; ~100 scenarios green with the winning policy |
| **M3** | Synthesizer + citation validator | Full 3-part analysis with verified citations in a thread |
| **M4** | Slash commands, /config, reply modes, cache+cooldown, error taxonomy | All §10 cases behave; dedupe re-posts cached analysis |
| **M5** | Tests (§13) incl. mutation pass, clippy, Docker, README | `cargo test` green; test:code ratio ≥ 1:1; mutation score ≥ 80% on `crates/core`; `docker compose up` works |
| **M6** | Live deploy + 48 h soak; `/status` shows healthy latency | Soak log: zero panics, all analyses < 60 s |

---

## 13. Testing strategy

- **Unit:** classifier scoring (AI vs non-AI fixtures), domain bucket resolution (incl. wildcard + default), citation validator (in-pool passes, out-of-pool pruned, fake-URL injection pruned), window computation (AI override fires), token trimming.
- **Integration (mocked):** full pipeline against a mock HTTP server standing in for TinyFish + LLM — asserts stage order, corpus assembly, failure drops, 0-result path, JSON repair retry.
- **Contract:** real TinyFish calls in an ignored-by-default `#[ignore]` test (requires key) to catch API drift.
- **Scenario suite (§5 Stage 9):** ~100 synthetic scenarios against `MockSearchProvider` + `MockFetcher` (search engine mocked per directive), run by the suite runner; a regression gate that fails CI if the shipped policy misses a scenario's expectations. The LLM is scripted in these tests — the loop mechanics are what's under test.
- **Discord:** no network tests; serenity client logic covered via unit tests on the gate/rendering functions (pure).
- **Mutation testing:** `cargo-mutants` (install: `cargo install cargo-mutants`). Run `cargo mutants` in `crates/core` — that is where all the logic lives; the bot crate is thin glue. Procedure: (1) baseline the current mutation score on a clean build, (2) add tests until the score is ≥ 80%, (3) keep it a pre-merge gate. Flags: `--timeout <secs>` so slow tests can't hang the run, `-j` for parallelism, `--in-place` to skip the checkout copy for speed if preferred. `unviable` mutants (build-breaking mutations) are excluded from the denominator. A full run takes 10–30 min on this codebase size, so it is a CI job / pre-merge gate, **not** part of `cargo test` itself. Target denominator: `caught / (caught + missed + timeout + no-cover)`.

---

## 14. Deployment

- **Dockerfile** — multi-stage: `rust:1.8x` builder (`cargo build --release`), runtime `debian:bookworm-slim` (musl static also viable; pick glibc for simplicity), non-root user, `COPY --from=builder` binary only.
- **docker-compose.yml** — env from `.env` (gitignored), `restart: unless-stopped`, `read_only: true` + tmpfs for SQLite or a small volume.
- Secrets: env vars only (v1). Fits the existing Docker stack pattern on this machine.

---

## 15. Definition of Done — "done" is when **all** of these pass

**Build & test**
- [ ] `cargo build --release` clean; `cargo clippy -- -D warnings` clean; `cargo test` green (unit + mocked integration)
- [ ] `cargo fmt --check` clean
- [ ] **Test:code ratio ≥ 1:1** — `scripts/test_ratio.sh` (implementation LOC = `src/` minus `#[cfg(test)]` modules; test LOC = `#[cfg(test)]` modules + `tests/`; both measured across both crates) exits 0
- [ ] **Mutation ratio ≥ ~80%** — `cargo mutants` on `crates/core` reports a score of ≥ 0.80 (`caught / (caught + missed + timeout + no-cover)`, `unviable` excluded); the baseline (score before the test-writing pass) is documented in the PR

**Core behavior (verified live in a test guild)**
- [ ] Post a link → `⏳` reaction on the message → thread created that **replies to the original message** → full 3-part analysis (summary / deep analysis / critique) in that thread, complete in **< 60 s**; `⏳` removed when done (or `❌` on terminal failure)
- [ ] An AI/LLM/agentic link → search log shows `window=30d` (recency rule fired)
- [ ] A non-AI link from a `fast` domain → `window=7d`; from an unknown domain → `window=30d`; from `evergreen` → no date filter (domain-speed rule fired)
- [ ] `⏱` footer in the output shows the window actually used
- [ ] **Search loop behaves per the tuned policy**: a typical run reads 6–12 related articles (over-search bias), stops on diminishing returns, and never exceeds `SEARCH_BUDGET` (log shows rounds + stop reason)
- [ ] **Optimization deliverable**: `scenarios/` contains ~100 synthetic scenarios; `cargo run --bin optimize` completes a full policy sweep against mocks; the winning cell + score table are recorded; `optimized_policy.json` is shipped and loaded at startup; the scenario suite passes with it (CI gate)

**Citations (the legit guarantee)**
- [ ] Every URL in the Sources message was fetched and read (validator log: 0 rejections on a normal run)
- [ ] Injection test: a fake/invented URL is **pruned**, never appears in output (unit test green)

**Robustness**
- [ ] Dead link, bot-blocked site, 0 search results, LLM outage → sane user-facing messages, no panics, bot stays connected
- [ ] Same URL twice within 24 h → second post is the cached analysis
- [ ] Long analysis splits correctly across the 2000-char limit in `split` mode
- [ ] 30 consecutive varied links with no crash, no rate-limit lockout (TinyFish 429 handled)

**Ops**
- [ ] `docker compose up` runs the bot; survives restart; logs are readable
- [ ] `/status` and `/config` work; channel gate and cooldown enforced
- [ ] README documents setup, all env vars, and the domain-speed table

**Definition of Done is met when every box above is checked with evidence** (test output + a soak log + a demo of one AI link and one non-AI link in a real guild).

---

## 16. Out of scope (conscious exclusions)

- Web dashboard / admin UI, analytics, multi-guild management plane
- Paywalled-content bypass, captcha solving, unblocking bot-blocked sites
- Non-OpenAI-compatible LLM backends (Azure works via `api_base`; anything else needs a new trait impl)
- Scheduled/monitoring modes (e.g. "watch this domain daily")
- i18n; multi-language analysis
- Streaming token-by-token responses (thread posts are chunk-fine)
- Metrics exporters (Prometheus) — counters only, v1

---

## 17. Open flags — resolved by the human (2026-08-01)

1. **Search provider default → Exa** (native date windows, `category=news`, `findSimilar`). TinyFish Search ships as the fallback impl behind the same trait. ✅ decided
2. **LLM endpoint → any OpenAI-compatible** (unchanged, confirmed). ✅ decided
3. **Reply mode → thread** (unchanged, confirmed). ✅ decided
4. **Search-loop intensity → left to the agent** (the coverage-feedback loop of §5 Stage 4): it is tuned, not hand-fixed — with a **slight bias toward over-search** ("better to read too much than too little"), enforced in the optimizer's utility function (a missed angle costs 4× a wasted fetch) and bounded by `MAX_ROUNDS`/`SEARCH_BUDGET` so it can't run away. ✅ decided
5. **Loop policy → left to the optimizer**: tuned offline by `cargo run --bin optimize` against **~100 synthetic scenarios with a mocked search engine**, shipped as `optimized_policy.json` (env overrides still win). ✅ decided

**New deliverable from this pass:** the synthetic scenario suite (§5 Stage 9) — ~100 mocked-search scenarios exercising the loop; the sweep that tunes the policy; the DoD gates that verify both.
