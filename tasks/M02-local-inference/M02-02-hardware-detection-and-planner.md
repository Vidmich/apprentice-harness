# M02-02 — Hardware detection and the memory planner

Status: todo
Depends on: M00-04, M02-01
Size: S

## Goal

`apprentice_core::hardware` produces a `HardwareProfile` (GPUs with
backend and VRAM, CPU cores and SIMD features, RAM, OS) from real probes,
and a `Planner` that turns a model's metadata plus the profile into
context/slot settings — GPU layers, KV type, context per slot, slot
count — with an explicit memory table (weights + Σ slots × context ×
per-token KV bytes). `harness doctor` shows the profile; the service
(M02-04), the model manager (M02-03) and the bench (M02-07) consume the
plan.

## Context

SPEC §7 ("VRAM budget = weights + Σ slots × context × per-token KV size;
slot count and per-slot context are configured per machine and reported
by the bench"), §3.1 Model manager (detects hardware, picks quantization
/ offload settings). M00-04 left `harness doctor`'s hardware section as
an `nvidia-smi` stub to be replaced here. The M02 README asks for the
default slot/context table for the 12 GB reference GPU to come out of
this planner, not out of guesswork.

## Scope

In: probes, profile type and RPC, planner with the memory formula,
`doctor` integration, config knobs the planner honours, unit tests with
recorded profiles.
Out: live VRAM monitoring during inference (M02-05 reports the service's
own numbers), multi-GPU splitting policy beyond "main GPU" (documented
as a later extension).

## Design

### Profile

```rust
pub struct HardwareProfile {
    pub os: String, pub arch: String,
    pub cpu: Cpu { name, physical_cores, logical_cores, features: Vec<String> /* avx2, avx512, fma, f16c, neon, sve */ },
    pub ram: Mem { total, available },
    pub gpus: Vec<Gpu { index, backend: Backend /* cuda|metal|vulkan|none */, name, memory_total, memory_free, driver: Option<String>, compute: Option<String>, unified_memory: bool }>,
    pub backends_built: Vec<Backend>,          // what this harnessd was compiled with (llama features)
    pub probed_at: Timestamp, pub probe_notes: Vec<String>,
}
```

Probes, in order, first success per field:

- CPU / RAM: `sysinfo` (already a dependency; add the `cpu` feature) and
  `std::arch::is_x86_feature_detected!` / `is_aarch64_feature_detected!`.
- GPUs: `apprentice_llama::devices()` when the `llama` feature is on —
  the ggml backend enumeration is the truth about what the *service* can
  use, including free memory. Without it (or as a cross-check):
  `nvidia-smi` (the M00-04 probe, kept), on macOS `system_profiler
  SPDisplaysDataType -json` with `unified_memory = true` and memory =
  RAM, on Linux Vulkan via `vulkaninfo --summary` if present. Every
  probe has a 5 s timeout and adds a note on failure; a GPU found by
  `nvidia-smi` but absent from `devices()` is reported with `backend:
  none` and a note ("driver present, harnessd built without cuda").
- The profile is cached in `AppState` for 60 s; `hardware.profile
  {refresh: bool}` forces a re-probe.

### Planner

```rust
pub struct PlanRequest { pub model: ModelMeta /* from apprentice_llama or the manifest (M02-03) */, pub want_slots: u32, pub want_ctx: u32, pub want_resident: u32 /* M02-04b states */, pub kv: KvPreference /* auto|f16|q8_0|q4_0 */, pub gpu_layers: GpuLayers /* auto|all|n|none */, pub headroom: f32 /* fraction of VRAM kept free, default 0.10 */ }
pub struct Plan {
    pub device: Option<u32>, pub backend: Backend,
    pub n_gpu_layers: u32, pub type_k: KvType, pub type_v: KvType, pub flash_attn: bool,
    pub n_ctx: u32 /* total cells = slots × per_slot_ctx */, pub per_slot_ctx: u32, pub slots: u32, pub resident: u32,
    pub n_batch: u32, pub n_ubatch: u32, pub threads: u32,
    pub memory: MemoryTable { weights_bytes, weights_on_gpu_bytes, kv_bytes_per_token, kv_bytes_total, compute_buffer_estimate, total_gpu_bytes, total_ram_bytes, gpu_free_bytes, fits_gpu: bool, fits_ram: bool },
    pub notes: Vec<String>,                     // every downgrade explained ("ctx reduced 32768 → 16384 to fit 4 slots in 12 GB")
}
pub fn plan(profile: &HardwareProfile, req: &PlanRequest) -> Plan;
```

Algorithm (deterministic, unit-testable):

1. Pick the device: the GPU with the most free memory among
   `backends_built`; none → CPU plan (`n_gpu_layers = 0`, threads =
   physical cores, KV in RAM).
2. Weights: `size_bytes` from the model; with `gpu_layers = auto`, all
   layers on the GPU if they fit with headroom, else the largest count
   that fits (per-layer bytes ≈ size / n_layer; the rest stays in RAM).
3. KV per token from `n_layer`, `n_head_kv`, `head_dim` and the KV
   types (`kv = auto` → `f16`; `q8_0` when that is what makes the wanted
   slots × ctx fit, with a note); recurrent models have a fixed
   per-sequence state size instead (`ModelMeta.state_bytes`).
4. Cells: total = (slots + resident) × per_slot_ctx. Reduce in this
   order until the table fits: per_slot_ctx by halving (floor 4096),
   then slots (floor 1), then KV to `q8_0`, then GPU layers. Each step
   is a note.
5. Compute buffer: a documented estimate (n_ubatch × n_embd × 4 bytes ×
   a factor) so the total is honest rather than optimistic; the bench
   (M02-07) reports the measured number next to it.

The planner never *executes* anything; the service applies the plan and
records the actual `memory_used()` so the two can be compared.

### Config (`[inference]`, defined here, consumed by M02-04)

```toml
[inference]
gpu_layers = "auto"        # auto | all | none | <n>
kv_cache_type = "auto"     # auto | f16 | q8_0 | q4_0
slots = 4                  # transient slots wanted
context = 16384            # per slot wanted
resident_states = 2        # M02-04b
headroom = 0.10
threads = 0                # 0 = physical cores
device = "auto"            # auto | <gpu index> | cpu
```

### Surfaces

- RPC `hardware.profile {refresh?} → HardwareProfile`; `hardware.plan
  {model, slots?, context?} → Plan` (model = a manifest id from M02-03
  or a GGUF path) — used by the GUI (M02-08) and `harness models plan`.
- `harness doctor`: the hardware section prints the profile (backend,
  VRAM free/total per GPU, CPU features, RAM) and, when a default model
  is assigned, its plan in one table. `--json` includes both.

## Acceptance

- [ ] Recorded profiles (`tests/fixtures/hardware/*.json`: the reference
      machine, a CPU-only laptop, an M2 Mac with unified memory, a
      24 GB Linux box) drive planner unit tests whose expected plans are
      insta snapshots; the reference machine gets 4 slots × 16k for a
      3B Q4_K_M at f16 KV and the notes explain the 7B case (fewer
      slots or q8_0 KV).
- [ ] The memory table for the fixture model of M02-01 matches the
      service's measured `memory_used()` within 15 % after loading with
      the plan (integration test, `#[ignore]` without a GPU).
- [ ] `harness doctor` on the reference machine shows the RTX 3080 Ti
      with `backend: cuda` and 12 GB when built with `cuda`, and
      `backend: none` + the note when built without.
- [ ] A profile with no GPU yields a CPU plan; a model larger than RAM
      yields `fits_ram: false` and a plan with a note rather than a
      panic.
- [ ] `hardware.profile` answers in < 100 ms from the cache and re-probes
      with `refresh`.

## Verification

`cargo test -p apprentice-core hardware::`; `harness doctor` and `harness
doctor --json` by hand on the reference machine and on the CI runners
(their output is attached to the run as an artefact).

## Notes

- Free VRAM is a moving target (the GUI's WebView, other apps); the
  headroom fraction is the safety margin, and the service must handle a
  load failure gracefully (M02-04) rather than trusting the plan.
- Keep `HardwareProfile` in `apprentice-api` (wire type) so the GUI and
  CLI show it without linking core.
