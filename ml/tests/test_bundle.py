"""The bundle reader consumes what `harness trace export` writes."""

import hashlib
import io
import json
import tarfile
from pathlib import Path

import pytest
import zstandard

from apprentice_ml.cli import main
from apprentice_ml.traces import bundle, stats_of

BODY = b'{"model":"claude-opus-5","messages":[]}'
BODY_ID = hashlib.sha256(BODY).hexdigest()
TEXT = b"hello world"
TEXT_ID = hashlib.sha256(TEXT).hexdigest()


def jsonl(rows: list[dict]) -> str:
    return "".join(json.dumps(r) + "\n" for r in rows)


def make_bundle(root: Path) -> Path:
    """A one-session bundle with two blobs, laid out like the exporter's."""
    root.mkdir()
    counts = {
        "sessions": 1,
        "workspaces": 1,
        "agents": 1,
        "steps": 1,
        "events": 4,
        "mentor_calls": 1,
        "messages": 2,
        "blobs": 2,
        "blob_bytes": len(BODY) + len(TEXT),
    }
    manifest = {
        "format_version": 1,
        "created_at": "2026-09-13T10:00:00.000Z",
        "harness_version": "0.1.0",
        "schema_version": 3,
        "selection": {"session_ids": ["s1"]},
        "sessions": [
            {
                "id": "s1",
                "title": "fix tests",
                "workspace_id": "w1",
                "created_at": "t0",
                "updated_at": "t1",
                "status": "open",
                "agents": 1,
                "events": 4,
                "mentor_calls": 1,
                "messages": 2,
            }
        ],
        "counts": counts,
        "redaction": {
            "applied": False,
            "rules": [],
            "replacements": 0,
            "secrets": 0,
            "paths": False,
            "touched_requests": 0,
            "replayable": True,
        },
    }
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    (root / "workspaces.jsonl").write_text(
        jsonl(
            [
                {
                    "id": "w1",
                    "root": "C:/src/demo",
                    "name": "demo",
                    "created_at": "t0",
                    "last_used_at": "t1",
                    "settings": {},
                }
            ]
        ),
        encoding="utf-8",
    )
    (root / "sessions.jsonl").write_text(
        jsonl(
            [
                {
                    "id": "s1",
                    "created_at": "t0",
                    "updated_at": "t1",
                    "title": "fix tests",
                    "workspace_id": "w1",
                    "config": {"mentor": {"model": "claude-opus-5"}},
                    "status": "open",
                    "message_count": 2,
                }
            ]
        ),
        encoding="utf-8",
    )
    (root / "agents.jsonl").write_text(
        jsonl(
            [
                {
                    "id": "a1",
                    "session_id": "s1",
                    "kind": "main",
                    "created_at": "t0",
                    "status": "ok",
                    "task_text": "add hello",
                    "options": {},
                }
            ]
        ),
        encoding="utf-8",
    )
    (root / "steps.jsonl").write_text(
        jsonl([{"id": "st1", "agent_id": "a1", "seq": 1, "started_at": "t0", "status": "ok"}]),
        encoding="utf-8",
    )
    events = [
        {"id": "e1", "session_id": "s1", "seq": 1, "ts": "t0", "kind": "session.created"},
        {
            "id": "e2",
            "session_id": "s1",
            "agent_id": "a1",
            "seq": 2,
            "ts": "t0",
            "kind": "user.message",
            "payload": {"text_len": 11},
            "blob_id": TEXT_ID,
        },
        {
            "id": "e3",
            "session_id": "s1",
            "agent_id": "a1",
            "step_id": "st1",
            "seq": 3,
            "ts": "t0",
            "kind": "mentor.request",
            "payload": {"call_id": "m1", "request_hash": BODY_ID, "bytes": len(BODY)},
            "blob_id": BODY_ID,
        },
        {
            "id": "e4",
            "session_id": "s1",
            "agent_id": "a1",
            "seq": 4,
            "ts": "t1",
            "kind": "agent.finished",
            "payload": {"status": "ok"},
        },
    ]
    (root / "events.jsonl").write_text(jsonl(events), encoding="utf-8")
    (root / "mentor_calls.jsonl").write_text(
        jsonl(
            [
                {
                    "id": "m1",
                    "session_id": "s1",
                    "agent_id": "a1",
                    "step_id": "st1",
                    "request_event_id": "e3",
                    "model": "claude-opus-5",
                    "started_at": "t0",
                    "status": "ok",
                    "kind": "step",
                }
            ]
        ),
        encoding="utf-8",
    )
    (root / "session_messages.jsonl").write_text(
        jsonl(
            [
                {
                    "session_id": "s1",
                    "seq": 1,
                    "role": "user",
                    "content": [{"type": "text", "text": "add hello"}],
                    "created_at": "t0",
                },
                {
                    "session_id": "s1",
                    "seq": 2,
                    "role": "assistant",
                    "content": [{"type": "text", "text": "done"}],
                    "created_at": "t1",
                },
            ]
        ),
        encoding="utf-8",
    )
    (root / "blobs.jsonl").write_text(
        jsonl(
            [
                {
                    "id": BODY_ID,
                    "size": len(BODY),
                    "media_type": "application/json",
                    "created_at": "t0",
                },
                {"id": TEXT_ID, "size": len(TEXT), "media_type": "text/plain", "created_at": "t0"},
            ]
        ),
        encoding="utf-8",
    )
    for blob_id, data in [(BODY_ID, BODY), (TEXT_ID, TEXT)]:
        p = root / "blobs" / blob_id[:2] / blob_id
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    return root


def pack(root: Path, out: Path) -> Path:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w") as tar:
        for p in sorted(root.rglob("*")):
            if p.is_file():
                tar.add(p, arcname=p.relative_to(root).as_posix())
    out.write_bytes(zstandard.ZstdCompressor(level=6).compress(buf.getvalue()))
    return out


def test_directory_bundle_iterates_and_matches_the_manifest(tmp_path: Path) -> None:
    root = make_bundle(tmp_path / "bundle")
    assert bundle.is_bundle(root)
    with bundle.load_bundle(root) as b:
        assert b.manifest["format_version"] == 1
        assert b.counts() == b.manifest["counts"]
        assert [s["id"] for s in b.sessions()] == ["s1"]
        assert [w["root"] for w in b.workspaces()] == ["C:/src/demo"]
        assert [e["seq"] for e in b.events("s1")] == [1, 2, 3, 4]
        assert [e["kind"] for e in b.events()][2] == "mentor.request"
        assert [m["role"] for m in b.messages("s1")] == ["user", "assistant"]
        assert b.read_blob(TEXT_ID) == TEXT
        call = next(b.mentor_calls())
        assert b.request_body(call) == BODY
        assert json.loads(b.request_body(call))["model"] == "claude-opus-5"
        s = bundle.stats(b)
        assert (s.sessions, s.agents, s.events, s.mentor_calls, s.blobs) == (1, 1, 4, 1, 2)
        assert s.blob_bytes == len(BODY) + len(TEXT)
        assert s.schema_version == 3
        assert s.events_by_kind == {
            "agent.finished": 1,
            "mentor.request": 1,
            "session.created": 1,
            "user.message": 1,
        }
        b.blob_path(TEXT_ID).write_bytes(b"tampered")
        with pytest.raises(ValueError, match="corrupted"):
            b.read_blob(TEXT_ID)
        assert b.read_blob(TEXT_ID, verify=False) == b"tampered"


def test_packed_bundle_unpacks_and_cleans_up(tmp_path: Path) -> None:
    root = make_bundle(tmp_path / "bundle")
    packed = pack(root, tmp_path / "bundle.tar.zst")
    assert bundle.is_bundle(packed)
    b = bundle.load_bundle(packed)
    unpacked = b.path
    assert unpacked != root and (unpacked / "manifest.json").is_file()
    assert b.counts() == b.manifest["counts"]
    assert b.read_blob(BODY_ID) == BODY
    b.close()
    assert not unpacked.exists()
    assert stats_of(packed).events == 4
    assert stats_of(root).events == 4


def test_not_a_bundle(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        bundle.load_bundle(tmp_path / "nope")
    empty = tmp_path / "empty"
    empty.mkdir()
    with pytest.raises(ValueError, match="no manifest.json"):
        bundle.load_bundle(empty)
    (tmp_path / "x.txt").write_text("x")
    with pytest.raises(ValueError, match="neither"):
        bundle.load_bundle(tmp_path / "x.txt")
    future = tmp_path / "future"
    future.mkdir()
    (future / "manifest.json").write_text('{"format_version": 2}')
    with pytest.raises(ValueError, match="newer"):
        bundle.load_bundle(future)


def test_cli_stats_takes_a_bundle_or_a_db(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    root = make_bundle(tmp_path / "bundle")
    assert main(["traces", "stats", str(root)]) == 0
    out = capsys.readouterr().out
    assert "schema v3" in out and "mentor.request" in out
    assert main(["traces", "stats", str(pack(root, tmp_path / "b.tar.zst")), "--json"]) == 0
    data = json.loads(capsys.readouterr().out)
    assert data["mentor_calls"] == 1
    assert main(["traces", "stats", str(tmp_path / "nope")]) == 2
    assert "error:" in capsys.readouterr().err
