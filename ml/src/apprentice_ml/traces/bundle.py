"""Reader for the trace bundles `harness trace export` writes (task M01-14).

A bundle is a directory (or a `.tar.zst` of one) with `manifest.json`,
one `.jsonl` file per table — `workspaces`, `sessions`, `agents`,
`steps`, `events`, `mentor_calls`, `session_messages`, `blobs` — and the
referenced blobs under `blobs/<aa>/<sha256>`. Rows are the table columns
as JSON objects, the JSON columns already parsed (`payload`, `content`,
`config`, `options`, `settings`).

Directory bundles need only the standard library; a packed bundle is
unpacked into a temporary directory with `zstandard`, which lives as
long as the `Bundle` (use it as a context manager or call `close()`).
"""

from __future__ import annotations

import hashlib
import json
import shutil
import tarfile
import tempfile
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from apprentice_ml.traces import TraceStats

MANIFEST_FILE = "manifest.json"
PACKED_SUFFIX = ".tar.zst"
FORMAT_VERSION = 1
ROW_FILES = {
    "workspaces": "workspaces.jsonl",
    "sessions": "sessions.jsonl",
    "agents": "agents.jsonl",
    "steps": "steps.jsonl",
    "events": "events.jsonl",
    "mentor_calls": "mentor_calls.jsonl",
    "messages": "session_messages.jsonl",
    "blobs": "blobs.jsonl",
}


def is_bundle(path: str | Path) -> bool:
    """A directory with a manifest, or a `.tar.zst`."""
    p = Path(path)
    if p.is_dir():
        return (p / MANIFEST_FILE).is_file()
    return p.is_file() and p.name.lower().endswith(PACKED_SUFFIX)


@dataclass
class Bundle:
    """An opened bundle: its manifest and lazy access to rows and blobs."""

    path: Path
    manifest: dict[str, Any]
    _tmp: Path | None = field(default=None, repr=False)

    def __enter__(self) -> Bundle:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        """Removes the temporary unpack of a packed bundle, if any."""
        if self._tmp is not None:
            shutil.rmtree(self._tmp, ignore_errors=True)
            self._tmp = None

    # ------------------------------------------------------------- rows

    def rows(self, table: str) -> Iterator[dict[str, Any]]:
        """The rows of one `.jsonl` file, in file order (a missing file is empty)."""
        file = self.path / ROW_FILES[table]
        if not file.is_file():
            return
        with file.open(encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    yield json.loads(line)

    def workspaces(self) -> Iterator[dict[str, Any]]:
        return self.rows("workspaces")

    def sessions(self) -> Iterator[dict[str, Any]]:
        return self.rows("sessions")

    def agents(self) -> Iterator[dict[str, Any]]:
        return self.rows("agents")

    def steps(self) -> Iterator[dict[str, Any]]:
        return self.rows("steps")

    def events(self, session_id: str | None = None) -> Iterator[dict[str, Any]]:
        """Events in `(session, seq)` order, optionally of one session."""
        for row in self.rows("events"):
            if session_id is None or row["session_id"] == session_id:
                yield row

    def mentor_calls(self) -> Iterator[dict[str, Any]]:
        return self.rows("mentor_calls")

    def messages(self, session_id: str | None = None) -> Iterator[dict[str, Any]]:
        """`session_messages` rows in `(session, seq)` order."""
        for row in self.rows("messages"):
            if session_id is None or row["session_id"] == session_id:
                yield row

    def blobs(self) -> Iterator[dict[str, Any]]:
        """Blob metadata rows (`id`, `size`, `media_type`, `pruned`)."""
        return self.rows("blobs")

    # ------------------------------------------------------------ blobs

    def blob_path(self, blob_id: str) -> Path:
        return self.path / "blobs" / blob_id[:2] / blob_id

    def read_blob(self, blob_id: str, verify: bool = True) -> bytes:
        """The blob's bytes; `ValueError` when they do not hash to its id."""
        data = self.blob_path(blob_id).read_bytes()
        if verify:
            actual = hashlib.sha256(data).hexdigest()
            if actual != blob_id:
                raise ValueError(f"blob {blob_id} is corrupted (content hashes to {actual})")
        return data

    def request_body(self, call: dict[str, Any]) -> bytes:
        """The `mentor.request` body of a `mentor_calls` row, as sent."""
        event = next(
            (e for e in self.events(call["session_id"]) if e["id"] == call["request_event_id"]),
            None,
        )
        if event is None or not event.get("blob_id"):
            raise KeyError(f"call {call['id']} has no request body in the bundle")
        return self.read_blob(event["blob_id"])

    # ----------------------------------------------------------- counts

    def counts(self) -> dict[str, int]:
        """Row counts from the files, in the manifest's `counts` shape."""
        out = {table: sum(1 for _ in self.rows(table)) for table in ROW_FILES}
        out["blob_bytes"] = sum(int(b["size"]) for b in self.blobs() if not b.get("pruned"))
        return out


def load_bundle(path: str | Path) -> Bundle:
    """Opens a bundle directory or `.tar.zst`.

    Raises `FileNotFoundError` for a missing path, `ValueError` for
    something that is not a bundle or a format this reader does not know,
    and `ImportError` for a packed bundle when `zstandard` is missing.
    """
    p = Path(path)
    if p.is_dir():
        return _open_dir(p, tmp=None)
    if not p.is_file():
        raise FileNotFoundError(p)
    if not p.name.lower().endswith(PACKED_SUFFIX):
        raise ValueError(f"{p} is neither a bundle directory nor a {PACKED_SUFFIX}")
    tmp = Path(tempfile.mkdtemp(prefix="harness-bundle-"))
    try:
        _unpack(p, tmp)
        return _open_dir(tmp, tmp=tmp)
    except BaseException:
        shutil.rmtree(tmp, ignore_errors=True)
        raise


def _open_dir(path: Path, tmp: Path | None) -> Bundle:
    manifest_file = path / MANIFEST_FILE
    if not manifest_file.is_file():
        raise ValueError(f"{path} is not a bundle: no {MANIFEST_FILE}")
    manifest = json.loads(manifest_file.read_text(encoding="utf-8"))
    version = int(manifest.get("format_version", 0))
    if version > FORMAT_VERSION:
        raise ValueError(
            f"bundle format v{version} is newer than this reader supports (v{FORMAT_VERSION})"
        )
    return Bundle(path=path, manifest=manifest, _tmp=tmp)


def _unpack(archive: Path, into: Path) -> None:
    try:
        import zstandard
    except ImportError as e:  # pragma: no cover - depends on the environment
        raise ImportError(
            "reading a .tar.zst bundle needs the `zstandard` package "
            "(or unpack it first and pass the directory)"
        ) from e
    with archive.open("rb") as f:
        with zstandard.ZstdDecompressor().stream_reader(f) as reader:
            with tarfile.open(fileobj=reader, mode="r|") as tar:
                tar.extractall(into, filter="data")


def stats(bundle: Bundle) -> TraceStats:
    """The same counts `traces stats` prints for a database."""
    by_kind: dict[str, int] = {}
    for e in bundle.events():
        by_kind[e["kind"]] = by_kind.get(e["kind"], 0) + 1
    counts = bundle.counts()
    return TraceStats(
        schema_version=int(bundle.manifest.get("schema_version", 0)),
        sessions=counts["sessions"],
        agents=counts["agents"],
        events=counts["events"],
        blobs=counts["blobs"],
        blob_bytes=counts["blob_bytes"],
        mentor_calls=counts["mentor_calls"],
        events_by_kind=dict(sorted(by_kind.items())),
    )
