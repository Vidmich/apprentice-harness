# M03-11 — Prompt iteration loop

Status: todo
Depends on: M03-01, M03-02
Size: S

## Goal

Changing a prompt is as disciplined as changing a model: protocol
versions are directories, every change to any file under `protocol/`
requires a CHANGELOG entry (a test enforces it), `harness protocol
diff` shows what changed between two versions (or the working tree and
the last released one), `harness protocol bump` creates the next
version directory and its CHANGELOG stub, sessions and records always
say which version they ran, two versions can run side by side for A/B
(`protocol.version` in config, per workspace), and — once M04 exists —
a replay-evaluation report id is attached to the entry before a version
becomes the default.

## Context

SPEC §6 ("Prompt changes are evaluated exactly like model changes (§12);
no prompt is promoted to default without a passing eval report"). M03-01
embedded `protocol/v1/` with `PROTOCOL_VERSION`, the manifest hash and
the first CHANGELOG entry; M01-09 set the precedent with `prompts/
CHANGELOG.md` for the mentor core. M04-06 later provides `harness eval
gate` — the M04 README names this task as its consumer.

## Scope

In: multi-version embedding and selection, the CHANGELOG format and its
test, `diff`/`bump`/`list`, the per-workspace version override, the
prefix-stability lint, the eval-attachment field and the promotion rule
(enforced from M04 on; a warning before), docs.
Out: evaluating prompts (M04), editing prompts in the GUI (not planned),
versioning the mentor core prompt (M01-09's own scheme stays; a core
change is a `prompt_version` bump, orthogonal).

## Design

### Versions

`protocol/v1/`, `protocol/v2/`, … each complete (no inheritance; `bump`
copies the previous directory so a diff is always self-contained).
`protocol/mod.rs` embeds every directory via a `build.rs`-generated
table (`include_str!` per file; the build script walks `protocol/`, so a
new version needs no Rust edit) and exposes `versions() -> [Manifest]`,
`get(version) -> &Protocol`, `default_version()` = the highest whose
CHANGELOG entry is marked `status: default`. Config:

```toml
[protocol]
version = ""        # "" = default; "v2" pins; workspace-overridable (A/B per project)
```

A session records `protocol_version` at its first run (M03-01) and
keeps it: a config change applies to new sessions only (frozen system
text, SPEC §6).

### CHANGELOG (`protocol/CHANGELOG.md`)

```
## v2 — 2026-10-03 — status: candidate        ← candidate | default | retired
Files: mentor_addendum.md (unchanged), roles/compressor.md (changed), roles/compactor.md (unchanged), …
Hash: 3f9c…
Eval: —                                        ← report id from `harness eval replay` (M04); required for status: default once M04 exists
- compressor: shell section asks for exit code first; …
```

The test (`protocol::changelog`): every `v*/` directory has an entry;
the entry's `Hash` equals the directory's manifest hash (so editing a
file without touching the CHANGELOG fails `cargo test`); exactly one
`status: default`; `Files:` lists every file with `changed|unchanged|
added|removed` against the previous version and it matches the real
diff; a `default` entry has an `Eval:` id when the `eval` feature is
compiled in (M04) — until then a warning printed by `harness protocol
lint`.

### Commands

- `harness protocol list` — versions, status, hash, eval id, which
  sessions in the last 30 days ran each (from `sessions.config_json`).
- `harness protocol diff [A] [B]` — unified diff of the two version
  directories (default: the working tree's highest version against the
  previous one); `--role R` narrows; `--stat` per file.
- `harness protocol bump [--from vN]` — creates `v<N+1>/` as a copy,
  appends a `status: candidate` CHANGELOG stub with the file list and
  a placeholder hash, prints what to do next; refuses on an unclean
  `protocol/` (a candidate already without an entry).
- `harness protocol lint` — the CHANGELOG test as a command plus the
  prefix-stability lint: role prompts and the addendum must not contain
  `{{` template markers outside the allowed set (`{{tool}}`, `{{input}}`,
  `{{task}}`, `{{state}}` — all substituted *after* the frozen part), no
  dates, no "today", no absolute paths; the addendum's byte length ≤
  the M03-01 cap.
- `harness protocol promote vN` — flips `status: default` (and the
  previous default to `retired`), requiring `Eval:` when M04 is present
  and `harness eval gate <report>` passes; otherwise `--force` with a
  printed warning that the promotion is unevaluated.
- `just protocol-check` = `lint` + the `changelog` test; in CI.

### A/B in practice

`harness config set --workspace <proj> protocol.version v2` runs the
candidate on one project's new sessions; `harness stats apprentice
--by protocol` (a `by_protocol` group on the M03-09 tables) compares
bypass rate, latency, regrets and estimated savings between versions
over a range — the cheap signal before M04's replay report.

## Acceptance

- [ ] Two embedded versions in a test build (`v1` and a fixture `v2`
      under `tests/fixtures/protocol/`): `get("v2")` returns its files,
      `default_version()` follows the CHANGELOG status, a session with
      `protocol.version = "v2"` records it and the addendum sent is
      v2's; a session started under v1 keeps v1 after the config
      changes.
- [ ] Changelog test: editing `v1/roles/compressor.md` without updating
      the entry's hash fails with a message naming the file; two
      `default` entries fail; a wrong `Files:` line fails.
- [ ] `bump` creates the directory and the stub; `diff` prints the
      change; `lint` flags a date and a stray `{{foo}}`; `promote`
      without `Eval:` warns (pre-M04) — a `#[cfg(feature = "eval")]`
      test asserts it refuses once M04 lands.
- [ ] `harness stats apprentice --by protocol` splits a fixture range
      with sessions on both versions.
- [ ] `just protocol-check` runs in CI and passes on `main`.
- [ ] `docs/protocol-workflow.md`: bump → edit → lint → run sessions on
      a workspace → `stats --by protocol` → (M04) `eval replay` →
      `promote`.

## Verification

`cargo test -p apprentice-core protocol::` (embedding, changelog, lint,
commands' library functions); `just protocol-check`; a manual `bump` /
`diff` / `promote --force` round trip on a scratch branch (revert
after).

## Notes

- The hash-in-CHANGELOG rule is deliberately annoying: an edited prompt
  that nobody wrote down is an unexplained change in every trace
  recorded after it. The mentor core (`prompts/`) should get the same
  test in passing if cheap.
- `promote` writes a file; it is the one command here that changes what
  every new session does. Keep it explicit (no auto-promotion from the
  eval gate) — M06's model promotion may automate its own path later.
