"""Read-only access to the harness trace store (`traces.sqlite`) and to
the bundles `harness trace export` writes (`apprentice_ml.traces.bundle`).

Only the standard library is used for the database and for directory
bundles, so the formats stay consumable from any Python environment
(`zstandard` is needed to open a packed `.tar.zst`). Blobs live next to
the database under `blobs/<aa>/<id>`.
"""

from __future__ import annotations

import sqlite3
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# The newest trace schema this reader understands; the queries below only
# touch v1 columns, so every version up to here reads the same.
SUPPORTED_SCHEMA = 3


@dataclass
class TraceStats:
    schema_version: int
    sessions: int
    agents: int
    events: int
    blobs: int
    blob_bytes: int
    mentor_calls: int
    events_by_kind: dict[str, int] = field(default_factory=dict)


def connect(db: str | Path) -> sqlite3.Connection:
    """Opens the database read-only (the daemon may be writing)."""
    path = Path(db)
    if not path.is_file():
        raise FileNotFoundError(path)
    conn = sqlite3.connect(f"{path.as_uri()}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def schema_version(conn: sqlite3.Connection) -> int:
    row = conn.execute("SELECT value FROM schema_meta WHERE key = 'version'").fetchone()
    if row is None:
        raise ValueError("not a trace database: schema_meta.version missing")
    return int(row["value"])


def stats(db: str | Path) -> TraceStats:
    with connect(db) as conn:
        version = schema_version(conn)
        if version > SUPPORTED_SCHEMA:
            raise ValueError(
                f"schema v{version} is newer than this reader supports (v{SUPPORTED_SCHEMA})"
            )

        def count(table: str) -> int:
            return int(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])

        by_kind = {
            row["kind"]: int(row["n"])
            for row in conn.execute(
                "SELECT kind, COUNT(*) AS n FROM events GROUP BY kind ORDER BY kind"
            )
        }
        blob_bytes = conn.execute("SELECT COALESCE(SUM(size), 0) FROM blobs").fetchone()[0]
        return TraceStats(
            schema_version=version,
            sessions=count("sessions"),
            agents=count("agents"),
            events=count("events"),
            blobs=count("blobs"),
            blob_bytes=int(blob_bytes),
            mentor_calls=count("mentor_calls"),
            events_by_kind=by_kind,
        )


def blob_path(db: str | Path, blob_id: str) -> Path:
    """`<data_dir>/blobs/<aa>/<id>` for a database at `<data_dir>/traces.sqlite`."""
    return Path(db).parent / "blobs" / blob_id[:2] / blob_id


def iter_events(db: str | Path, session_id: str | None = None) -> Iterator[dict[str, Any]]:
    """Events in `(session_id, seq)` order with parsed payloads."""
    import json

    with connect(db) as conn:
        sql = (
            "SELECT id, session_id, agent_id, step_id, seq, ts, kind, payload_json, blob_id "
            "FROM events"
        )
        params: tuple[Any, ...] = ()
        if session_id is not None:
            sql += " WHERE session_id = ?"
            params = (session_id,)
        sql += " ORDER BY session_id, seq"
        for row in conn.execute(sql, params):
            item = dict(row)
            item["payload"] = json.loads(item.pop("payload_json"))
            yield item


def stats_of(path: str | Path) -> TraceStats:
    """Stats of a database or of a bundle (directory or `.tar.zst`)."""
    from apprentice_ml.traces import bundle

    if bundle.is_bundle(path):
        with bundle.load_bundle(path) as b:
            return bundle.stats(b)
    return stats(path)


def format_stats(s: TraceStats) -> str:
    lines = [
        f"schema v{s.schema_version}",
        f"sessions {s.sessions} · agents {s.agents} · events {s.events} · "
        f"mentor calls {s.mentor_calls} · blobs {s.blobs} ({s.blob_bytes} bytes)",
    ]
    if s.events_by_kind:
        width = max(len(k) for k in s.events_by_kind)
        lines.append("events by kind:")
        lines.extend(f"  {k:<{width}}  {n}" for k, n in s.events_by_kind.items())
    return "\n".join(lines)
