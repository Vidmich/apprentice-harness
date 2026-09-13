# M02-03 — Model manager and manifest

Status: todo
Depends on: M00-03, M00-06, M02-02
Size: M

## Goal

`apprentice_core::models`: a local model store under `<data_dir>/models/`
where every model, adapter and small encoder has a `manifest.toml`
(source, hash, size, licence, quantisation, context, architecture, rated
roles and eval scores), a built-in catalogue of candidate base models,
resumable verified downloads from Hugging Face, per-role assignment in
config, and `harness models list|pull|remove|info|assign|catalog|plan`.
The service (M02-04) loads what the manifest describes; the eval engine
(M04) writes the scores back.

## Context

SPEC §3.1 Model manager ("downloads/validates GGUF models and adapters,
detects hardware, picks quantization/offload settings, maintains a model
manifest (which roles a model/adapter is rated for and its eval
scores)"), §8 (candidates: Qwen2.5-Coder 1.5B/3B/7B, Qwen3 4B/8B text-only,
a recurrent/hybrid family; permissive licences), §10 ("everything
promoted is versioned; roll back is one command" — the adapter side of
that is M06, the manifest shape is fixed here).

## Scope

In: store layout, manifest schema, catalogue, downloader, verification,
assignment config, RPCs with progress streaming, CLI, manifest updates
from eval (the API, not the eval).
Out: training artefact registry (M06 adds entries through the same
manifest), GUI (M02-08), ONNX encoders' runtime (M02-06 — their
manifests are defined here).

## Design

### Store layout

```
<data_dir>/models/
  <id>/manifest.toml              id = catalogue id or user-chosen slug: qwen2.5-coder-3b-q4_k_m
  <id>/<file>.gguf                the weights (one file; sharded GGUF later)
  <id>/adapter.gguf               kind = "adapter" entries (LoRA, from M06)
  <id>/model.onnx + tokenizer.json   kind = "encoder" entries (M02-06)
  <id>/.partial/                  in-flight downloads (resumable)
  index.toml                      cache of id → summary, rebuilt from manifests on start
```

### Manifest schema (v1)

```toml
schema = 1
id = "qwen2.5-coder-3b-q4_k_m"
kind = "gguf"                     # gguf | adapter | encoder
name = "Qwen2.5-Coder-3B-Instruct Q4_K_M"
family = "qwen2.5-coder"
architecture = "qwen2"            # from GGUF metadata; "rwkv7" / "mamba2" for recurrent
is_recurrent = false
parameters = "3B"
quant = "Q4_K_M"
context_train = 32768
licence = "Apache-2.0"
licence_url = "https://..."
size_bytes = 2019377152
sha256 = "..."
files = ["qwen2.5-coder-3b-instruct-q4_k_m.gguf"]
chat_template = "chatml"          # informative; the GGUF's own template is used

[source]
provider = "huggingface"          # huggingface | url | local | trained (M06)
repo = "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF"
revision = "main"                 # resolved to a commit sha at pull time
file = "qwen2.5-coder-3b-instruct-q4_k_m.gguf"
pulled_at = "2026-09-20T10:00:00Z"

[base]                            # adapter only
model = "qwen2.5-coder-3b-q4_k_m"

[roles]                           # rated roles; scores written by M04/M06, empty until then
compressor = { rated = true, sufficiency = 0.93, report = "eval/2026-10-01-replay.json" }
compactor  = { rated = false }

[meta]                            # free-form, e.g. n_layer / n_head_kv / head_dim copied from GGUF for the planner without loading the model
n_layer = 36
n_head_kv = 2
head_dim = 128
n_embd = 2048
```

Rules: `id` is `[a-z0-9._-]+`; a manifest whose `sha256` does not match
the file is `state = "corrupt"` in listings and refused by the service;
`schema` newer than the binary → refuse the entry, keep the rest.

### Catalogue

`crates/core/models/catalog.toml` (embedded) lists the M02 candidates
with repo, file, quant, sizes and licences, in two tiers: 3–4B (Q4_K_M,
Q5_K_M) for any GPU/CPU, 7–8B (Q4_K_M) for 8 GB+ GPUs, plus the
smallest available recurrent/hybrid candidate (RWKV-7 or a Falcon-H1 /
Nemotron-H class model, whichever llama.cpp supports at M02-01's pinned
commit — the entry is marked `experimental = true`). `harness models
catalog` prints it with a "fits" column from the planner (M02-02) for
this machine. The catalogue is data, not policy: pulling anything else
by repo/file or local path is one command.

### Downloader

- `https://huggingface.co/<repo>/resolve/<revision>/<file>` (and
  `/api/models/<repo>` to resolve the revision and read the file's
  sha256 from the LFS pointer, so the hash is known *before* the
  download and verified at the end). `HF_TOKEN` from the secret store
  (`huggingface_token`, optional; `harness auth set-key --provider
  huggingface`) for gated repos.
- `reqwest` streaming into `<id>/.partial/<file>` with `Range` resume
  when a partial exists (server ETag must match), sha256 running while
  writing, atomic rename at the end, 3 retries with backoff on network
  errors, disk-space check first (size + 5 %).
- One download at a time per id; the RPC returns a subscription and
  streams `models.progress {id, bytes, total, bytes_per_s, eta_s,
  phase: resolving|downloading|verifying|done|failed}` (same mechanism
  as `agent.run`), so the GUI and `harness models pull` show a bar.
- Local import: `harness models add <path.gguf> [--id X]` copies or
  links (`--link`) and writes a manifest by reading the GGUF metadata
  through `apprentice_llama::Model::meta` (mmap, no full load).

### Assignment

```toml
[apprentice.roles]                # workspace-overridable (already under `apprentice.`)
default    = "qwen2.5-coder-3b-q4_k_m"          # what the service loads when a role has no entry
compressor = "qwen2.5-coder-3b-q4_k_m"
compactor  = "qwen2.5-coder-3b-q4_k_m@compactor-lora-v3"   # model@adapter
selector   = "bge-reranker-base"                # an encoder id (M02-06)
```

`harness models assign <role> <model[@adapter]>` validates that the id
exists, that an adapter's `base` matches, that the manifest rates the
role or `--force` is given, and writes the config key. `harness models
rollback <role>` restores the previous value (the last five assignments
per role are kept in `models/assignments.log`).

### RPC and CLI

`models.list {kind?}` → entries with `state` (`ready | partial | corrupt
| missing_file`) and size; `models.info {id}` → manifest + planner table
for this machine; `models.catalog`; `models.pull {id | repo, file,
revision?, id?}` → `{subscription}`; `models.remove {id, keep_files?}`;
`models.add {path, id?, link?}`; `models.assign {role, target, force?}`;
`models.rollback {role}`; `models.update_manifest {id, roles?}` (used by
M04/M06 to write scores; validated against the schema).

CLI mirrors: `harness models list|info ID|catalog|pull TARGET|add PATH|
remove ID|assign ROLE TARGET|rollback ROLE|plan ID`.

## Acceptance

- [ ] Manifest round-trips through `toml` with unknown keys under
      `[meta]` preserved; a manifest with `schema = 2` is skipped with a
      warning, a corrupt hash shows `corrupt` in `models list`.
- [ ] Pull against a `wiremock` "Hugging Face" (LFS pointer endpoint +
      a 5 MB file): resolves the revision, streams progress events,
      verifies the hash, writes the manifest; interrupting at 60 % and
      pulling again resumes with a `Range` request (the mock asserts it)
      and finishes; a wrong hash leaves no `manifest.toml` and a
      `.partial` that the next pull discards (ETag changed).
- [ ] `models add` on the M02-01 fixture GGUF writes a manifest whose
      `[meta]` matches `Model::meta()`.
- [ ] `assign` refuses an unknown id, an adapter on the wrong base and
      an unrated role without `--force`; `rollback` restores the previous
      target; workspace-layer assignment overrides the user layer.
- [ ] `harness models catalog` on the reference machine marks the 3B and
      7B entries as fitting and shows the planner's slot/ctx numbers.
- [ ] Removing an assigned model warns and clears the assignment unless
      `--keep-assignment`.

## Verification

`cargo test -p apprentice-core models::` (wiremock for the downloads);
one real pull of the 3B candidate on the reference machine, timed, with
the resulting manifest committed to the task's completion notes.

## Notes

- Licences: record them, do not enforce. The catalogue only lists
  permissively licensed models; a user-added model is their call.
- Sharded GGUF (`-00001-of-00003`) is out of scope for M02 (nothing in
  the 8B tier needs it at Q4/Q5); the `files` array leaves the door
  open.
- Keep the manifest the single source of truth for what the service and
  the GUI know about a model; never read GGUF metadata at listing time.
