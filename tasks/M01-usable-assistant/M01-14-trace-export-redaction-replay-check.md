# M01-14 — Trace export bundle, redaction, replay check

Status: todo
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

- [ ] Export → import into a fresh store → export again yields an
      identical bundle (modulo ids when not `--keep-ids`).
- [ ] Redaction: a fixture session containing a fake API key, a PEM block
      and a user pattern is exported with all three replaced consistently;
      report counts match; originals absent from every file in the bundle
      (grep).
- [ ] `replay-check` passes for all calls of a real dogfood session;
      `--rebuild` passes for sessions created after M01-09 (prompt v1).
- [ ] Corrupt a blob in a bundle → import refuses with the blob id.
- [ ] Python loader iterates a bundle and matches the counts in the
      manifest.

## Verification

Integration tests with fixture sessions; manual run on real dogfood data
before the first export leaves the machine.

## Notes

- Compression: `zstd` level 6; a month of heavy use is expected to be a
  few GB raw (shell outputs dominate).
