# LLM-as-Judge Reliability for Scoring Long-Form Analysis Quality

**Context:** bacain_chatbot generates long-form analyses (summary, deep analysis, critique, citations) with `deepseek-v4-flash` via Ollama Cloud. This note feeds a decision on (a) rubric dimensions/scales for an LLM-judge quality loop and (b) whether to judge with a stronger model than the generator. All claims are cited to primary sources (arXiv papers, lab docs); no blog summaries.

---

## Executive Summary

- LLM judges are usable but **biased in known, measurable ways**: self-preference, verbosity bias, position bias, and sycophancy are all documented in primary studies (MT-Bench, "LLMs are not Fair Evaluators", "LLM Evaluators Recognize and Favor Their Own Generations", "Towards Understanding Sycophancy"). None of these biases is fatal; each has a proven mitigation (order-swapping/aggregation, length control, evidence-before-score, rubric + reference answers).
- **Judge model choice matters, and a stronger judge than the generator is the safer default.** Frontier judges (GPT-4-class) reach >80% agreement with human preferences — the same level as human–human agreement (MT-Bench). Open judges can match GPT-4 only when given a **score rubric + reference answer** (Prometheus: Pearson 0.897 vs humans, on par with GPT-4's 0.882). Using the *same* model as judge and generator is the one configuration with a directly documented failure mode: self-preference bias inflates the generator's own outputs.
- **Rubric design is the highest-leverage intervention.** Multi-dimension scoring with explicit per-dimension rubrics and a reference answer is the configuration with the strongest human-alignment evidence. Factual/citation dimensions are the most reliably judgeable because they decompose into checkable atomic facts (FActScore, SAFE, ARES); holistic "depth/critique quality" dimensions are judgeable but with lower reliability and need anchoring examples.
- **Validate the judge before gating anything.** Exact-match agreement overstates judge quality; Cohen's kappa deflates 33–41 percentage points vs exact match on MT-Bench. Report kappa and correlation against a human-labeled sample (a few hundred items), and treat the judge as a gate only after it clears a pre-registered threshold on that sample.

---

## 1. Known failure modes and proven mitigations

### 1.1 Self-preference (self-enhancement)

- MT-Bench / Chatbot Arena (arXiv:2306.05685, "Judging LLM-as-a-Judge with MT-Bench and Chatbot Arena", OpenAI/LMSYS) documents **self-enhancement bias**: GPT-4 judges favor their own outputs, and the paper proposes mitigation by excluding the judge's own model from comparisons where possible. https://arxiv.org/abs/2306.05685
- "LLM Evaluators Recognize and Favor Their Own Generations" (arXiv:2404.13076, Google DeepMind) shows an LLM evaluator **scores its own outputs higher than others' while human annotators consider them equal quality**, and that self-recognition capability correlates linearly with self-preference strength (established via fine-tuning + controlled experiments). https://arxiv.org/abs/2404.13076
- "Self-Preference Bias in LLM-as-a-Judge" (arXiv:2410.21819) quantifies GPT-4's self-preference and traces it to **perplexity/familiarity**: LLMs assign higher scores to lower-perplexity (more familiar) text regardless of authorship. https://arxiv.org/abs/2410.21819
- **Implication for this project:** a judge that is the same model as the generator (deepseek-v4-flash judging deepseek-v4-flash output) is the exact configuration where self-preference is documented. A different/stronger judge removes this failure mode by construction.

### 1.2 Verbosity bias

- MT-Bench (2306.05685) reports GPT-4 judges **prefer longer answers**; the paper's mitigation is to instruct the judge to penalize verbosity. https://arxiv.org/abs/2306.05685
- "Length-Controlled AlpacaEval" (arXiv:2404.04475, Stanford) shows AlpacaEval's LLM annotator **favored longer outputs**; a regression-based length control (predicting preference conditioned on zero length difference) increased Spearman correlation with Chatbot Arena from **0.94 to 0.98** and made the metric robust to verbosity manipulation. https://arxiv.org/abs/2404.04475
- A 2026 large-cohort study (arXiv:2606.19544, "Reliability without Validity", 21 judges × 3 benchmarks × ~541k judgments) found verbosity bias **small (<0.011)** under a single pairwise rubric — i.e., verbosity bias is real but controllable via rubric/prompt design. https://arxiv.org/abs/2606.19544
- **Mitigation:** length-controlled scoring (regression adjustment) or explicit rubric language that decouples length from quality.

### 1.3 Position bias

- "Large Language Models are not Fair Evaluators" (arXiv:2305.17926, Tsinghua) shows **order-of-appearance manipulation can flip rankings** — e.g., Vicuna-13B can be made to beat ChatGPT on 66/80 queries with ChatGPT as evaluator. Proven mitigations proposed: (1) **Multiple Evidence Calibration** (generate evaluation evidence before assigning ratings), (2) **Balanced Position Calibration** (aggregate across swapped orders — the "swap and reverse" protocol), (3) Human-in-the-Loop Calibration (entropy-based difficulty detection). https://arxiv.org/abs/2305.17926
- "Judging the Judges: A Systematic Study of Position Bias in LLM-as-a-Judge" (arXiv:2406.07791) measures position bias across **15 LLM judges** in pairwise and listwise settings with three metrics (repetition stability, position consistency, preference fairness) and confirms it is widespread. https://arxiv.org/abs/2406.07791
- "Am I More Pointwise or Pairwise? Revealing Position Bias in Rubric-Based LLM-as-a-Judge" (arXiv:2602.02219) shows **rubric-based (pointwise) evaluation itself exhibits position bias** — it resembles a multiple-choice setting where judges prefer score options at specific positions. https://arxiv.org/abs/2602.02219
- **Mitigation:** always run pairwise comparisons in both orders and aggregate (or randomize order per item); for pointwise rubric scoring, randomize/rotate the order of score options and score anchors.

### 1.4 Sycophancy

- "Towards Understanding Sycophancy in Language Models" (arXiv:2310.13548, Anthropic) shows **five state-of-the-art assistants consistently exhibit sycophancy** across four free-form tasks, and that both humans and preference models prefer convincingly-written sycophantic responses over correct ones a non-negligible fraction of the time. https://arxiv.org/abs/2310.13548
- **Mitigation:** judges should be given the source material and asked to score against explicit criteria (not "is this good?"), reducing the space for agreeing-with-the-prompt behavior. No primary source shows a complete fix; awareness + rubric anchoring is the practical stance.

### 1.5 Which mitigations are proven (summary)

| Mitigation | Evidence | Status |
|---|---|---|
| Rubric + reference answer | Prometheus (2310.08491): 13B judge reaches Pearson 0.897 vs humans with rubric+reference, on par with GPT-4 (0.882) | Proven, strongest lever |
| Evidence/CoT before score | G-Eval (2303.16634) CoT+form-filling; 2305.17926 Multiple Evidence Calibration | Proven |
| Order swapping/aggregation | 2305.17926 Balanced Position Calibration | Proven |
| Length control | 2404.04475 regression-based length control (0.94→0.98 Spearman) | Proven |
| Judge ≠ generator | 2404.13076, 2410.21819 self-preference | Proven by construction |
| Pairwise vs pointwise | Prometheus 2 (2405.01535) supports both; 2602.02219 shows pointwise/rubric has its own position bias | Both usable; pairwise more robust to scale misuse, pointwise needed for per-dimension scores |

---

## 2. Stronger judge vs same-model judge: does it materially change human agreement?

**Bottom line: yes — judge capability is a first-order factor in human agreement, and same-model judging adds a documented bias. Use a judge stronger than (or at least different from) the generator.**

- **Frontier judges reach human-level agreement.** MT-Bench (2306.05685): GPT-4 as judge achieves **over 80% agreement with human preferences — the same level as agreement between humans** — while weaker judges (e.g., GPT-3.5) agree less. https://arxiv.org/abs/2306.05685
- **Judge capability is the binding constraint on hard pairs.** JudgeBench (arXiv:2410.12784, Princeton) shows that on challenging response pairs (knowledge, reasoning, math, coding), **many strong models (e.g., GPT-4o) perform just slightly better than random guessing** as judges; as generated responses get more sophisticated, stronger judges are required. https://arxiv.org/abs/2410.12784
- **Capability can be partially substituted by rubric + reference.** Prometheus (2310.08491): a 13B open judge with a user-supplied score rubric and reference answer scores **Pearson 0.897 with human evaluators, on par with GPT-4 (0.882) and far above ChatGPT (0.392)** — i.e., judge size matters, but rubric+reference can close most of the gap. https://arxiv.org/abs/2310.08491
- **For long-form specifically, the reliability gap is larger and rubrics/references help but are not always sufficient.** LongJudgeBench (arXiv:2606.01629, 2026) — a meta-evaluation benchmark for long-form outputs — finds a **substantial reliability gap: current LLM judges remain unstable across scenarios, and rubrics or references are helpful but not always sufficient** for long-form judging. https://arxiv.org/abs/2606.01629
- **Judge rankings are not stable across benchmarks.** 2606.19544: judge rankings shift by **up to 14 positions** across MT-Bench/JudgeBench/RewardBench, and high test–retest reliability (>0.95) can coexist with severe position bias (>0.10) in production judges ("consistency–bias paradox"). https://arxiv.org/abs/2606.19544
- **Same-model judging is the one configuration with a directly documented failure mode** (self-preference, §1.1: 2404.13076, 2410.21819). No primary study shows same-model judging matching cross-model judging on human agreement.
- **Direct evidence for "deepseek-v4-flash as judge" does not exist in the literature** (it is a 2026 model; DeepSeek's own API docs list `deepseek-v4-flash` and `deepseek-v4-pro` as the available models — https://api-docs.deepseek.com/). The decision must therefore be made from the general evidence above: frontier-class judges agree with humans at human–human levels; open judges need rubric+reference to approach that; same-model judging adds self-preference. **Recommendation: judge with a different, stronger model than the generator** (e.g., a frontier-class model or at minimum a different model family), and validate on a human-labeled sample (§4).

---

## 3. Practical rubric design for multi-dimension scoring of long-form analysis

### 3.1 Which dimensions are reliably judgeable

- **Factual grounding — most reliably judgeable, because it decomposes.** FActScore (arXiv:2305.14251, AI2) breaks long-form generations into **atomic facts** and scores the percentage supported by a reliable knowledge source; an automated estimator reaches <2% error vs human FActScore. This decomposition is what makes factuality judgeable at all — holistic "is it factual?" scores are unreliable; per-fact verification is not. https://arxiv.org/abs/2305.14251
- **Long-form factuality via search-verified facts.** SAFE (arXiv:2403.18802, Google DeepMind, "Long-form factuality in large language models") decomposes responses into facts and verifies each against Google Search; it **agrees with crowdsourced human annotators 72% of the time and wins 76% of disagreements**, at 20× lower cost. https://arxiv.org/abs/2403.18802
- **Citation relevance / verifiability — judgeable as citation recall + precision.** "Evaluating Verifiability in Generative Search Engines" (arXiv:2304.09848, Google) defines verifiability as **citation recall** (all statements supported by citations) and **citation precision** (every citation supports its statement) and evaluates it with human annotation — the same two quantities can be scored by a judge per-citation. https://arxiv.org/abs/2304.09848
- **RAG-style dimensions are established.** ARES (arXiv:2311.09476, Stanford) evaluates RAG along **context relevance, answer faithfulness, and answer relevance** with fine-tuned lightweight judges + prediction-powered inference. https://arxiv.org/abs/2311.09476
- **Summary accuracy — judgeable with a reference.** SummEval (arXiv:2007.12626, NYU) is the standard human-annotated summarization benchmark with four dimensions (coherence, consistency, fluency, relevance); G-Eval (2303.16634) reached Spearman **0.514 with humans on summarization** using GPT-4 with CoT + form-filling — the best of its time, still moderate, i.e., summary quality is judgeable but not trivially. https://arxiv.org/abs/2007.12626 | https://arxiv.org/abs/2303.16634
- **Analysis depth / critique quality — judgeable but least reliable.** These are the "creativity/diversity" dimensions where reference-based metrics fail and LLM judges are the only option (G-Eval motivation, 2303.16634). LongJudgeBench (2606.01629) explicitly flags **document-level assessments of organization, coverage/depth, and cross-section consistency** as the hard part of long-form judging. Anchor these dimensions with exemplar scores in the rubric; expect lower judge–human agreement than for factuality/citation dimensions.

### 3.2 Scales

- **Both 1–5 and 1–10 are used in the primary literature with no head-to-head winner published in the sources reviewed here.** G-Eval and Prometheus use **1–5** rubrics (2303.16634, 2310.08491); MT-Bench uses **1–10** (2306.05685). Prometheus 2 (2405.01535) evaluates on both direct-assessment (pointwise) and pairwise formats with user-defined criteria. https://arxiv.org/abs/2405.01535
- **Practical guidance from the evidence:** the scale granularity matters less than (a) explicit per-score anchor descriptions in the rubric (Prometheus's rubrics are 1–5 with per-level descriptions — 2310.08491), and (b) awareness that **rubric/pointwise scoring has its own position bias over score options** (2602.02219) — so keep the option order fixed/randomized and the anchors explicit. A 1–5 scale with written anchors per level is the configuration with the strongest published human-alignment numbers (Prometheus 0.897 Pearson).
- **Gap flagged:** no primary source in this set directly compares 1–5 vs 1–10 vs Likert reliability for LLM judges; treat scale choice as secondary to rubric anchoring.

### 3.3 Free-text justification

- **Justification before scoring improves reliability.** 2305.17926's Multiple Evidence Calibration (generate evidence, then rate) is a proven bias mitigation. G-Eval's CoT-then-score (2303.16634) is the same pattern. CritiqueLLM (arXiv:2311.18702) shows that training judges to produce **informative critiques** (not just scores) improves evaluation quality in both pointwise and pairwise settings. https://arxiv.org/abs/2311.18702
- **Recommendation:** require the judge to output per-dimension evidence/justification *before* the numeric score, and parse the score from a structured field (JSON) so the justification cannot leak into the score.

### 3.4 Recommended rubric skeleton (synthesis, not a published rubric)

| Dimension | Judgeability | Scale | Notes |
|---|---|---|---|
| Summary accuracy | High (with reference) | 1–5 anchored | Compare against source material; per-claim check |
| Factual grounding | Highest (decomposable) | 1–5 anchored or % supported | Atomic-fact decomposition (FActScore/SAFE pattern) |
| Citation relevance | High | 1–5 anchored | Score citation recall + precision per citation |
| Analysis depth | Medium | 1–5 anchored | Anchor with exemplars; expect lower agreement |
| Critique quality | Medium | 1–5 anchored | Anchor with exemplars; expect lower agreement |

---

## 4. Calibration: measuring judge–human agreement and gating decisions

### 4.1 Metrics

- **Exact-match agreement overstates judge quality; use chance-corrected agreement.** 2606.19544 (21 judges, ~541k judgments): **kappa deflation between exact match and Cohen's kappa is universal — 33–41 percentage points on MT-Bench** — and the paper's Minimum Viable Validation Protocol requires reporting kappa, not just agreement. https://arxiv.org/abs/2606.19544
- **Correlation for continuous scores.** The field standard for pointwise scores is Pearson/Spearman vs human scores: G-Eval Spearman 0.514 (summarization, 2303.16634); Prometheus Pearson 0.897 (with rubric+reference, 2310.08491); length-controlled AlpacaEval Spearman 0.98 vs Chatbot Arena (2404.04475).
- **Human–human agreement is the ceiling reference.** MT-Bench: human–human agreement ≈ 80%; GPT-4 judge ≈ 80% — a judge at human–human agreement level is the practical ceiling (2306.05685).

### 4.2 When is a judge trustworthy enough to gate prompt-variant comparisons?

- **Pre-register a human-labeled validation sample and a threshold.** The evidence-based pattern: collect a few hundred human-labeled items on the exact task distribution, compute kappa (for pairwise/classification) or correlation (for pointwise scores) between judge and humans, and only then use the judge to gate comparisons. ARES (2311.09476) formalizes this with **prediction-powered inference**: a few hundred human annotations are used to correct judge predictions with valid confidence intervals. https://arxiv.org/abs/2311.09476
- **Calibration against human labels is a proven post-hoc step.** AutoCalibrate (arXiv:2309.13308) calibrates an off-the-shelf LLM evaluator toward human labels (draft criteria → select best → self-refine) and reports significant correlation gains with expert evaluation. CalibraEval (arXiv:2410.15393) calibrates the judge's prediction distribution to mitigate selection bias. https://arxiv.org/abs/2309.13308 | https://arxiv.org/abs/2410.15393
- **Do not trust consistency alone.** 2606.19544: high test–retest reliability (>0.95) coexists with severe position bias (>0.10) in two production judges — a judge can be self-consistent and wrong. Gate on *agreement with humans*, not on self-consistency. https://arxiv.org/abs/2606.19544
- **For gating prompt-variant comparisons specifically:** use **paired comparisons with order randomization** (both orders, aggregated) rather than absolute scores where possible — pairwise judging is the format with the strongest human-alignment evidence (MT-Bench >80% agreement, 2306.05685) and the swap-and-reverse protocol removes position bias (2305.17926). If absolute per-dimension scores are required (for the quality loop), validate each dimension's correlation against the human sample separately, and treat dimensions with correlation below ~0.5 (the G-Eval-level bar for holistic quality) as non-gating / informational only.
- **Practical threshold suggestion (synthesis):** gate prompt-variant comparisons when judge–human Cohen's kappa ≥ 0.6 (pairwise) or Spearman ≥ 0.7 (pointwise) on a pre-registered sample of ≥ 200 items from the production distribution, with the judge model ≠ generator model. These thresholds are engineering judgment informed by the cited numbers (human–human ≈ 0.8 agreement; G-Eval 0.514 Spearman is "best available" for holistic quality, not a gate bar), not a published standard — flag as such in the decision session.

---

## Sources

1. Zheng et al., "Judging LLM-as-a-Judge with MT-Bench and Chatbot Arena" (OpenAI/LMSYS) — https://arxiv.org/abs/2306.05685
2. Wang et al., "Large Language Models are not Fair Evaluators" (Tsinghua) — https://arxiv.org/abs/2305.17926
3. Liu et al., "G-Eval: NLG Evaluation using GPT-4 with Better Human Alignment" (Microsoft) — https://arxiv.org/abs/2303.16634
4. Panickssery et al., "LLM Evaluators Recognize and Favor Their Own Generations" (Google DeepMind) — https://arxiv.org/abs/2404.13076
5. Xu et al., "Self-Preference Bias in LLM-as-a-Judge" — https://arxiv.org/abs/2410.21819
6. Sharma et al., "Towards Understanding Sycophancy in Language Models" (Anthropic) — https://arxiv.org/abs/2310.13548
7. Dubois et al., "Length-Controlled AlpacaEval" (Stanford) — https://arxiv.org/abs/2404.04475
8. Chen et al., "Judging the Judges: A Systematic Study of Position Bias in LLM-as-a-Judge" — https://arxiv.org/abs/2406.07791
9. "Am I More Pointwise or Pairwise? Revealing Position Bias in Rubric-Based LLM-as-a-Judge" — https://arxiv.org/abs/2602.02219
10. Kim et al., "Prometheus: Inducing Fine-grained Evaluation Capability in Language Models" (KAIST AI) — https://arxiv.org/abs/2310.08491
11. Kim et al., "Prometheus 2: An Open Source Language Model Specialized in Evaluating Other Language Models" (KAIST AI) — https://arxiv.org/abs/2405.01535
12. Gu et al., "JudgeBench: A Benchmark for Evaluating LLM-based Judges" (Princeton) — https://arxiv.org/abs/2410.12784
13. "Benchmarking LLM-as-a-Judge for Long-Form Output Evaluation" (LongJudgeBench) — https://arxiv.org/abs/2606.01629
14. "Reliability without Validity: A Systematic, Large-Scale Evaluation of LLM-as-a-Judge Models Across Agreement, Consistency, and Bias" — https://arxiv.org/abs/2606.19544
15. "Training an LLM-as-a-Judge Model: Pipeline, Insights, and Practical Lessons" (Themis) — https://arxiv.org/abs/2502.02988
16. Min et al., "FActScore: Fine-grained Atomic Evaluation of Factual Precision in Long Form Text Generation" (AI2) — https://arxiv.org/abs/2305.14251
17. Wei et al., "Long-form factuality in large language models" (SAFE, Google DeepMind) — https://arxiv.org/abs/2403.18802
18. Saad-Falcon et al., "ARES: An Automated Evaluation Framework for Retrieval-Augmented Generation Systems" (Stanford) — https://arxiv.org/abs/2311.09476
19. Fabbri et al., "SummEval: Re-evaluating Summarization Evaluation" (NYU) — https://arxiv.org/abs/2007.12626
20. Liu et al., "Evaluating Verifiability in Generative Search Engines" (Google) — https://arxiv.org/abs/2304.09848
21. "Calibrating LLM-Based Evaluator" (AutoCalibrate) — https://arxiv.org/abs/2309.13308
22. "CalibraEval: Calibrating Prediction Distribution to Mitigate Selection Bias in LLMs-as-Judges" — https://arxiv.org/abs/2410.15393
23. Ke et al., "CritiqueLLM: Towards an Informative Critique Generation Model for Evaluation of Large Language Model Generation" — https://arxiv.org/abs/2311.18702
24. "A Survey on LLM-as-a-Judge" — https://arxiv.org/abs/2411.15594
25. "From Generation to Judgment: Opportunities and Challenges of LLM-as-a-judge" — https://arxiv.org/abs/2411.16594
26. Li et al., "Arena-Hard and BenchBuilder Pipeline" (LMSYS) — https://arxiv.org/abs/2406.11939
27. OpenAI, "GPT-4 Technical Report" — https://arxiv.org/abs/2303.08774
28. Ouyang et al., "Training language models to follow instructions with human feedback" (InstructGPT, OpenAI) — https://arxiv.org/abs/2203.02155
29. Anthropic, "Building Evals" (official cookbook: model-based grading, grader prompts) — https://github.com/anthropics/anthropic-cookbook/blob/main/misc/building_evals.ipynb
30. DeepSeek API documentation (model list: deepseek-v4-flash, deepseek-v4-pro) — https://api-docs.deepseek.com/
