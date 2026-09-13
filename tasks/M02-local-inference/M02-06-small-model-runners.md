# M02-06 — Small-model runners (encoders and classifiers)

Status: todo
Depends on: M02-03, M02-04
Size: M

## Goal

A second execution path inside the same `InferenceService` façade for
small encoder/classifier models — embedding models, cross-encoder
rankers, sequence classifiers (the future gate) — running through ONNX
Runtime with millisecond latency on CPU or GPU, loaded from `kind =
"encoder"` manifests, with batching, a warm pool, and the same trace
records and metrics as the generative path. The context selector's
ranker (M03-06 prompted, M06 trained) and the gate (M09) run here.

## Context

SPEC §7 ("Small dedicated models (rankers, gates) run in the same service
or in-process (ONNX/candle) and are expected to cost milliseconds"), §5.2
("a small encoder/cross-encoder ranker (fast, cheap to train) with the
generative model only for query formulation"), §8 ("purpose-built
components that *are* trained from scratch on top of pretrained encoders
(rankers, gates) are part of the design"), §11 (ONNX export for small
models from the Python side). M02-03 defined the `encoder` manifest kind.

## Scope

In: the runner, the three model shapes (embed, score pairs, classify),
tokenisation, batching and threading, manifest fields, export format
contract with `ml/`, RPC/CLI for a manual call, tests with a public
small model.
Out: training or exporting the models (M06), using them in a role (M03/
M09), GPU execution providers beyond what `ort` ships as prebuilt
(CUDA on Windows/Linux, CoreML on macOS — enabled when available, CPU is
the guaranteed path).

## Design

### Runtime choice

`ort` (pyke, 2.x) with the `download-binaries` strategy for the CPU
provider and, behind a `onnx-cuda` feature, the CUDA provider; `ort` is
a safe API (no `unsafe` in our code; the crate stays outside the llama
crate's exemption). `candle` is the recorded alternative if `ort`'s
binary distribution becomes a problem on a platform; the `Encoder` trait
below hides the choice. Tokenisation via the `tokenizers` crate reading
the manifest's `tokenizer.json` (HF format, the same file the Python
side exports).

### Manifest (`kind = "encoder"`, extends M02-03)

```toml
kind = "encoder"
task = "embed"                    # embed | rank | classify
files = ["model.onnx", "tokenizer.json"]
[encoder]
max_length = 512
pooling = "cls"                   # embed: cls | mean | none (model pools)
normalize = true                  # embed: L2-normalise
inputs = ["input_ids", "attention_mask", "token_type_ids"]   # names, in order; absent → probed from the graph
output = "logits"                 # rank/classify: output name; embed: last_hidden_state or pooler_output
labels = ["local", "remote", "ask"]   # classify only
dtype = "fp32"                    # fp32 | fp16 | int8 (the exported graph's)
```

### API (part of `apprentice_core::inference`)

```rust
pub trait Encoder: Send + Sync {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;                 // task = embed
    fn score(&self, pairs: &[(&str, &str)]) -> Result<Vec<f32>>;              // task = rank  (query, candidate) → relevance logit
    fn classify(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;              // task = classify → probabilities per label
    fn info(&self) -> EncoderInfo { id, task, max_length, dim: Option<usize>, labels, provider: "cpu"|"cuda"|"coreml" };
}
impl InferenceService {
    pub async fn encoder(&self, id: &str) -> Result<Arc<dyn Encoder>>;          // loads on first use, kept in a pool (inference.encoder_pool, default 3, LRU)
    pub async fn embed(&self, req: EncodeRequest) -> Result<EncodeResponse>;    // role, model, inputs, budget, priority → outputs + Usage{items, tokens, latency_ms}
    pub async fn score(&self, req: ScoreRequest) -> Result<ScoreResponse>;
    pub async fn classify(&self, req: ClassifyRequest) -> Result<ClassifyResponse>;
}
```

Execution: a dedicated `rayon`-style thread pool (`inference.encoder_
threads`, default = physical cores / 2, min 1) separate from the llama
scheduler thread, so a 200-pair rerank never delays a token. Inputs are
tokenised (truncation to `max_length`, padding to the batch's longest),
batched at `inference.encoder_batch` (default 32) and run in one
`session.run`; long inputs (a chunk of code) are truncated head-first
with a record in the response (`truncated: n`). Budgets and priority
(M02-05) apply: a rank request that cannot start within `start_by` is
bypassed the same way; encoder work always counts as one "slot" of its
own pool, never a llama slot.

Trace: `apprentice.invocation {role, model, kind: "encoder", task,
items, tokens_in, latency_ms, bypassed, provider}`; inputs/outputs to a
blob under `capture_io` like the generative path (embeddings as f32
arrays in JSON are big — store `float16` packed bytes with a media type
of their own).

### Export contract (`ml/`)

`apprentice_ml.export.onnx` (M06 fills the trainers; the contract is
fixed here): a function that takes a HF model dir and writes
`model.onnx` (opset 17, dynamic batch and sequence axes, fp32 or
`--fp16`), `tokenizer.json`, and a `manifest.toml` with the fields above,
plus a small `ml/tests/test_onnx_export.py` that exports a tiny public
model (e.g. `sentence-transformers/all-MiniLM-L6-v2` for `embed`,
`cross-encoder/ms-marco-MiniLM-L-6-v2` for `rank`) and checks that the
Rust runner's outputs match PyTorch's within 1e-3 (the fixture pair is
committed under `crates/core/tests/fixtures/onnx/` — MiniLM-L6 is ~22 MB
fp32; use the int8 export, ~6 MB, to keep the repo small, or download
in `just llama-fixtures`).

### Surfaces

- RPC `inference.encode {model, task, inputs | pairs}` (manual use and
  the GUI's "try it"); CLI `harness apprentice encode --model ID
  --task rank --query "…" --candidates a b c`.
- `models catalog` gains the two MiniLM entries and `bge-reranker-base`
  as the recommended rank model.

## Acceptance

- [ ] The fixture `embed` model: 8 sentences → 8 vectors of the declared
      `dim`, L2-normalised, cosine of paraphrases > 0.7 and of unrelated
      pairs < 0.3; outputs match the Python reference within 1e-3.
- [ ] The fixture `rank` model: (query, 50 candidates) scores match the
      reference ordering; latency < 50 ms for the batch on the reference
      machine's CPU (`#[ignore]` timing test; CI asserts correctness
      only).
- [ ] `classify` with a two-label toy model (exported in the test)
      returns probabilities summing to 1 in the manifest's label order.
- [ ] Truncation at `max_length` is reported; a manifest whose input
      names do not match the graph fails at load with the graph's actual
      names in the error.
- [ ] Concurrency: 4 rank requests + 2 llama generations in flight — the
      generations' per-token latency is unchanged (mock llama backend
      with a timer) and the rank requests complete in parallel on the
      encoder pool.
- [ ] Records: each call writes `apprentice.invocation {kind: encoder}`;
      `harness stats apprentice --by role` counts them; a bypass under a
      100 ms `start_by` with the pool saturated is recorded like a llama
      bypass.
- [ ] CUDA provider (`onnx-cuda`) loads on the reference machine and the
      embed test passes on it; the CPU path is what CI runs.

## Verification

`cargo test -p apprentice-core inference::encoder::` with the fixture
models; `uv run pytest ml/tests/test_onnx_export.py`; the timing test on
the reference machine.

## Notes

- `ort`'s prebuilt binaries are downloaded at build time; pin the
  version and record the hashes (`ort` verifies them); `deny.toml` needs
  the ONNX Runtime licence (MIT).
- Keep the encoder path independent of llama.cpp so a CPU-only machine
  without the `llama` feature can still rank (and so M02-09's remote
  service can serve encoders too).
