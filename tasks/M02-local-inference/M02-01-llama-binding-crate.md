# M02-01 — llama.cpp binding crate

Status: todo
Depends on: M00-01, M00-12
Size: L

## Goal

`crates/llama/` (`apprentice-llama`, lib `apprentice_llama`): a thin, safe
Rust API over llama.cpp that loads a GGUF model, tokenises, runs batched
decode over several sequences, samples, copies/removes/saves/restores
per-sequence KV state, and enumerates compute devices — built with the
CPU backend everywhere and CUDA / Metal / Vulkan behind cargo features.
It is the only crate in the workspace allowed `unsafe`. Everything in M02
above it (service, session state, bench) is written against this API, so
the binding choice is settled here by a build spike on all three OSes.

## Context

SPEC §7 (single shared service built on llama.cpp; KV cache is the
session state; snapshot / restore / fork), §8 (GGUF, Q4–Q5, 1.5B–8B;
recurrent/hybrid candidates need state save/restore/copy exposed by the
binding). `tasks/README.md`: no `unsafe` outside this crate; CLI and GUI
never link it (they do not depend on `core`). M00-12's CI builds on the
three OSes and must keep passing — the CPU backend is what CI compiles.

## Scope

In: the crate, the build spike and its written decision, the public API
below, backend features, device enumeration, a tiny CLI example, tests
with a small real model, CI wiring (CPU only), `deny.toml` updates.
Out: scheduling, slots, prefix cache, prompts (M02-04); hardware policy
(M02-02); downloads (M02-03).

## Design

### Build spike (first two days, written up in `crates/llama/DECISION.md`)

Candidates, in order of preference:

1. `llama-cpp-2` + `llama-cpp-sys-2` (utilityai): maintained bindings
   with `cuda`, `metal`, `vulkan` features, `bindgen` over a pinned
   llama.cpp submodule; exposes `llama_state_seq_*`, `llama_kv_self_seq_cp
   / seq_rm / seq_keep`, batches with per-token `seq_id` lists, and the
   ggml backend device API.
2. A vendored `llama-cpp-sys` of our own (same llama.cpp commit, `cc` +
   `bindgen`), only if (1) lags on a needed API or fails to build on a
   target.
3. Fallback shape — **managed `llama-server` subprocess** over HTTP
   (`/completion`, `/slots` with save/restore, `/tokenize`): no `unsafe`,
   slower to iterate, no `seq_cp` across slots. Used only if neither (1)
   nor (2) builds on the reference machine with CUDA within the spike.

Spike matrix: Windows 11 + CUDA 12 (reference machine), macOS + Metal
(CI runner), Linux + Vulkan and CPU (CI runner). Record: build time,
binary size, which of the needed calls exist, whether recurrent models
(RWKV-7 / Mamba-family GGUF) load and whether their state save/restore
works. The decision names the crate version and the llama.cpp commit.

### Crate layout

```
crates/llama/
  Cargo.toml        [lints.rust] unsafe_code = "allow"  (only here); features: cpu (default), cuda, metal, vulkan
  DECISION.md       spike result
  src/lib.rs        VERSION, LLAMA_CPP_COMMIT, backend init (once), log redirection into `tracing`
  src/model.rs      Model, ModelParams, ModelMeta
  src/context.rs    Context, ContextParams, Batch, SeqId, Token, decode / sample / kv ops / state
  src/tokenizer.rs  tokenize / detokenize / chat-template helpers
  src/devices.rs    Device { backend, name, memory_total, memory_free }
  src/error.rs      LlamaError
  examples/say.rs   `cargo run -p apprentice-llama --example say -- model.gguf "prompt"`
```

`unsafe` is confined to `context.rs`, `model.rs` and `devices.rs`; every
`unsafe` block carries a `// SAFETY:` comment naming the invariant (the
pointer is owned by `self`, the batch outlives the call, …).

### Public API

```rust
pub fn init() -> Result<()>;                                 // ggml backend init, idempotent; llama log → tracing at debug
pub fn devices() -> Vec<Device>;                             // ggml_backend_dev_* enumeration; CPU is always present

pub struct ModelParams { pub n_gpu_layers: u32 /* u32::MAX = all */, pub main_gpu: i32, pub split_mode: SplitMode, pub use_mmap: bool, pub use_mlock: bool }
pub struct Model(..);                                        // Send + Sync; Arc-shared
impl Model {
    pub fn load(path: &Path, params: &ModelParams, progress: impl FnMut(f32)) -> Result<Model>;
    pub fn meta(&self) -> &ModelMeta;                        // arch, n_layer, n_embd, n_head, n_head_kv, head_dim (k and v), n_ctx_train, vocab size, quant name, chat_template, is_recurrent, size_bytes
    pub fn kv_bytes_per_token(&self, k: KvType, v: KvType) -> u64;   // 2 × n_layer × n_head_kv × head_dim × bytes(type) (0 for recurrent; use state_bytes)
    pub fn tokenize(&self, text: &str, add_special: bool, parse_special: bool) -> Result<Vec<Token>>;
    pub fn detokenize(&self, tokens: &[Token], special: bool) -> Result<String>;
    pub fn apply_chat_template(&self, messages: &[ChatMessage], add_assistant: bool) -> Result<String>;
}

pub struct ContextParams { pub n_ctx: u32, pub n_batch: u32, pub n_ubatch: u32, pub n_seq_max: u32, pub n_threads: u32, pub n_threads_batch: u32, pub flash_attn: bool, pub type_k: KvType, pub type_v: KvType, pub offload_kqv: bool }
pub struct Context(..);                                      // Send, !Sync: one owner thread (M02-04's scheduler)
impl Context {
    pub fn new(model: Arc<Model>, params: &ContextParams) -> Result<Context>;
    pub fn n_ctx(&self) -> u32;  pub fn n_seq_max(&self) -> u32;
    pub fn decode(&mut self, batch: &Batch) -> Result<DecodeOutcome>;      // Ok | NoKvSlot (batch too large for free cells)
    pub fn logits(&self, batch_index: usize) -> &[f32];
    pub fn sample(&mut self, batch_index: usize, sampler: &mut Sampler) -> Token;
    pub fn seq_rm(&mut self, seq: SeqId, from: Pos, to: Option<Pos>);     // drop cells [from, to)
    pub fn seq_cp(&mut self, src: SeqId, dst: SeqId, from: Pos, to: Option<Pos>);
    pub fn seq_keep(&mut self, seq: SeqId);
    pub fn seq_pos_max(&self, seq: SeqId) -> Option<Pos>;
    pub fn state_seq_size(&self, seq: SeqId) -> usize;
    pub fn state_seq_save(&self, seq: SeqId) -> Result<Vec<u8>>;           // KV cells or recurrent state of one sequence
    pub fn state_seq_load(&mut self, seq: SeqId, bytes: &[u8]) -> Result<u32 /* tokens restored */>;
    pub fn memory_used(&self) -> MemoryReport;                              // kv cells used / total, bytes
}

pub struct Batch { .. }                                       // add(token, pos, &[seq], want_logits); clear(); len()
pub struct Sampler { .. }                                     // chain: temperature, top_k, top_p, min_p, repeat penalty, grammar (GBNF, optional), seed; greedy when temperature == 0
pub struct ChatMessage { pub role: String, pub content: String }
pub enum LlamaError { Load{path, reason}, Backend(String), Tokenize, Decode(i32), NoKvSlot, State(String), Unsupported(&'static str) }
```

Positions and sequence ids are newtypes (`Pos(u32)`, `SeqId(u32)`) so a
caller cannot swap them. `Context` is `Send` but not `Sync`: the service
runs it on one thread and hands out channels. A recurrent model returns
`Unsupported("seq_rm inside a sequence")` for a partial `seq_rm`
(only whole-sequence removal and snapshot restore are possible), which
is what M02-04b relies on to pick the rollback strategy.

### Features and build

- `cpu` (default; nothing to install), `cuda` (needs CUDA toolkit on the
  build host; the reference machine), `metal` (macOS only, on by default
  there), `vulkan` (needs the Vulkan SDK / loader). `harnessd` forwards
  them as `llama-cuda`, `llama-metal`, `llama-vulkan`; `apprentice-core`
  gets a `llama` feature that pulls the crate in (M02-04) so `cargo
  build -p apprentice-core` without it stays fast.
- Build time: llama.cpp compiles in the crate's `build.rs`; a
  `LLAMA_CPP_BUILD_DIR` env override lets developers reuse a build and CI
  caches `target/` with `Swatinem/rust-cache` (a separate key for the
  llama artefacts).
- `deny.toml`: llama.cpp is MIT; the binding crate MIT/Apache; add the
  `bindgen` tree to the allow-list.

### Tests

A small test model (~40 MB; a Qwen2.5-0.5B-Instruct Q4 or a purpose-made
tiny GGUF) downloaded once by `just llama-fixtures` into
`target/fixtures/` (not committed; tests skip with a clear message when
absent; CI pulls it from the Hugging Face URL in the justfile and caches
it).

## Acceptance

- [ ] `DECISION.md` records the spike on the three OSes with build times
      and the chosen crate + llama.cpp commit; CUDA verified on the
      reference machine, Metal and Vulkan/CPU on the CI runners.
- [ ] `cargo build -p apprentice-llama` (CPU) passes on Windows, macOS,
      Linux; `--features cuda` on the reference machine; `cargo clippy
      -D warnings` clean; every `unsafe` block has a `SAFETY:` comment
      (a test greps the sources).
- [ ] With the fixture model: tokenize/detokenize round-trips a UTF-8
      string with emoji; greedy generation of 16 tokens from a fixed
      prompt is byte-identical across two runs and across CPU and GPU
      (seeded, temperature 0).
- [ ] Two sequences in one context decode interleaved in one batch and
      produce the same tokens as when decoded alone.
- [ ] `seq_cp` of a 200-token prefix into a fresh sequence, then
      continuing, produces the same output as re-prefilling the prefix;
      `seq_rm` of the tail rewinds correctly (the next token matches).
- [ ] `state_seq_save` → new context → `state_seq_load` → the next
      sampled token equals the one from the original context; the byte
      size matches `state_seq_size`.
- [ ] `devices()` lists the CPU everywhere and the RTX 3080 Ti with its
      VRAM under `cuda`; `kv_bytes_per_token` for the fixture model
      matches the hand-computed value.
- [ ] A recurrent GGUF (RWKV-7 or Mamba, smallest available) loads and
      round-trips `state_seq_save/load`, or the decision records why not
      (llama.cpp support at the pinned commit).

## Verification

`cargo test -p apprentice-llama` (CPU, fixture model) locally and in CI;
`--features cuda` run once on the reference machine; `cargo run --example
say` as the smoke test of the whole path.

## Notes

- Keep the API surface minimal and unopinionated: no prompts, no chat
  loop, no scheduling. Anything that needs a policy belongs in M02-04.
- llama.cpp's KV-cache API has been renamed more than once
  (`llama_kv_cache_*` → `llama_kv_self_*` → `llama_memory_*`); pin the
  commit and wrap the names so the service never sees them.
- Log noise: llama.cpp prints to stderr by default; route through
  `llama_log_set` into `tracing` at `debug`, `warn` for its warnings.
- If the fallback shape (3) wins, the public API above is implemented by
  a `ServerBackend` and `state_seq_*` map onto `/slots/<id>?action=save`;
  `seq_cp` becomes "unsupported" and M02-04 loses cross-slot prefix
  sharing — say so in the decision.
