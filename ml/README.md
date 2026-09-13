# apprentice-ml

Python workspace for apprentice-harness: dataset builders, trainers,
evaluators and compute backends (from M05/M06). M00 ships the package
skeleton and a trace reader; M01-14 adds the reader for trace bundles.

```
uv sync                      # create .venv with dev tools
uv run apprentice-ml --version
uv run apprentice-ml traces stats ~/.local/share/apprentice-harness/traces.sqlite
uv run apprentice-ml traces stats week.tar.zst      # a bundle from `harness trace export`
uv run pytest
uv run ruff check .
```

## Trace bundles

`harness trace export` writes a directory or a `.tar.zst` with
`manifest.json`, one `.jsonl` per table and the blobs under
`blobs/<aa>/<sha256>`. `apprentice_ml.traces.bundle` reads them:

```python
from apprentice_ml.traces.bundle import load_bundle

with load_bundle("week.tar.zst") as b:  # a directory needs no `with`
    print(b.manifest["counts"], b.manifest.get("redaction"))
    for call in b.mentor_calls():
        body = b.request_body(call)  # the exact bytes the mentor saw
    for m in b.messages(session_id="..."):  # the stored conversation
        ...
    b.read_blob(blob_id)  # verified against its id
```

A directory bundle needs only the standard library; a packed one is
unpacked with `zstandard` into a temporary directory that lives as long
as the `Bundle`. A redacted bundle (`manifest.redaction.applied`) carries
bodies with `<REDACTED:kind:n>` tokens; `replayable` says whether any
request body was touched.
