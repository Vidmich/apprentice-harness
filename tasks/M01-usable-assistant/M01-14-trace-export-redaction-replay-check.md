# M01-14 — Trace export bundle, redaction, replay check

Status: done
Depends on: M00-06, M01-08, M01-10
Size: M

## Goal

Traces can leave the machine safely and come back intact: a portable
bundle format for a selection of sessions/agents with their blobs, a
redaction pass for secrets and configurable patterns, import into another
store, and a `replay-check` that proves any recorded mentor request can be
reconstructed byte-for-byte from the stored data — the property the
evaluation engine (M04) and dataset builders (M05) depend on.

## Context

SPEC §9 (export/import, redaction hooks, replayability), §15 (privacy).
The M01 exit criterion: "trace export produces a corpus; a replay of any
recorded step reproduces the exact mentor request".

## Scope

In: bundle format, export/import commands and RPCs, redaction rules and
report, replay-check command, integrity verification, Python reader for
bundles.
Out: anonymisation of code content (out of scope; the user decides what to
export), cloud upload.

## Design

### Bundle format

Directory or `.tar.zst`:

```
bundle/
  manifest.json      {format_version: 1, created_at, harness_version, schema_version, sessions: [...], counts, redaction: {applied: bool, rules: [...], replacements: n}}
  sessions.jsonl     one session row per line (incl. workspace snapshot info)
  agents.jsonl / steps.jsonl / events.jsonl / mentor_calls.jsonl / session_messages.jsonl / permissions.jsonl
  blobs/<aa>/<sha256>   referenced blobs only (after redaction, blobs are re-hashed; a map old→new is kept in manifest.redaction.blob_map)
```

Selection: `--session ID...`, `--workspace ID`, `--since/--until`,
`--all`. Events of kind `feedback` and future eval events included when
present.

### Redaction

Pipeline applied to every text blob and payload string when
`--redact` (default ON for export, OFF for local operations):

1. Built-in secret detectors: Anthropic/OpenAI/GitHub/AWS key formats,
   `Bearer …`, `password=…`, private key PEM blocks, `.env`-style
   `KEY=value` where KEY matches `*_SECRET|*_TOKEN|*_KEY|PASSWORD`.
2. User patterns from `<config_dir>/redact.toml` (`[[pattern]] regex =
   "...", replace = "<REDACTED:name>"`), and workspace `.harness/redact.toml`.
3. Path redaction option `--redact-paths`: replace the workspace root and
   the user home directory with `<WS>` / `<HOME>`.

Replacements are deterministic tokens `<REDACTED:kind:n>` (same secret →
same n within a bundle) so structure is preserved for training. Report
printed and stored in the manifest (counts per rule, never the values).
`mentor.request` blobs are redacted too; the manifest marks them
`replayable: false` if any replacement touched them — replay-check on an
imported redacted bundle compares against the redacted stored body, not
the original.

### Import

`harness trace import <bundle> [--into-workspace ID]` inserts with new
session ids if they collide (`--keep-ids` to preserve), verifies blob
hashes, records `import` provenance in `sessions.config_json`.

### Replay check

`harness trace replay-check [--session ID | --agent ID | --call ID] [--rebuild]`:
- default: for each `mentor.request`, verify the blob exists, hashes match
  the payload `request_hash`, the JSON parses into `MentorRequest`, and
  re-serialising it yields the same bytes (canonical serialisation test).
- `--rebuild`: reconstruct the request from `session_messages` + stored
  session config (prompt version, tools hash) using the runtime's
  `build_request` and compare to the blob; report any diff (this catches
  drift in prompt assembly and proves M04's replay evaluator will be able
  to substitute apprentice output at the right places).

Exit non-zero on any mismatch; `--json` report per call.

### Python

`ml/src/apprentice_ml/traces/bundle.py`: `load_bundle(path) -> Bundle`
with iterators over sessions/agents/steps/events and lazy blob access;
`apprentice-ml traces stats <bundle|db>` extended to bundles.

## Acceptance

- [x] Export → import into a fresh store → export again yields an
      identical bundle (modulo ids when not `--keep-ids`).
      — `core/tests/bundle.rs::export_import_export_is_the_same_bundle`:
      every `.jsonl` and blob file of the second export equals the
      first's (the manifest's `created_at` and the `import` provenance
      in `config` aside), through a directory and through a `.tar.zst`;
      the copy passes `replay-check --rebuild`; a second import of the
      same bundle gets fresh ids with every reference (payload `call_id`s
      included) remapped, and `--keep-ids` is a `conflict`.
- [x] Redaction: a fixture session containing a fake API key, a PEM block
      and a user pattern is exported with all three replaced consistently;
      report counts match; originals absent from every file in the bundle
      (grep). — `redaction_replaces_secrets_everywhere_and_the_copy_still_replays`:
      a real trajectory whose prompt holds the key twice, a PEM block and
      `ACME-4242` (a `ticket` pattern in `redact.toml`), exported with
      `--redact-paths`; the report counts the three rules and `paths`,
      `secrets == 3`, every request body touched (`replayable: false`);
      no file of the bundle contains an original or the workspace root;
      the bodies, the messages, the events and the agent's task carry the
      same `<REDACTED:anthropic_key:1>`; the imported copy passes
      `replay-check --rebuild` against the redacted bodies.
- [x] `replay-check` passes for all calls of a real dogfood session;
      `--rebuild` passes for sessions created after M01-09 (prompt v1).
      — `replay_check_passes_for_a_real_trajectory_and_catches_a_tampered_body`
      (the three-step trajectory of the loop tests, all five checks; a
      tampered body fails `hash` by call id; a rewritten message fails
      `rebuild` naming `$.messages[0].content[0].text`; a title call is
      `skipped` under `--rebuild`). The run on this machine's store is
      in the completion notes.
- [x] Corrupt a blob in a bundle → import refuses with the blob id.
      — `a_corrupted_bundle_blob_is_refused_by_id`: `blob_corrupted`
      naming the id, nothing written; a missing file is `blob_missing`;
      a directory without a manifest and a `format_version: 2` bundle are
      refused with the reason.
- [x] Python loader iterates a bundle and matches the counts in the
      manifest. — `ml/tests/test_bundle.py`: `Bundle.counts()` equals
      `manifest.counts` for a directory and a `.tar.zst` (unpacked with
      `zstandard`, removed on `close()`), rows iterate in file order,
      `read_blob` verifies the hash, `request_body(call)` returns the
      stored bytes; `apprentice-ml traces stats <bundle|db>`.

## Verification

Integration tests with fixture sessions; manual run on real dogfood data
before the first export leaves the machine.

## Notes

- Compression: `zstd` level 6; a month of heavy use is expected to be a
  few GB raw (shell outputs dominate).

## Completion notes (2026-09-13)

`cargo test -p apprentice-core` (`tests/bundle.rs` new: five
integration tests over a real trajectory; unit tests in `bundle::`),
`-p apprentice-api --test snapshots` (`trace_export`, `trace_import`,
`trace_replay_check` goldens), `-p harness` (parse tests, output tests,
`tests/trace.rs` over a live router); `uv run pytest` (9, `test_bundle.py`
new); `pnpm test` (39; the three methods in `api.ts`, goldens read).

- Bundle (`crates/core/src/bundle/`): `manifest.json` (`format_version:
  1`, `created_at`, `harness_version`, `schema_version`, the resolved
  `selection`, one entry per session with its counts, `counts`,
  `redaction`), `workspaces.jsonl`, `sessions.jsonl`, `agents.jsonl`,
  `steps.jsonl`, `events.jsonl`, `mentor_calls.jsonl`,
  `session_messages.jsonl`, `blobs.jsonl` (id, size, media type,
  `pruned`), `blobs/<aa>/<sha256>`. Rows are the columns as JSON, the
  JSON columns parsed. A `.tar.zst` output is packed (zstd 6, sorted
  entries, deterministic headers) from a temporary directory beside it;
  a packed input is unpacked beside itself for the import and removed.
  Rows are read in a fixed order (sessions by `created_at`, agents by
  `created_at`, steps by `seq`, events by `seq`, calls by `started_at`,
  messages by `seq`, workspaces and blobs by id), so the same data
  makes the same files.
- Selection: `--session ID` (repeatable), `--workspace ID`,
  `--since` / `--until` on `sessions.created_at` (the `stats` range
  grammar), `--all`; nothing selecting is `invalid_params`, an unknown
  session `not_found`, an empty match an error.
- Redaction (`bundle/redact.rs`), on by default for an export: built-in
  detectors `anthropic_key`, `openai_key`, `github_token`, `aws_key`,
  `bearer` (the token after `Bearer`), `pem` (the whole block),
  `env_secret` (`X_SECRET|X_TOKEN|X_KEY|PASSWORD=value` at a line
  start), `password` (`password[=:]value`); then `[[pattern]]` entries
  of `<config_dir>/redact.toml` (`name`, `regex`, optional literal
  `replace`; a `(?P<secret>…)` group narrows what is replaced) and of
  each exported workspace's `.harness/redact.toml`; `--redact-paths`
  replaces the workspace roots (both slash forms) and the home directory
  with `<WS>` / `<HOME>`. A match becomes `<REDACTED:kind:n>`, `n` per
  distinct value across the bundle (first seen). Applied to every text
  field, payload and message, and to text blobs: JSON blobs literal by
  literal (unescaped, redacted, re-escaped — every other byte stays, so
  a body stays valid JSON whatever a pattern matches), other text as a
  whole. A changed blob is re-hashed; `blob_map` (old → new), event
  `blob_id`s and `$blob` refs follow; a touched `mentor.request` payload
  gets `request_hash` and `bytes` of the redacted body and
  `redacted: true`, the call row's `request_bytes` too, and the manifest
  says `replayable: false` with `touched_requests`. The report holds
  matches per rule and the distinct count, never a value.
- Import (`bundle/import.rs`, `trace/bundle.rs`): the manifest's format
  and schema versions are checked, every blob file hashed before any
  write (`blob_corrupted` / `blob_missing` name the id), then one
  transaction: sessions keep their ids unless taken (then the session
  and everything under it get fresh ids, references and id strings in
  payloads remapped) or `--keep-ids` (a taken id is a `conflict`);
  `config_json.import = {bundle, bundle_created_at, harness_version,
  imported_at, redacted, original_session_id}`; blob rows are registered
  with one reference per event naming them, files written
  content-addressed first; messages go through `put_message_at` so the
  search index is rebuilt; `updated_at` stays the bundle's. A session's
  workspace is kept when its root is registered here or exists on disk
  (then registered under the bundle's id); `--into-workspace ID`
  attaches everything to that one; otherwise it stays unattached with
  its `workspace_path`.
- `replay-check` (`bundle/replay.rs`): per call — `blob` (the request
  event has a body file), `hash` (the file hashes to its id, the
  payload's `request_hash` and `bytes` agree), `parse` (the body is the
  wire request: `mentor::WireRequest`, the same shape
  `AnthropicMentor::request_body` now serialises), `canonical`
  (serialising the parsed body gives the same bytes), and with
  `--rebuild`: the first `message_count` stored messages of the session
  loaded into a `Conversation` with the body's system blocks and tools
  (cache flags stripped; the core block must be `mentor_system_v1` when
  the payload says so; the tools must hash to the session's
  `tools_hash` unless a `session.prefix_changed` event exists), the
  body's model, `max_tokens`, `thinking` and `effort`, then
  `Conversation::request()` → `request_body()` compared byte for byte;
  a difference names the first JSON path. Title calls are `skipped`
  for the rebuild. The CLI exits 2 on any failure, `--json` prints the
  report first.
- RPC: `trace.export {output (absolute), session_ids, workspace_id,
  since, until, all, redact (default true), redact_paths}` → `{path,
  manifest}`; `trace.import {path, into_workspace, keep_ids}` →
  `{sessions: [{from, to}], counts, redacted, blobs_written}`;
  `trace.replay_check {session_id | agent_id | call_id, rebuild}` →
  `ReplayReport`. Relative paths are refused (the daemon's working
  directory is not the caller's); the CLI resolves them.
- CLI: `harness trace export [--session ID]... [--workspace ID] [--since
  --until] [--all] [--no-redact] [--redact-paths] [-o PATH]` (default
  `harness-traces-<utc>Z.tar.zst` in the current directory; a path
  without `.tar.zst` is a directory), `trace import <PATH>
  [--into-workspace ID] [--keep-ids]`, `trace replay-check [--session |
  --agent | --call ID] [--rebuild]`.
- Python: `apprentice_ml.traces` is a package (`traces.py` moved to
  `traces/__init__.py`, `SUPPORTED_SCHEMA` 3); `traces/bundle.py` has
  `load_bundle`, `Bundle` (rows per table, `read_blob` with the hash
  check, `request_body(call)`, `counts()`), `stats`; `apprentice-ml
  traces stats <path>` takes a database or a bundle (`--db` kept).
  `zstandard` is a dependency for `.tar.zst`.

Manual run (2026-09-13), over real daemons on Windows: this machine's
store has no session yet (dogfooding starts with M01-15), so
`harness trace replay-check --rebuild` there checked 0 calls and
`trace export --all` refused with "no session matches"; in a scratch
`--home` with a `redact.toml` (`ticket`), a session titled with a fake
key and `ACME-77` was exported packed with `--redact-paths` (the report
listed `anthropic_key 2`, `ticket 2`, `paths 3`) and as a plain
directory, imported into a second scratch home (the title read
`manual <REDACTED:ticket:2> <REDACTED:anthropic_key:1>`, the workspace
`<WS>`), refused again under `--keep-ids` (`conflict`), and read by
`apprentice-ml traces stats <bundle.tar.zst>`. The replay checks over a
real trajectory are the integration tests; the first dogfood session
gets the run from the notes above before its export leaves the machine.

Deviations / decisions:

- No `permissions.jsonl`: permission decisions are `permission.decision`
  events (in `events.jsonl`); session rules live in memory. Blob
  metadata gets its own `blobs.jsonl` (the design listed only the
  files) so an import knows media types and pruned blobs.
- The rebuild takes the system blocks and the tool definitions from the
  stored body rather than from the store: the workspace block is built
  live at the session's first run and not persisted (a resumed session
  builds it again), so it cannot be reconstructed from rows. What is
  checked instead: the core block against the embedded
  `mentor_system_v1` for that `prompt_version`, the tools against the
  session's `tools_hash`, and everything the runtime assembles —
  messages, cache breakpoints, tool order, the settings — byte for byte.
- `mentor_calls.kind` still has no `count_tokens` / `other`
  (M01-13); `WireRequest` is the M00-11 body shape made a type, so the
  bytes on the wire and in the trace are unchanged.
- A session's workspace comes back on import only when the root is
  registered or exists here; a redacted bundle (`<WS>`) never registers
  one. The design's `--into-workspace` overrides.
- The default export name uses UTC (`…Z`): the CLI has no local-offset
  source without a daemon round trip.
- The Python reader unpacks a `.tar.zst` to a temporary directory
  (lazy blob access needs random access); `zstandard` became a
  dependency of `apprentice-ml` instead of a stdlib-only reader for the
  packed form.
