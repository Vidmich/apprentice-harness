# M03-03 — Role: output compressor

Status: todo
Depends on: M03-02, M03-01, M01-01
Size: M

## Goal

The first role that saves tokens: every large tool result (shell, grep,
read_file, git_diff, run_tests, …) is rewritten by the apprentice into
a `#result` block that is **references first** — `err:` lines copied
verbatim, `ref blob:<id> L<a>-<b>` locators for everything else — and
the harness, not the model, expands the locators it decides the mentor
needs verbatim (errors with context, exact strings for edits, small
results whole). Verbatim copies from the model are accepted only when
they match the raw bytes; the raw blob stays retrievable by id
(`expand`, M03-04). When enabled it replaces M01-01's head/tail
truncation as the policy for what the mentor sees.

## Context

SPEC §5.1 (input, output, "References before copies", success = the
mentor's next action is unchanged, fallback = the raw result with a
mechanical truncation, "lossless on demand"). M01-01's `tools::limits`
("the apprentice's output compressor (M03) replaces this policy, and the
raw blob is what it trains on"): `Executed { block, blob_id,
output_bytes, mentor_bytes, truncated, metadata }`, `tools.max_mentor_
bytes` (32 KiB), the omission marker naming the blob. M03-01's wire
grammar and `roles/compressor.md`; M03-02's `Role` trait and
`on_tool_result` planning.

## Scope

In: the role, per-tool-type prompt sections and input shaping, the
reference validator, the harness expansion policy, `#need raw`, the
skip rules, the fallback, the `tool.result` payload additions, config,
a golden corpus of raw results with expected blocks (from M01 dogfood
traces), tests on the mock backend.
Out: the `expand` tool (M03-04), measuring whether the mentor's action is
unchanged (M04's replay evaluator), training (M06).

## Design

### When it runs

`wants()`: the result has a blob, `output_bytes ≥ compressor.min_bytes`
(2048), the tool is not `edit_file`/`write_file` (their results are
small and exact), `kind ∉ {Denied, InvalidInput, Cancelled, Failed}`
(diagnostics pass through), the media type is text. Otherwise the
baseline block goes through untouched and no invocation is recorded
(`skipped` is counted in the step's `agent.apprentice` summary only).

### Input shaping (per tool type)

The role prompt `roles/compressor.md` has a common part and one `## tool:
<name>` section chosen by `Executed.name` (`shell` further by
`metadata.program` — `cargo`, `pytest`, `npm`, `git`, generic — and
`run_tests` by its parsed outcome). The input is the raw text with line
numbers prefixed (`L<n>│…`) so locators are exact, cut to `compressor.
max_input_bytes` (48 KiB; the head and tail with the omitted middle
marked, the cut range recorded) plus a header line: tool, arguments
(compact), exit code, byte and line counts, and the current task line
(`#state.goal` when one exists, else the first user message's first
line, ≤ 200 chars). Stateless backend: prompt = section + input;
observer: `compress <tool_call_id>` query with the same input
uncommitted.

### Output and validation

The model emits one `#result` block (M03-01 grammar); `wire::parse`
then `validate(block, raw)`:

- every `ref blob:<id> L<a>-<b>` must name this result's blob and a
  range within its line count — others are dropped;
- every `err:` line and every fenced `raw L<a>-<b>` copy must match the
  raw text at that range byte-for-byte after whitespace normalisation —
  a mismatch drops the line (`refs_invalid += 1`) rather than sending
  the model's version;
- the block must fit `compressor.max_output_bytes` (8 KiB before
  expansion);
- more than `compressor.max_invalid_ratio` (0.3) of lines dropped, or
  no block at all → `InvalidOutput` → fallback.

### Harness expansion (`ExpandPolicy`)

After validation the harness resolves locators into verbatim text from
the blob, under `compressor.expand_budget_bytes` (12 KiB per result):

1. every `err:` line gets `±expand.err_context` (3) raw lines appended
   as a `raw` fence (compile errors, stack frames, failed assertions);
2. `#need raw blob:<id> L<a>-<b>` lines the model emitted are honoured
   in order until the budget is spent (the model asks, the harness
   copies);
3. `ref` lines flagged `edit` (the model marks ranges it expects the
   mentor to edit) are expanded while budget remains;
4. results whose validated block plus expansions exceed the baseline's
   `mentor_bytes` are replaced by the baseline (compression that grew
   is not compression — recorded as `reason: NoGain`).

The final block text goes into `Executed.block` (the `tool_result`
content) with a trailing line `expand blob:<id> for the full result`
so the mentor knows the key; `Executed.mentor_bytes` is updated.

### Fallback and records

Bypass, error, invalid output, `NoGain` → the M01-01 truncated block as
built by the tool (kept in `Executed` as `baseline_block`). The
`tool.result` payload gains `{compressed: bool, compressor: {invocation_
id, mentor_bytes_before, mentor_bytes_after, refs, expanded_bytes,
verbatim_bytes, dropped_lines}}` so M04 can rebuild either variant of
the request from the trace without re-running anything. The invocation
record (M03-02) carries `tokens_before` = tokens of the baseline block
and `tokens_after` = tokens of the final block (M03-09 supplies the
counts; before it exists both are byte counts / 4 labelled `estimate:
bytes`).

### Config

```toml
[apprentice.compressor]
enabled = true
min_bytes = 2048
max_input_bytes = 49152
max_output_bytes = 8192
expand_budget_bytes = 12288
err_context = 3
max_invalid_ratio = 0.3
tools = ["shell", "grep", "read_file", "git_diff", "git_log", "run_tests", "glob", "list_dir"]
```

Per-run: `--no-apprentice` (M03-02) turns it off with the rest.

### Golden corpus

`crates/core/tests/fixtures/compressor/<n>-<tool>.{raw,args.json,
expected.md}`: 20 raw results sampled from M01 dogfood traces (`harness
trace export --kind tool.result --sample 20`, redacted) — cargo build
errors, a pytest failure log, a grep with 300 hits, a 900-line
read_file, a 2 000-line diff, a shell listing, a run_tests pass. The
expected blocks are hand-written v1 targets; the test runs the
validator and the expansion policy on them (no model) and asserts the
output invariants (every `err:` in the raw appears, every locator
resolves, size ≤ budget). The same corpus is M04's first replay set.

## Acceptance

- [ ] `wants()` skips small results, edit/write results and denied
      calls; a 3 KB shell result runs.
- [ ] Validator: a block with a wrong line range, a paraphrased `err:`
      line and a good `ref` keeps only the good `ref` and counts two
      drops; over `max_invalid_ratio` → `InvalidOutput` → baseline
      block, record `bypassed: true, reason: InvalidOutput`.
- [ ] Expansion: an `err:E0308 @ src/x.rs:42` line gets L39–45 raw
      appended; three `#need raw` lines are honoured until the budget
      is spent and the fourth is left as a `ref`; a block that ends up
      larger than the baseline is replaced (`NoGain`).
- [ ] The golden corpus passes; on the mock backend scripted to return
      the expected blocks end-to-end (M03-02's loop), the requests'
      `tool_result` blocks equal the expected text plus the `expand`
      trailer and `tool.result.compressed = true`.
- [ ] Bypass from the service (`Deadline`) → the baseline block, the
      step is not delayed beyond `start_by`, the record says so.
- [ ] Live, reference machine, 3B model, 20 dogfood steps: median
      `mentor_bytes_after / mentor_bytes_before` ≤ 0.35 on results over
      8 KB, `refs_invalid` rate < 10 % of lines, compressor p95 latency
      within the M02-05 budget, no bypass caused by the role itself;
      numbers in the completion notes with the session id.

## Verification

`cargo test -p apprentice-core runtime::apprentice::compressor::`
(validator, expansion, corpus, mock end-to-end); the live session; M04
later measures replay-equivalence on the same corpus.

## Notes

- The model only has to *locate*; the harness copies. Resist the urge to
  let the model paraphrase error text — a wrong string in an `err:` line
  sends the mentor to the wrong place, which is worse than the raw
  result. Dropping on mismatch is the rule.
- The prompt sections will change often; keep every wording change in
  `protocol/CHANGELOG.md` (M03-11) and re-run the corpus.
