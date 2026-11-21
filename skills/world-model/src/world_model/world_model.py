"""SQLite-backed World Model for long-horizon Prime Agent runs.

The store decouples parallel worker sessions from shared, structured context:
workers save raw artifacts to disk and commit provenance-backed updates
(hypotheses, immutable evidence, claims, claim relations, and tasks) to one
SQLite database that parent and child processes open concurrently.

Scope resolution (see `default_db_path`):

* explicit ``path`` argument wins;
* a pinned workspace resolves to ``<agent_dir>/workspaces/<name>/world_model.db``
  (cross-session persistence);
* otherwise the store is session-local at ``<session_artifact_dir>/world_model.db``.

The provenance invariant is hard: a claim cannot be written or promoted to
``status="supported"`` unless at least one already-existing evidence row is
attached to it.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterator

_HYPOTHESIS_STATUSES = ("open", "supported", "refuted", "parked")
_CLAIM_KINDS = ("finding", "decision", "dead_end")
_CLAIM_STATUSES = ("open", "supported", "refuted")
_EVIDENCE_KINDS = ("code_run", "file_slice", "test_run", "paper", "web", "dataset")
_EDGE_RELS = ("supports", "refutes", "supersedes", "derived_from")
_TASK_STATUSES = ("pending", "in_progress", "completed", "cancelled")

# Mirrors pa-types workspace.rs: 1-64 chars of lowercase letters, digits,
# '-' or '_', starting with a letter or digit, and not a reserved word.
_WORKSPACE_NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")
_RESERVED_WORKSPACE_NAMES = frozenset(
    {
        "off", "clear", "global", "none", "con", "prn", "aux", "nul",
        "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
        "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    }
)

_SCHEMA = """-- World Model store (SQLite, WAL). One schema, shared by all processes.

CREATE TABLE IF NOT EXISTS hypotheses (
  id INTEGER PRIMARY KEY,
  statement TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'supported', 'refuted', 'parked')),
  priority INTEGER NOT NULL DEFAULT 1,
  cycle INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

-- Evidence is immutable: it records what was actually observed, so there is
-- deliberately no update or delete path for it.
CREATE TABLE IF NOT EXISTS evidence (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('code_run', 'file_slice', 'test_run', 'paper', 'web', 'dataset')),
  ref TEXT NOT NULL,
  excerpt TEXT NOT NULL DEFAULT '',
  artifact_path TEXT,
  author TEXT,
  cycle INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS claims (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('finding', 'decision', 'dead_end')),
  statement TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'supported', 'refuted')),
  confidence REAL,
  cycle INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS claim_evidence (
  claim_id INTEGER NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
  evidence_id INTEGER NOT NULL REFERENCES evidence(id) ON DELETE CASCADE,
  PRIMARY KEY (claim_id, evidence_id)
);

-- Edges connect two claims. Evidence links to claims through claim_evidence;
-- claim-to-claim support, refutation, and provenance flow through edges.
CREATE TABLE IF NOT EXISTS edges (
  id INTEGER PRIMARY KEY,
  src_id INTEGER NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
  dst_id INTEGER NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
  rel TEXT NOT NULL CHECK (rel IN ('supports', 'refutes', 'supersedes', 'derived_from')),
  note TEXT,
  cycle INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
  id INTEGER PRIMARY KEY,
  cycle INTEGER NOT NULL DEFAULT 0,
  role TEXT NOT NULL,
  prompt TEXT NOT NULL,
  blocked_by_json TEXT NOT NULL DEFAULT '[]',
  assignee TEXT,
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'in_progress', 'completed', 'cancelled')),
  artifacts_json TEXT NOT NULL DEFAULT '[]',
  result TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS claims_fts USING fts5(
  statement, content='claims', content_rowid='id'
);
CREATE TRIGGER IF NOT EXISTS claims_ai AFTER INSERT ON claims BEGIN
  INSERT INTO claims_fts(rowid, statement) VALUES (new.id, new.statement);
END;
CREATE TRIGGER IF NOT EXISTS claims_ad AFTER DELETE ON claims BEGIN
  INSERT INTO claims_fts(claims_fts, rowid, statement) VALUES ('delete', old.id, old.statement);
END;
CREATE TRIGGER IF NOT EXISTS claims_au AFTER UPDATE ON claims BEGIN
  INSERT INTO claims_fts(claims_fts, rowid, statement) VALUES ('delete', old.id, old.statement);
  INSERT INTO claims_fts(rowid, statement) VALUES (new.id, new.statement);
END;

CREATE VIRTUAL TABLE IF NOT EXISTS evidence_fts USING fts5(
  ref, excerpt, content='evidence', content_rowid='id'
);
CREATE TRIGGER IF NOT EXISTS evidence_ai AFTER INSERT ON evidence BEGIN
  INSERT INTO evidence_fts(rowid, ref, excerpt) VALUES (new.id, new.ref, new.excerpt);
END;
CREATE TRIGGER IF NOT EXISTS evidence_ad AFTER DELETE ON evidence BEGIN
  INSERT INTO evidence_fts(evidence_fts, rowid, ref, excerpt) VALUES ('delete', old.id, old.ref, old.excerpt);
END;"""


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _agent_dir() -> Path:
    """Resolve the Prime Agent config dir the same way the runtime does."""
    raw = (
        os.environ.get("PRIME_AGENT_CODING_AGENT_DIR")
        or os.environ.get("PI_CODING_AGENT_DIR")
        or str(Path.home() / ".prime" / "agent")
    )
    return Path(raw).expanduser()


def _env_dir(name: str) -> Path | None:
    # Set-but-empty env values must behave as unset, like the runtime harness.
    value = (os.environ.get(name) or "").strip()
    return Path(value).expanduser() if value else None


def _workspace_name(raw: str) -> str | None:
    """A valid workspace name, or None when raw is invalid or reserved."""
    normalized = raw.strip().lower()
    if _WORKSPACE_NAME_RE.fullmatch(normalized) and normalized not in _RESERVED_WORKSPACE_NAMES:
        return normalized
    return None


def _workspace_from_record(session_dir: Path) -> str | None:
    """The session's own workspace record.

    Returns None when there is no record (fall through to other sources) and
    the empty string when the record is present but empty, meaning the session
    explicitly runs in global state.
    """
    record = session_dir / "workspace"
    try:
        raw = record.read_text()
    except OSError:
        return None
    if not raw.strip():
        return ""
    return _workspace_name(raw)


def _workspace_from_env() -> str | None:
    """The workspace pinned for this kernel, from the harness-state dir the
    host injects (subagents inherit it)."""
    root = _env_dir("RLM_WORKSPACE_HARNESS_STATE_DIR")
    if root is None:
        return None
    # <agent_dir>/workspaces/<name>/harness — the workspace dir is the parent.
    name = root.parent.name
    if root.parent.parent.name != "workspaces":
        return None
    return _workspace_name(name)


def resolve_workspace(
    agent_dir: Path | str | None = None, session_dir: Path | str | None = None
) -> str | None:
    """The workspace this store belongs to, or None for session-local scope.

    Resolution mirrors pa-types `resolve_session_workspace`: the session's own
    record wins (an empty record means explicitly global), then the kernel's
    resolved workspace env, then the agent-dir active-workspace marker.
    """
    agent = _agent_dir() if agent_dir is None else Path(agent_dir).expanduser()
    session = _env_dir("RLM_SESSION_DIR") if session_dir is None else Path(session_dir)
    if session is not None:
        record = _workspace_from_record(session)
        if record is not None:
            return record or None
    return _workspace_from_env() or _workspace_from_marker(agent)


def _workspace_from_marker(agent_dir: Path) -> str | None:
    try:
        raw = (agent_dir / "active_workspace").read_text()
    except OSError:
        return None
    return _workspace_name(raw)


def default_db_path(
    agent_dir: Path | str | None = None, session_dir: Path | str | None = None
) -> Path:
    """Default store location: pinned workspace, else session artifact dir.

    Raises RuntimeError when no workspace is pinned and no session artifact
    dir is available; pass an explicit ``path`` in that case.
    """
    agent = _agent_dir() if agent_dir is None else Path(agent_dir).expanduser()
    session = _env_dir("RLM_SESSION_DIR") if session_dir is None else Path(session_dir)
    workspace = resolve_workspace(agent, session)
    if workspace is not None:
        return agent / "workspaces" / workspace / "world_model.db"
    if session is not None:
        return session / "world_model.db"
    raise RuntimeError(
        "no world-model location: no workspace is pinned and no session artifact dir "
        "is set (RLM_SESSION_DIR); open the store with an explicit path"
    )


def _validate(name: str, value: Any, choices: tuple[str, ...]) -> str:
    if value not in choices:
        raise ValueError(f"{name} must be one of {choices}, got {value!r}")
    return value


def _validate_statement(name: str, value: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be a non-empty string")
    return value


# FTS5 syntax an expert query may deliberately use: pass those through raw.
_FTS_OPERATORS = re.compile(r'["*():^]|\b(?:AND|OR|NOT|NEAR)\b')


def _fts_match_expr(query: str) -> str:
    """A plain query becomes AND over quoted tokens (so `lock contention`
    matches rows containing both words, not the exact phrase); explicit FTS5
    syntax goes through verbatim."""
    if _FTS_OPERATORS.search(query):
        return query
    return " AND ".join(f'"{token}"' for token in query.replace('"', "").split()) or query


#: The last object `_SCHEMA` creates; a store that has it finished schema
#: creation (an interrupted pass leaves it missing, so init re-runs safely).
_SCHEMA_SENTINEL = "evidence_ad"


def _connect(path: Path) -> sqlite3.Connection:
    """A per-operation connection: `busy_timeout` makes every lock wait
    queue instead of failing. WAL persists in the database file, so only
    initialization ever touches the journal mode."""
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, timeout=5.0, isolation_level=None)
    conn.execute("PRAGMA busy_timeout=5000")
    conn.execute("PRAGMA foreign_keys=ON")
    return conn


def _initialize(path: Path, attempts: int = 10) -> None:
    """First-open initialization, retried around every contended step.

    Converting a fresh database to WAL does not invoke the busy handler, so
    simultaneous first opens race with an immediate "database is locked";
    the WAL check/switch and the schema pass all live inside the retry. A
    store that already carries the schema (its last object exists) and the
    WAL journal skips both, so instantiating a live store never contends
    with its writers.
    """
    last: sqlite3.OperationalError | None = None
    for attempt in range(attempts):
        conn = _connect(path)
        try:
            mode = str(conn.execute("PRAGMA journal_mode").fetchone()[0]).lower()
            if mode != "wal":
                conn.execute("PRAGMA journal_mode=WAL")
            initialized = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE name = ?", (_SCHEMA_SENTINEL,)
            ).fetchone()
            if initialized is None:
                conn.executescript(_SCHEMA)
            return
        except sqlite3.OperationalError as e:
            last = e
        finally:
            conn.close()
        time.sleep(0.05 * (attempt + 1))
    raise RuntimeError(f"cannot initialize the world model at {path}: {last}")


@contextmanager
def _txn(conn: sqlite3.Connection) -> Iterator[None]:
    """A write transaction that takes the lock upfront (BEGIN IMMEDIATE).

    With `busy_timeout` set, concurrent writers queue instead of failing, so
    multi-process writers never need their own retry loops.
    """
    conn.execute("BEGIN IMMEDIATE")
    try:
        yield
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    conn.execute("COMMIT")


class WorldModel:
    """One world-model store. Every method opens its own short-lived
    connection with WAL on, so a single instance is safe to hand to code that
    runs across processes or threads without sharing a connection."""

    def __init__(self, path: str | Path | None = None) -> None:
        if path is None:
            self.db_path = default_db_path()
        else:
            self.db_path = Path(path).expanduser()
        _initialize(self.db_path)

    def _open(self) -> sqlite3.Connection:
        conn = _connect(self.db_path)
        conn.row_factory = sqlite3.Row
        return conn

    @contextmanager
    def _conn(self) -> Iterator[sqlite3.Connection]:
        conn = self._open()
        try:
            yield conn
        finally:
            conn.close()

    # -- writes ---------------------------------------------------------

    def add_hypothesis(
        self,
        statement: str,
        *,
        status: str = "open",
        priority: int = 1,
        cycle: int = 0,
    ) -> int:
        """Record a hypothesis; returns its id."""
        _validate_statement("statement", statement)
        _validate("status", status, _HYPOTHESIS_STATUSES)
        now = _now()
        with self._conn() as conn, _txn(conn):
            cur = conn.execute(
                "INSERT INTO hypotheses (statement, status, priority, cycle, created_at, updated_at)"
                " VALUES (?, ?, ?, ?, ?, ?)",
                (statement, status, priority, cycle, now, now),
            )
            return cur.lastrowid

    def add_evidence(
        self,
        kind: str,
        ref: str,
        *,
        excerpt: str = "",
        artifact_path: str | None = None,
        author: str | None = None,
        cycle: int = 0,
    ) -> int:
        """Record immutable evidence; returns its id.

        `kind` is one of code_run, file_slice, test_run, paper, web, dataset;
        `ref` names where the evidence comes from (a command, path, DOI, URL,
        or dataset id). Evidence is never updated or deleted afterwards.
        """
        _validate("kind", kind, _EVIDENCE_KINDS)
        _validate_statement("ref", ref)
        with self._conn() as conn, _txn(conn):
            cur = conn.execute(
                "INSERT INTO evidence (kind, ref, excerpt, artifact_path, author, cycle, created_at)"
                " VALUES (?, ?, ?, ?, ?, ?, ?)",
                (kind, ref, excerpt, artifact_path, author, cycle, _now()),
            )
            return cur.lastrowid

    def add_claim(
        self,
        kind: str,
        statement: str,
        *,
        status: str = "open",
        confidence: float | None = None,
        cycle: int = 0,
        evidence_ids: list[int] | tuple[int, ...] = (),
    ) -> int:
        """Record a claim; returns its id.

        Provenance gate: `status="supported"` is rejected unless at least one
        `evidence_ids` entry names an existing evidence row.
        """
        _validate("kind", kind, _CLAIM_KINDS)
        _validate_statement("statement", statement)
        _validate("status", status, _CLAIM_STATUSES)
        if confidence is not None and not 0.0 <= confidence <= 1.0:
            raise ValueError(f"confidence must be within [0, 1], got {confidence!r}")
        now = _now()
        with self._conn() as conn, _txn(conn):
            if status == "supported":
                self._require_evidence(conn, evidence_ids)
            cur = conn.execute(
                "INSERT INTO claims (kind, statement, status, confidence, cycle, created_at, updated_at)"
                " VALUES (?, ?, ?, ?, ?, ?, ?)",
                (kind, statement, status, confidence, cycle, now, now),
            )
            claim_id = cur.lastrowid
            self._link_evidence(conn, claim_id, evidence_ids)
            return claim_id

    @staticmethod
    def _require_evidence(conn: sqlite3.Connection, evidence_ids: list[int] | tuple[int, ...]) -> None:
        if not evidence_ids:
            raise ValueError(
                'a claim with status="supported" requires at least one evidence_id: '
                "add evidence first, or record the claim as open and attach evidence later"
            )
        missing = [
            eid
            for eid in evidence_ids
            if conn.execute("SELECT 1 FROM evidence WHERE id = ?", (eid,)).fetchone() is None
        ]
        if missing:
            raise ValueError(f"evidence ids do not exist: {sorted(missing)}")

    @staticmethod
    def _link_evidence(
        conn: sqlite3.Connection, claim_id: int, evidence_ids: list[int] | tuple[int, ...]
    ) -> None:
        claim = conn.execute("SELECT 1 FROM claims WHERE id = ?", (claim_id,)).fetchone()
        if claim is None:
            raise ValueError(f"claim id {claim_id} does not exist")
        for eid in evidence_ids:
            if not isinstance(eid, int):
                raise ValueError(f"evidence id {eid!r} is not an int")
            exists = conn.execute("SELECT 1 FROM evidence WHERE id = ?", (eid,)).fetchone()
            if exists is None:
                raise ValueError(f"evidence id {eid} does not exist")
            conn.execute(
                "INSERT OR IGNORE INTO claim_evidence (claim_id, evidence_id) VALUES (?, ?)",
                (claim_id, eid),
            )

    def attach_evidence(self, claim_id: int, evidence_id: int) -> None:
        """Link an existing evidence row to an existing claim."""
        with self._conn() as conn, _txn(conn):
            self._link_evidence(conn, claim_id, [evidence_id])

    def set_claim_status(self, claim_id: int, status: str) -> None:
        """Update a claim's status. Promoting to "supported" passes the same
        provenance gate as `add_claim` (the claim must already have evidence
        linked); downgrades are always allowed."""
        _validate("status", status, _CLAIM_STATUSES)
        with self._conn() as conn, _txn(conn):
            row = conn.execute("SELECT id FROM claims WHERE id = ?", (claim_id,)).fetchone()
            if row is None:
                raise ValueError(f"claim id {claim_id} does not exist")
            if status == "supported":
                linked = conn.execute(
                    "SELECT 1 FROM claim_evidence WHERE claim_id = ? LIMIT 1", (claim_id,)
                ).fetchone()
                if linked is None:
                    raise ValueError(
                        f'claim {claim_id} cannot be promoted to status="supported" '
                        "without linked evidence; call attach_evidence first"
                    )
            conn.execute(
                "UPDATE claims SET status = ?, updated_at = ? WHERE id = ?",
                (status, _now(), claim_id),
            )

    def add_edge(
        self,
        src_id: int,
        dst_id: int,
        rel: str,
        *,
        note: str | None = None,
        cycle: int = 0,
    ) -> int:
        """Record a claim-to-claim relation; returns the edge id.

        `src` supports/refutes/supersedes/derives from `dst` — both must be
        existing claim ids.
        """
        _validate("rel", rel, _EDGE_RELS)
        with self._conn() as conn, _txn(conn):
            for name, cid in (("src_id", src_id), ("dst_id", dst_id)):
                row = conn.execute("SELECT 1 FROM claims WHERE id = ?", (cid,)).fetchone()
                if row is None:
                    raise ValueError(f"edge {name} must name an existing claim, got {cid}")
            cur = conn.execute(
                "INSERT INTO edges (src_id, dst_id, rel, note, cycle, created_at)"
                " VALUES (?, ?, ?, ?, ?, ?)",
                (src_id, dst_id, rel, note, cycle, _now()),
            )
            return cur.lastrowid

    def add_task(
        self,
        role: str,
        prompt: str,
        *,
        cycle: int = 0,
        blocked_by: list[int] | tuple[int, ...] = (),
        assignee: str | None = None,
    ) -> int:
        """Queue a task; returns its id. `blocked_by` lists task ids that must
        reach completed or cancelled before this one can be claimed."""
        _validate_statement("role", role)
        _validate_statement("prompt", prompt)
        now = _now()
        with self._conn() as conn, _txn(conn):
            for tid in blocked_by:
                row = conn.execute("SELECT 1 FROM tasks WHERE id = ?", (tid,)).fetchone()
                if row is None:
                    raise ValueError(f"blocked_by task id {tid} does not exist")
            cur = conn.execute(
                "INSERT INTO tasks (cycle, role, prompt, blocked_by_json, assignee, status,"
                " artifacts_json, created_at, updated_at)"
                " VALUES (?, ?, ?, ?, ?, 'pending', '[]', ?, ?)",
                (cycle, role, prompt, json.dumps(list(blocked_by)), assignee, now, now),
            )
            return cur.lastrowid

    def claim_next(self, *, role: str | None = None, assignee: str | None = None) -> dict[str, Any] | None:
        """Atomically claim the oldest claimable pending task, or None.

        Runs under BEGIN IMMEDIATE so concurrent processes never claim the
        same task; a task is claimable when every `blocked_by` task has
        reached completed or cancelled. The task moves to in_progress and the
        full row (with blocked_by and artifacts parsed) is returned.
        """
        with self._conn() as conn, _txn(conn):
            rows = conn.execute(
                "SELECT id FROM tasks WHERE status = 'pending'"
                " AND (?1 IS NULL OR role = ?1) ORDER BY cycle, id",
                (role,),
            ).fetchall()
            for row in rows:
                if self._unblocked(conn, row["id"]):
                    conn.execute(
                        "UPDATE tasks SET status = 'in_progress',"
                        " assignee = COALESCE(?1, assignee), updated_at = ?2 WHERE id = ?3",
                        (assignee, _now(), row["id"]),
                    )
                    return self._task_row(conn, row["id"])
            return None

    @staticmethod
    def _unblocked(conn: sqlite3.Connection, task_id: int) -> bool:
        row = conn.execute("SELECT blocked_by_json FROM tasks WHERE id = ?", (task_id,)).fetchone()
        blocked_by = json.loads(row["blocked_by_json"])
        if not blocked_by:
            return True
        placeholders = ",".join("?" * len(blocked_by))
        blockers = conn.execute(
            f"SELECT COUNT(*) FROM tasks WHERE id IN ({placeholders})"
            " AND status NOT IN ('completed', 'cancelled')",
            blocked_by,
        ).fetchone()[0]
        return blockers == 0

    def _evidence_id(self, kind: str, ref: str, excerpt: str) -> int | None:
        """The id of an existing evidence row with this (kind, ref, excerpt),
        or None. Factory commits reuse rows instead of duplicating them."""
        with self._conn() as conn:
            row = conn.execute(
                "SELECT id FROM evidence WHERE kind = ?1 AND ref = ?2 AND excerpt = ?3"
                " ORDER BY id LIMIT 1",
                (kind, ref, excerpt),
            ).fetchone()
        return row["id"] if row is not None else None

    def _claim_id(self, statement: str) -> int | None:
        """The id of an existing claim with this exact statement, or None."""
        with self._conn() as conn:
            row = conn.execute(
                "SELECT id FROM claims WHERE statement = ?1", (statement,)
            ).fetchone()
        return row["id"] if row is not None else None

    def _edge_exists(self, src_id: int, dst_id: int, rel: str) -> bool:
        """Whether this exact relation row already exists."""
        with self._conn() as conn:
            row = conn.execute(
                "SELECT 1 FROM edges WHERE src_id = ?1 AND dst_id = ?2 AND rel = ?3 LIMIT 1",
                (src_id, dst_id, rel),
            ).fetchone()
        return row is not None

    def complete_task(
        self, task_id: int, *, result: str | None = None, artifacts: list[str] | tuple[str, ...] = ()
    ) -> None:
        """Mark an in-progress task completed, with its result and artifact paths."""
        self._finish_task(task_id, "completed", result=result, artifacts=artifacts)

    def cancel_task(self, task_id: int, *, result: str | None = None) -> None:
        """Mark an in-progress task cancelled (unblocking its dependents)."""
        self._finish_task(task_id, "cancelled", result=result)

    def _finish_task(
        self,
        task_id: int,
        status: str,
        *,
        result: str | None = None,
        artifacts: list[str] | tuple[str, ...] = (),
    ) -> None:
        _validate("status", status, ("completed", "cancelled"))
        with self._conn() as conn, _txn(conn):
            cur = conn.execute(
                "UPDATE tasks SET status = ?1, result = ?2,"
                " artifacts_json = ?3, updated_at = ?4 WHERE id = ?5 AND status = 'in_progress'",
                (status, result, json.dumps(list(artifacts)), _now(), task_id),
            )
            if cur.rowcount == 0:
                raise ValueError(f"task id {task_id} is not an in_progress task")

    @staticmethod
    def _task_row(conn: sqlite3.Connection, task_id: int) -> dict[str, Any]:
        row = dict(conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone())
        row["blocked_by"] = json.loads(row.pop("blocked_by_json"))
        row["artifacts"] = json.loads(row.pop("artifacts_json"))
        return row

    # -- reads ----------------------------------------------------------

    def search(self, query: str, *, limit: int = 20) -> list[dict[str, Any]]:
        """Full-text search (FTS5) over claim statements and evidence
        references/excerpts, best match first.

        A plain multi-word query matches rows containing every word (token
        AND); explicit FTS5 syntax (quotes, `*`, `AND`/`OR`/`NEAR`, column
        filters) is passed through verbatim.
        """
        _validate_statement("query", query)
        match = _fts_match_expr(query)
        matched: list[dict[str, Any]] = []
        with self._conn() as conn:
            for entity, sql in (
                (
                    "claim",
                    "SELECT c.id, c.kind, c.status, c.statement FROM claims c"
                    " JOIN claims_fts f ON f.rowid = c.id WHERE claims_fts MATCH ?"
                    " ORDER BY rank LIMIT ?",
                ),
                (
                    "evidence",
                    "SELECT e.id, e.kind, e.ref, e.excerpt FROM evidence e"
                    " JOIN evidence_fts f ON f.rowid = e.id WHERE evidence_fts MATCH ?"
                    " ORDER BY rank LIMIT ?",
                ),
            ):
                rows = conn.execute(sql, (match, limit)).fetchall()
                for row in rows:
                    matched.append({"entity": entity, **dict(row)})
        return matched

    def contradictions(self) -> list[dict[str, Any]]:
        """Claim pairs joined by a `refutes` edge: `src` refutes `dst`."""
        with self._conn() as conn:
            rows = conn.execute(
                "SELECT a.id AS src_id, a.statement AS src_statement, a.status AS src_status,"
                " b.id AS dst_id, b.statement AS dst_statement, b.status AS dst_status,"
                " e.note AS note"
                " FROM edges e JOIN claims a ON a.id = e.src_id JOIN claims b ON b.id = e.dst_id"
                " WHERE e.rel = 'refutes' ORDER BY e.id"
            ).fetchall()
        return [dict(row) for row in rows]

    def unrefuted(self) -> list[dict[str, Any]]:
        """Claims that no `refutes` edge targets and that are not themselves
        marked refuted — the set a planner can build on."""
        with self._conn() as conn:
            rows = conn.execute(
                "SELECT c.* FROM claims c WHERE c.status != 'refuted' AND NOT EXISTS"
                " (SELECT 1 FROM edges e WHERE e.rel = 'refutes' AND e.dst_id = c.id)"
                " ORDER BY c.cycle DESC, c.id"
            ).fetchall()
        return [dict(row) for row in rows]

    def digest(self, *, max_chars: int = 4096) -> str:
        """A compact planner summary: counts, open hypotheses, surviving
        supported claims, in-progress and pending tasks, contradictions."""
        with self._conn() as conn:
            lines = [f"World Model — {self.db_path}", ""]
            counts = {
                row["status"]: row["n"]
                for row in conn.execute("SELECT status, COUNT(*) AS n FROM hypotheses GROUP BY status")
            }
            lines.append(
                "Hypotheses: {} open / {} supported / {} refuted / {} parked".format(
                    counts.get("open", 0),
                    counts.get("supported", 0),
                    counts.get("refuted", 0),
                    counts.get("parked", 0),
                )
            )
            claim_counts = {
                row["status"]: row["n"]
                for row in conn.execute("SELECT status, COUNT(*) AS n FROM claims GROUP BY status")
            }
            evidence_count = conn.execute("SELECT COUNT(*) FROM evidence").fetchone()[0]
            lines.append(
                "Claims: {} supported / {} open / {} refuted; evidence: {}".format(
                    claim_counts.get("supported", 0),
                    claim_counts.get("open", 0),
                    claim_counts.get("refuted", 0),
                    evidence_count,
                )
            )
            task_counts = {
                row["status"]: row["n"]
                for row in conn.execute("SELECT status, COUNT(*) AS n FROM tasks GROUP BY status")
            }
            lines.append(
                "Tasks: {} pending / {} in_progress / {} completed / {} cancelled".format(
                    task_counts.get("pending", 0),
                    task_counts.get("in_progress", 0),
                    task_counts.get("completed", 0),
                    task_counts.get("cancelled", 0),
                )
            )
            contradiction_count = conn.execute(
                "SELECT COUNT(*) FROM edges WHERE rel = 'refutes'"
            ).fetchone()[0]
            lines.append(f"Contradictions: {contradiction_count}")
            lines.append("")
            open_hypotheses = conn.execute(
                "SELECT id, statement, priority, cycle FROM hypotheses WHERE status = 'open'"
                " ORDER BY priority DESC, id LIMIT 10"
            ).fetchall()
            if open_hypotheses:
                lines.append("Open hypotheses:")
                lines.extend(
                    f"- [h{h['id']}|pri {h['priority']}|cycle {h['cycle']}] {h['statement']}"
                    for h in open_hypotheses
                )
                lines.append("")
            surviving = conn.execute(
                "SELECT c.* FROM claims c WHERE c.status = 'supported' AND NOT EXISTS"
                " (SELECT 1 FROM edges e WHERE e.rel = 'refutes' AND e.dst_id = c.id)"
                " ORDER BY c.cycle DESC, c.id LIMIT 10"
            ).fetchall()
            if surviving:
                lines.append("Supported claims (unrefuted):")
                lines.extend(
                    f"- [c{c['id']}|{c['kind']}"
                    + (f"|conf {c['confidence']}" if c["confidence"] is not None else "")
                    + f"] {c['statement']}"
                    for c in surviving
                )
                lines.append("")
            for status, label in (("in_progress", "In-progress tasks"), ("pending", "Pending tasks")):
                rows = conn.execute(
                    "SELECT id, role, prompt FROM tasks WHERE status = ?1 ORDER BY cycle, id LIMIT 10",
                    (status,),
                ).fetchall()
                if rows:
                    lines.append(f"{label}:")
                    lines.extend(
                        f"- [t{t['id']}|{t['role']}] {t['prompt'][:80]}" for t in rows
                    )
                    lines.append("")
        text = "\n".join(lines).rstrip()
        if len(text) > max_chars:
            marker = f"\n... [digest truncated, {len(text)} chars total] ..."
            text = text[: max(0, max_chars - len(marker))] + marker
        return text

    def export_markdown(self) -> str:
        """The whole store as a Markdown document, for reading or reports."""
        with self._conn() as conn:
            out = [f"# World Model", "", f"_Generated {_now()} from `{self.db_path}`_", ""]
            out.append("## Hypotheses")
            for row in conn.execute("SELECT * FROM hypotheses ORDER BY id"):
                out.append(
                    f"- **h{row['id']}** ({row['status']}, priority {row['priority']},"
                    f" cycle {row['cycle']}): {row['statement']}"
                )
            out.append("")
            out.append("## Evidence")
            for row in conn.execute("SELECT * FROM evidence ORDER BY id"):
                out.append(f"- **e{row['id']}** ({row['kind']}, cycle {row['cycle']}): {row['ref']}")
                if row["excerpt"]:
                    out.append(f"  > {row['excerpt'].replace(chr(10), ' ')}")
                if row["artifact_path"]:
                    out.append(f"  artifact: `{row['artifact_path']}`")
            out.append("")
            out.append("## Claims")
            for row in conn.execute("SELECT * FROM claims ORDER BY id"):
                confidence = f", confidence {row['confidence']}" if row["confidence"] is not None else ""
                out.append(
                    f"- **c{row['id']}** ({row['kind']}, {row['status']}{confidence},"
                    f" cycle {row['cycle']}): {row['statement']}"
                )
                links = conn.execute(
                    "SELECT evidence_id FROM claim_evidence WHERE claim_id = ? ORDER BY evidence_id",
                    (row["id"],),
                ).fetchall()
                for link in links:
                    out.append(f"  - evidence: e{link['evidence_id']}")
            out.append("")
            out.append("## Edges")
            for row in conn.execute(
                "SELECT * FROM edges ORDER BY id"
            ):
                note = f" — {row['note']}" if row["note"] else ""
                out.append(f"- c{row['src_id']} --{row['rel']}--> c{row['dst_id']}{note}")
            out.append("")
            out.append("## Tasks")
            for row in conn.execute("SELECT * FROM tasks ORDER BY id"):
                blocked_by = json.loads(row["blocked_by_json"])
                blocked = f", blocked by {blocked_by}" if blocked_by else ""
                assignee = f", assignee {row['assignee']}" if row["assignee"] else ""
                out.append(
                    f"- **t{row['id']}** (cycle {row['cycle']}, {row['role']},"
                    f" {row['status']}{blocked}{assignee}): {row['prompt']}"
                )
                if row["result"]:
                    out.append(f"  result: {row['result']}")
                artifacts = json.loads(row["artifacts_json"])
                for artifact in artifacts:
                    out.append(f"  artifact: `{artifact}`")
        return "\n".join(out).rstrip() + "\n"
