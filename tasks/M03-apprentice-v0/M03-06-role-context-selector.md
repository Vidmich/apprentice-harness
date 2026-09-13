# M03-06 — Role: context selector

Status: todo
Depends on: M03-02, M03-05, M03-08
Size: M

## Goal

A prompted context selector: at the start of a task (and after a
compaction or when the mentor's target moves), formulate retrieval
queries from the task and the current `#state`, gather candidate chunks
from the repository index (symbol matches, lexical search, recent edits,
files the mentor named), rank them with the apprentice, cut to a token
budget, and inject a `#ctx` block before the mentor call so the mentor
reads the right code without a round of `grep`/`read_file` calls. Every
candidate and its score stay in the trace, so M04 can ablate ("were the
selected chunks used, were omitted ones needed").

## Context

SPEC §5.2 (input: task, state summary, index, candidates; output: a
ranked, budgeted list; success: the mentor succeeds with the selected
set, ablation shows selected chunks used and omitted not needed;
implementation candidates: a small encoder ranker with the generative
model only for query formulation). M03-05's `RepoIndex {symbols, search,
callers, chunks_for, recent_edits}`; M03-01's `#ctx` grammar with
`chunk <id> <path> L<a>-<b>`; M03-08's placement of a before-call
block; M02-06's `Encoder::score` for the optional encoder ranker.

## Scope

In: triggers, query formulation, candidate gathering, prompted listwise
ranking (and the encoder ranker behind the same interface), budgeting,
the `#ctx` block, trace records for ablation, config, tests with the
fixture repos.
Out: training the ranker (M06), giving the mentor index tools (M10),
selecting non-code context (docs are indexed as plain-text chunks and
compete on the same footing).

## Design

### Triggers

`wants()` in `before_call` when: step 1 of a run; the step after a
compaction (M03-07); the previous mentor turn named ≥ 2 paths the mentor
has not read (from `tool_use` inputs and `path:line` mentions in text);
or `selector.every_steps` (0 = off) elapsed. Never twice within
`selector.min_gap_steps` (3). Bounded to `selector.max_per_run` (5).

### Stage 1 — queries (`Interactive`, small budget)

Query formulation (`roles/selector.md` "queries" section; observer
query `select <task>`): from the task line, the last user message, the
`#state` (if any) and the last mentor turn, produce ≤ 6 lines: `sym
<name>`, `text <terms>`, `path <glob>`, `callers <name>`. Parsed with
`wire::parse` (a `#queries` block, internal — never sent to the
mentor). Fallback on bypass: mechanical queries (identifiers in the user
message that match `symbols(prefix)`, paths mentioned, recent edits).

### Stage 2 — candidates (no model)

Run the queries against the index: `symbols` → their chunks;
`search(terms)` → top 20 by BM25; `callers` → top 10; `chunks_for(paths
mentioned)`; `recent_edits` → the last 5 touched files' edited chunks;
chunks the mentor already read this run (from `SeenFiles`) are
**excluded** (they are in the history). De-duplicated, capped at
`selector.candidates` (40), each with its gathering reasons and scores
(`CandidateRecord {chunk_id, path, range, sources: [sym|text|callers|
path|recent], bm25, tokens_est}`).

### Stage 3 — ranking

`Ranker` interface: `rank(task: &str, state: Option<&str>, candidates)
-> Vec<(ChunkId, f32)>`.

- `prompted` (v1 default): listwise — the candidates as numbered
  entries (path, range, symbol signature, the first 6 lines) and the
  task; the model returns an ordered id list with one-line reasons
  (`#rank` block, internal). Candidates above `selector.rank_input_
  tokens` (6k) are ranked in two halves and merged by score.
- `encoder:<model_id>` (M02-06): `score(pairs = (task ⊕ state, chunk
  text))` on the encoder pool — milliseconds, no prompt; the prompted
  ranker's reasons are absent. Config `selector.ranker`; M06 trains a
  model for this slot.
- Fallback on bypass: order by (`sources` count, bm25).

### Budget and the block

Take ranked chunks until `selector.budget_tokens` (4000, M03-09's
estimate; bytes/4 before it) is spent, at most `selector.max_chunks`
(12); merge adjacent chunks of one file. Emit `#ctx budget=… used=…`
with a `chunk <id> <path> L<a>-<b> [why: <reason>]` line and the verbatim
fenced text per chunk (M03-01 grammar), injected by M03-08 (system
message or user block). The text is read from the file at injection
time and hash-checked against the chunk row; a stale chunk is skipped
and re-indexing queued.

### Records

One `apprentice.invocation` per stage that called the model (`role:
"selector", stage: queries|rank`) plus one summary record `role:
"selector", stage: "inject"` with `{queries, candidates: [Candidate
Record…], ranked: [(id, score, reason)], selected: [id…], budget,
used_tokens, ranker}` — M04's ablation reads `candidates` and `selected`
and the following steps' `read_file`/`expand` calls to score "used /
needed". `tokens_before = 0`, `tokens_after = used_tokens` (the selector
*adds* tokens; the saving it claims is the avoided tool round trips —
M03-09 reports it as `added`, M04 measures the net).

### Config

```toml
[apprentice.selector]
enabled = false            # on after the compressor and compactor are stable (README order)
ranker = "prompted"        # prompted | encoder:<model_id>
budget_tokens = 4000
candidates = 40
max_chunks = 12
rank_input_tokens = 6144
min_gap_steps = 3
every_steps = 0
max_per_run = 5
```

## Acceptance

- [ ] Fixture repo (M03-05's Rust crate) and a mock backend scripted
      with a `#queries` and a `#rank` block: the candidate set contains
      the chunks the queries should reach (golden of `CandidateRecord`s),
      chunks already read by the run are excluded, the injected `#ctx`
      holds the ranked chunks up to the budget with merged neighbours,
      and the summary record lists candidates/ranked/selected.
- [ ] Triggers: fires at step 1 and after a compaction, not within
      `min_gap_steps`, not more than `max_per_run`; a mentor turn naming
      two unread paths fires it.
- [ ] Bypass at stage 1 → mechanical queries; at stage 3 → source/bm25
      order; both produce a block and records with the reasons.
- [ ] `ranker = "encoder:<fixture MiniLM rank model>"` (M02-06) ranks
      the same candidates through the encoder pool in < 100 ms on the
      reference machine and the block has no reasons.
- [ ] A chunk changed on disk between indexing and injection is skipped
      and the file re-indexed; the next selection sees the new chunk.
- [ ] Live: on the harness repo, "add a `--json` flag to `harness index
      status`" as a first message — the `#ctx` holds the `index` CLI
      module and the RPC types, and the mentor's first turn edits
      without a `grep` round (transcript id in the notes; M04 quantifies
      later).

## Verification

`cargo test -p apprentice-core runtime::apprentice::selector::` (fixture
repo, mock backend); the encoder variant with the M02-06 fixture; the
live session.

## Notes

- The selector is the riskiest role (it can waste tokens rather than
  save them) — it ships disabled by default in M03 and is enabled once
  the M04 ablation says it earns its budget. Do not enable it in
  `config` defaults in this task.
- Keep stage 2 model-free and cheap so the encoder ranker path (M06's
  target) has no generative call at all except query formulation.
