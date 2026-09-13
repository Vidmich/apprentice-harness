# M03-01 — Protocol v1 artifact

Status: todo
Depends on: M01-09, M00-06
Size: M

## Goal

The mentor–apprentice protocol as a versioned, embedded artifact under
`crates/core/protocol/v1/`: the mentor system-prompt **addendum** (a
separate block after the frozen M01-09 core, so baseline and apprentice
sessions share the core's cache entry and can be A/B'd), the apprentice
role prompts, the wire-format specification for the tagged blocks
(`#ctx`, `#result`, `#state`, `#need`, `expand <id>`, `#ask-apprentice`)
with a Rust parser/emitter, a CHANGELOG, and `protocol_version` recorded
in every `mentor.request` payload and every `apprentice.invocation`.
Nothing here calls a model; the roles (M03-03/06/07) and the orchestrator
(M03-02) consume it.

## Context

SPEC §6 ("The protocol is a first-class, versioned artifact
(`protocol/vN/`), consisting of the mentor system prompt sections, the
apprentice role prompts, and the wire formats"; wire principles: compact
tagged blocks, exact strings and line numbers never paraphrased, stable
prefix layout, append-only prompts, `#ask-apprentice`, the end-of-step
verdict). M01-09 froze `mentor_system_v1.md` (`runtime::prompt`,
`PROMPT_VERSION`, `prompts/CHANGELOG.md`) and made `prompt_version`
travel with sessions and `mentor.request` payloads; `mentor_calls.
apprentice_applied` exists since M00-06. The M03 README asks that the
addendum be separate from the baseline "so A/B stays possible".

## Scope

In: the directory layout and embedding, the addendum text, the four role
prompts (compressor, compactor, selector, observer) in their v1 wording,
the wire spec and its parser, version plumbing into the trace, the
CHANGELOG with the first entry, `harness protocol show|list`.
Out: the `expand` tool implementation (M03-04, declared here), the
`#verdict` structured output (M05 — the spec reserves the tag), the
diff/bump workflow (M03-11), any evaluation of the wording (M04).

## Design

### Layout (`crates/core/protocol/`)

```
protocol/
  CHANGELOG.md
  v1/
    mentor_addendum.md          system block after the core: what the apprentice is, the tags, expand, #ask-apprentice
    wire.md                     the block grammar, references, examples (human-readable spec; the parser's tests quote it)
    roles/
      compressor.md             common part + one `## tool: <name>` section per tool type (M03-03 selects the section)
      compactor.md              produce a `#state` document from a transcript window
      selector.md               query formulation + listwise ranking prompts
      observer.md               ingestion framing (how events are presented to the state) + the query prompts (`#state`, `compress`, `select`)
```

`protocol/mod.rs`: `pub const PROTOCOL_VERSION: &str = "v1"`, every
file via `include_str!`, `pub fn addendum() -> SystemBlock`, `pub fn
role(name: Role) -> &'static str`, `pub fn manifest() -> ProtocolManifest
{version, files: [(path, sha256)], hash}` (the hash of all v1 bytes, in
`daemon.status` and the CHANGELOG test). A future `v2/` is a sibling
directory selected by `protocol.version` (M03-11); v1 stays embedded.

### Prompt layout

`system = [core (M01-09), addendum, #workspace]` — the addendum goes
**before** the per-session workspace block so `core + addendum` is one
shared cached prefix across apprentice sessions, while `core` alone stays
shared with baseline sessions. Each block keeps its own cache breakpoint
(`Conversation::request`). `runtime::prompt::build_system` takes an
`apprentice: bool` and inserts the block; `SystemPrompt.version` becomes
`mentor_system_v1+proto_v1` for apprentice sessions (the session's
`prompt_version` column shows both; `session.prefix_changed` fires when
a session toggles the apprentice between runs, as it does for a prompt
bump — the toggle is per session in practice).

### The addendum (`mentor_addendum.md`, ≤ 600 tokens)

Tells the mentor: a local apprentice model pre-processes tool results,
history and repository context; its output arrives in tagged blocks; the
apprentice may be wrong; exact data is always available by reference via
the `expand` tool (M03-04) and nothing is lost; `#need raw` in a reply
is not a thing — the mentor calls `expand`; `#ask-apprentice` lines in
the mentor's text are handled locally (M03-02 strips them from the
transcript the user sees and answers in the next tool-result turn); the
machine-to-machine register (no pleasantries, no restating). It does
**not** describe the roles' internals, so role prompt changes never touch
the mentor's prefix.

### Wire format (`wire.md`, `protocol::wire`)

Blocks are line-oriented, start with a tag line and end at the next tag
or the end of the message; exact text is always inside fenced code
lines. Grammar (parsed by `protocol::wire::parse(&str) -> Vec<Block>`,
emitted by `Block::to_string()`, round-trip tested):

```
#result <tool_call_id> <blob:id> bytes=<n> lines=<n> [status=ok|error exit=<n>]
  err:<code> @ <path>:<line>[:<col>] — <one line>          ← precise, copied verbatim from the raw
  ref <blob:id> L<a>-<b> — <what is there>                  ← a locator; the harness may expand it (M03-03)
  sym <path>#<name> L<a>-<b> [<signature>]
  note <free text, one line>
  ```raw L<a>-<b>
  <verbatim lines the compressor or the harness copied>
  ```
#ctx budget=<tokens> used=<tokens>
  chunk <chunk_id> <path> L<a>-<b> [why: <one line>]
  ```<lang> <path> L<a>-<b>
  <verbatim>
  ```
#state v=<n> step=<n>
  goal: …
  decisions: - …
  files: - <path> (<what changed>)
  open: - …
  tried: - …
#need raw <blob:id> [L<a>-<b>]                              ← apprentice → harness: expand this range verbatim (M03-03)
#ask-apprentice <free text>                                 ← mentor → apprentice, in the mentor's own text
#verdict …                                                  ← reserved (M05)
```

References: `blob:<id>` (the trace blob id of a `tool.result`; also the
`expand` key), `L<a>-<b>` (1-based, inclusive), `err:<code> @
<path>:<line>`, `sym <path>#<name>`, `chunk <id>` (M03-05's chunk ids).
`Ref::parse`/`Display`, `Block::references() -> Vec<Ref>` (M03-03
validates them against the store), `Block::verbatim_bytes()` (M03-09
counts copies vs references).

### Version plumbing

- `mentor.request` payload gains `protocol_version: Option<String>`
  (null for baseline sessions); `record_mentor_request` takes it next to
  `prompt_version`; `mentor_calls.apprentice_applied` is set by M03-02.
- `apprentice.invocation` (M02-04 wrote `protocol_version: null`) is
  filled from the orchestrator (M03-02).
- `sessions.config_json` records `protocol_version` at the first run.
- `daemon.status.protocol {version, hash}`.

### Surfaces

- CLI `harness protocol list` (versions, hash, files), `harness protocol
  show [--role R | --addendum | --wire] [--version vN]`.
- `protocol/CHANGELOG.md` first entry: `v1 — <date> (M03-01)` with the
  file list; a test asserts every `v*/` directory has an entry naming
  every file (M03-11 tightens this).

## Acceptance

- [ ] `cargo test -p apprentice-core protocol::` — `wire::parse` round-
      trips every example in `wire.md` (the test reads the fenced
      examples out of the spec file itself), rejects malformed tag lines
      with a position, and `proptest` shows `parse(emit(b)) == b` for
      generated blocks; `Ref::parse` covers all forms.
- [ ] `build_system(.., apprentice = true)` yields `[core, addendum,
      #workspace]` with three cache breakpoints, `core` byte-identical
      to the baseline's `system[0]`; a snapshot test fixes the addendum
      bytes; the baseline test "no apprentice text" (M01-09) still passes
      for `apprentice = false`.
- [ ] A session run with `apprentice = true` records `protocol_version:
      "v1"` in `sessions.config_json` and in each `mentor.request`
      payload; a baseline session records `null`; `harness trace show`
      prints it.
- [ ] `harness protocol show --role compressor` prints the role prompt;
      `list` prints the manifest hash that `daemon.status` reports.
- [ ] Live (`just live`, reference machine): one apprentice-flagged
      session's second call reports `cache_read_input_tokens` ≥ the
      first call's `input_tokens` − the new tail (the addendum did not
      break the prefix); recorded in the completion notes.
- [ ] The CHANGELOG test passes; `cargo deny`/`clippy` clean.

## Verification

`cargo test -p apprentice-core protocol:: runtime::prompt::`; the live
cache check; `harness protocol show` by hand.

## Notes

- Wording is v1 and *expected* to change; the point of this task is the
  plumbing that makes changing it safe (versions, hashes, records), not
  the prose. Do not polish the prompts here — M03-03/06/07 iterate on
  their own sections and M03-11 gives the loop its tooling.
- Keep the addendum short and stable: it sits in the mentor's cached
  prefix for every apprentice session; every edit to it costs a cache
  rebuild per open session (SPEC §6's "justified by a measured saving").
