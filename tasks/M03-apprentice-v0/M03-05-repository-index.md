# M03-05 — Repository index

Status: todo
Depends on: M01-02
Size: M

## Goal

A per-workspace symbol and chunk index built with tree-sitter: functions,
types, methods, imports and their line ranges per file, plus a stable
chunking of every file along symbol boundaries — persisted in SQLite
next to the workspace's data, refreshed incrementally from M01-02's
`FileIndex` changes and the harness's own file tools, and queried
through one API used by the context selector (M03-06) now and by the
trained ranker (M06) later. The index is data for the roles; it makes
no decisions.

## Context

SPEC §5.2 (selector input: "repository index (tree-sitter symbols, file
list, recent edits), candidate chunks"), §16 (Rust core). M01-02 built
`workspace::index::FileIndex { build, entries, get, under, is_stale,
language_of }` over the ignore rules, with `workspace.index` RPC; the
file tools (`read_file`, `edit_file`, `write_file`) know which paths a
session touched (`SeenFiles`).

## Scope

In: the grammar set, the extractor per language, the chunker, the
SQLite schema, incremental refresh, the query API and RPC/CLI, size and
speed budgets, tests on fixture repos.
Out: semantic embeddings (M06's ranker may add a vector column), call
graphs beyond textual "callers" search, indexing outside the workspace,
the selector's ranking (M03-06).

## Design

### Grammars

`tree-sitter` 0.25 with the vendored grammar crates for **Rust, Python,
TypeScript/TSX, JavaScript, Go, C, C++, Java, C#**, chosen by
`language_of`; each grammar behind a cargo feature in a
`workspace::repo_index::lang` module (`lang-rust`, … all on by default;
a build without one treats the language as plain text). Every other
language (and Markdown/TOML/YAML) gets the plain-text chunker. Grammar
queries (`queries/<lang>.scm`) capture `@definition.function/method/
class/struct/enum/trait/interface/type/const/module` and `@import`, with
the name node and, where the grammar has it, the signature range.

### Schema (`<data_dir>/workspaces/<workspace_id>/index.sqlite`)

```sql
files   (path PK, lang, size, mtime_ns, hash, indexed_at, symbols INT, chunks INT)
symbols (id PK, path, kind, name, qualified /* mod::Type::method */, start_line, end_line, signature, parent_id)
imports (path, target /* module or file string */, line)
chunks  (id PK /* sha256(path, start, end, hash)[..16] */, path, start_line, end_line, kind /* symbol|window|leading */, symbol_id, bytes, tokens_est)
edits   (path, at, session_id, tool)           -- recent edits by the harness's own tools, ring of 500
meta    (key, value)                            -- schema version, grammar versions, last full build
```

Indexes on `symbols(name)`, `symbols(path)`, `chunks(path)`; FTS5 table
`chunks_fts(text)` over the chunk text for the lexical retrieval M03-06
uses (content stored in the FTS table only; the source file is the
truth). Chunk text is not duplicated elsewhere.

### Chunking

One chunk per top-level symbol; a symbol longer than `index.chunk_max_
lines` (120) is split at its nested symbols, else into windows of
`chunk_max_lines` with `chunk_overlap` (8) lines; file-leading text
(imports, module docs) is a `leading` chunk; plain-text files are
windows. Ids are content-derived so an unchanged region keeps its id
across refreshes (the selector's trace records stay comparable, M04's
ablations too).

### Refresh

- Full build on first open of a workspace (async, `Background` thread
  pool, progress event `index.progress {done, total}`); a workspace
  over `index.max_files` (20 000) indexes the largest-language subset
  first and reports `partial: true`.
- Incremental: `FileIndex` rebuilds (M01-02's staleness) diff `hash`
  per path → re-extract changed files, delete removed; the file tools
  call `repo_index.touch(path)` after `edit_file`/`write_file` so the
  session's own edits are indexed before the next step (bounded to
  `index.touch_max_ms`, 50 ms per file, else queued).
- `edits` records every harness write (path, session, tool) — the
  selector's "recent edits" signal.

### Query API (`RepoIndex`, in `Workspace`)

```rust
pub fn symbols(&self, q: SymbolQuery { name: Option<&str>, prefix: Option<&str>, kind: Option<Kind>, path: Option<&str>, limit }) -> Vec<Symbol>;
pub fn outline(&self, path: &str) -> Vec<Symbol>;                          // the file's tree, nested
pub fn chunk(&self, id: &ChunkId) -> Option<Chunk>;                        // with text read from the file (hash-checked; stale → None + re-index queued)
pub fn chunks_for(&self, paths: &[&str]) -> Vec<Chunk>;
pub fn search(&self, terms: &[&str], limit) -> Vec<(ChunkId, f32 /* bm25 */)>;   // FTS5 over chunk text
pub fn callers(&self, name: &str, limit) -> Vec<(ChunkId, line)>;         // textual: identifier occurrences outside its definition
pub fn recent_edits(&self, limit) -> Vec<Edit>;
pub fn status(&self) -> IndexStatus { files, symbols, chunks, partial, building: bool, last_full, grammar_versions };
```

### Surfaces

- RPC `index.status {workspace_id}`, `index.refresh {full?}`,
  `index.symbols {q}`, `index.outline {path}`, `index.search {terms}`,
  `index.chunk {id}`; events `index.progress`.
- CLI `harness index status|refresh [--full]|symbols <q>|outline
  <path>|search <terms>|chunk <id>`.
- Mentor tools: none in M03 (the mentor keeps `grep`/`glob`; giving it
  `symbols` is an M10 candidate).

## Acceptance

- [ ] Fixture repos under `crates/core/tests/fixtures/repos/` (a small
      Rust crate, a Python package, a TS app, a Go module, a mixed tree
      with unsupported files): goldens for `outline` per language
      (kinds, names, ranges, signatures, nesting) and for the chunk
      list (ids stable across two builds).
- [ ] Incremental: editing one function shifts later chunks' ranges,
      keeps unchanged chunks' ids, re-indexes only that file (the
      `files.indexed_at` of others unchanged); deleting a file removes
      its rows; `touch` after `edit_file` makes the new symbol visible
      to `symbols()` before the next step in an end-to-end run.
- [ ] `search(["parse", "config"])` on the Rust fixture ranks the chunk
      holding `parse_config` first; `callers("parse_config")` finds the
      call sites and not the definition.
- [ ] Speed (reference machine, `#[ignore]` timing test): the harness
      repo itself (~60 k lines) full build < 5 s, incremental single-
      file refresh < 50 ms; memory of the index process < 200 MB during
      the build. `partial` handling on a synthetic 30 k-file tree.
- [ ] A build without `lang-go` treats `.go` as plain text (windows
      chunks, no symbols) and says so in `status`.
- [ ] `harness index status` and `symbols` work against a live daemon;
      the index file is under the workspace's data directory and is
      removed by `workspace remove`.

## Verification

`cargo test -p apprentice-core workspace::repo_index::` (fixtures,
goldens via `insta`); the timing test; `harness index` by hand.

## Notes

- `tree-sitter` grammar crates pull C sources; check `cargo deny`
  licences (MIT throughout) and build time on all three platforms in CI
  before adding more languages.
- Chunk ids are the currency of the selector's trace records and of
  M04's ablations; changing the chunker changes ids everywhere — treat
  it like a protocol change (CHANGELOG line, `meta.schema` bump, full
  rebuild).
