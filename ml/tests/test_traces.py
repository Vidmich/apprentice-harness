"""The Python reader consumes a database created from the Rust schema."""

import json
import sqlite3
from pathlib import Path

import pytest

from apprentice_ml import traces
from apprentice_ml.cli import main

REPO = Path(__file__).resolve().parents[2]
SCHEMA_V1 = REPO / "crates" / "core" / "src" / "trace" / "migrations" / "v001.sql"
BLOB_ID = "ab" + "c" * 62


def make_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(SCHEMA_V1.read_text())
    conn.execute("INSERT INTO schema_meta VALUES ('version', '1')")
    conn.execute(
        "INSERT INTO sessions(id, created_at, updated_at, config_json) "
        "VALUES ('s1', 't', 't', '{}')"
    )
    conn.execute(
        "INSERT INTO blobs(id, size, media_type, created_at) VALUES (?, 5, 'text/plain', 't')",
        (BLOB_ID,),
    )
    rows = [
        ("e1", "s1", 1, "session.created", json.dumps({"title": None}), None),
        ("e2", "s1", 2, "user.message", json.dumps({"text_len": 5}), BLOB_ID),
        ("e3", "s1", 3, "user.message", json.dumps({"text_len": 7}), None),
    ]
    conn.executemany(
        "INSERT INTO events(id, session_id, seq, ts, kind, payload_json, blob_id) "
        "VALUES (?, ?, ?, 't', ?, ?, ?)",
        rows,
    )
    conn.commit()
    conn.close()


def test_stats_counts_per_kind(tmp_path: Path) -> None:
    db = tmp_path / "traces.sqlite"
    make_db(db)
    s = traces.stats(db)
    assert s.schema_version == 1
    assert s.sessions == 1
    assert s.events == 3
    assert s.blobs == 1
    assert s.blob_bytes == 5
    assert s.events_by_kind == {"session.created": 1, "user.message": 2}
    events = list(traces.iter_events(db, "s1"))
    assert [e["seq"] for e in events] == [1, 2, 3]
    assert events[1]["payload"] == {"text_len": 5}
    assert traces.blob_path(db, "abcdef") == tmp_path / "blobs" / "ab" / "abcdef"


def test_cli_prints_counts(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    db = tmp_path / "traces.sqlite"
    make_db(db)
    assert main(["traces", "stats", "--db", str(db)]) == 0
    out = capsys.readouterr().out
    assert "user.message" in out and "2" in out
    assert main(["traces", "stats", "--db", str(db), "--json"]) == 0
    data = json.loads(capsys.readouterr().out)
    assert data["events_by_kind"]["user.message"] == 2


def test_cli_rejects_missing_or_newer_db(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert main(["traces", "stats", "--db", str(tmp_path / "nope.sqlite")]) == 2
    db = tmp_path / "traces.sqlite"
    make_db(db)
    conn = sqlite3.connect(db)
    conn.execute("UPDATE schema_meta SET value = '99' WHERE key = 'version'")
    conn.commit()
    conn.close()
    assert main(["traces", "stats", "--db", str(db)]) == 2
    assert "newer" in capsys.readouterr().err
