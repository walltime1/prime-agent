"""Factory-machine state callables for the discovery-loop machine.

These are the pure-Python states the bundled ``discovery-loop`` machine
references: ``init`` opens the scoped store and renders the starting
digest; ``commit`` validates provenance, writes the cycle's verified
updates, and evaluates the stop condition. They follow the factory's
python-state contract: one dict of declared inputs in, one dict of exactly
the declared outputs out, importable with no kernel globals.

Idempotency: the factory re-runs a python call on retry and after an
escalate pause resumes, so ``commit`` never duplicates rows — evidence is
keyed on (kind, ref, excerpt), claims on their unique statement, and
edges on (src, dst, rel). A repeated call reuses what exists and reports
only the new rows.
"""

from __future__ import annotations

import sqlite3
from typing import Any

from .world_model import (
    _CLAIM_KINDS,
    _CLAIM_STATUSES,
    _EVIDENCE_KINDS,
    WorldModel,
)

_DIGEST_CHARS = 4000


def init(inputs: dict[str, Any]) -> dict[str, Any]:
    """Entry state: open the default-scoped store, render the digest.

    Fails (and the run escalates) when no workspace is pinned and no
    session-local scope is available: a discovery run needs a persistent
    store. Pin a workspace before starting the machine.
    """
    del inputs  # an entry state binds no inputs
    wm = WorldModel()
    return {"digest": wm.digest(max_chars=_DIGEST_CHARS), "db_path": str(wm.db_path)}


def commit(inputs: dict[str, Any]) -> dict[str, Any]:
    """Write one cycle's verified updates and evaluate the stop condition.

    Inputs: ``db_path`` (required), ``plan`` (the planner's
    ``{"done": bool, "reason": str}``, optional), and ``candidates`` (the
    skeptic's verified update set, optional). Candidates carry ``evidence``
    rows, ``claims`` whose ``evidence`` fields index that array, and
    ``refutes`` pairs of existing claim ids. The provenance gate is hard: a
    claim marked supported without valid evidence is downgraded to open and
    reported, never silently written. ``done`` mirrors the planner's stop
    judgment.
    """
    db_path = inputs.get("db_path")
    if not isinstance(db_path, str) or not db_path:
        raise ValueError("commit requires a non-empty db_path input")
    plan = inputs.get("plan")
    if plan is not None and not isinstance(plan, dict):
        raise ValueError(f"commit plan input must be a JSON object, got {type(plan).__name__}")
    candidates = inputs.get("candidates")
    if candidates is not None and not isinstance(candidates, dict):
        raise ValueError(
            f"commit candidates input must be a JSON object, got {type(candidates).__name__}"
        )

    wm = WorldModel(db_path)
    cycle = _next_cycle(wm)
    report: dict[str, Any] = {
        "cycle": cycle,
        "evidence_added": 0,
        "evidence_reused": 0,
        "claims_added": 0,
        "claims_reused": 0,
        "claims_downgraded": [],
        "edges_added": 0,
        "skipped": [],
        "done": bool((plan or {}).get("done")),
        "plan_reason": (plan or {}).get("reason"),
    }

    evidence_ids = _commit_evidence(wm, candidates or {}, cycle, report)
    _commit_claims(wm, candidates or {}, cycle, evidence_ids, report)
    _commit_refutes(wm, candidates or {}, cycle, report)

    return {
        "digest": wm.digest(max_chars=_DIGEST_CHARS),
        "done": report["done"],
        "report": report,
    }


def _next_cycle(wm: WorldModel) -> int:
    """The cycle label for this commit: one past the highest stored cycle."""
    highest = -1
    with wm._conn() as conn:  # same-package private seam
        for table in ("hypotheses", "evidence", "claims", "tasks"):
            row = conn.execute(f"SELECT MAX(cycle) FROM {table}").fetchone()
            highest = max(highest, row[0] if row[0] is not None else -1)
    return highest + 1


def _commit_evidence(
    wm: WorldModel, candidates: dict[str, Any], cycle: int, report: dict[str, Any]
) -> list[int]:
    """Insert the cycle's evidence rows (idempotently); their ids in order."""
    evidence_ids: list[int] = []
    rows = candidates.get("evidence")
    if rows is None:
        return evidence_ids
    if not isinstance(rows, list):
        raise ValueError("candidates.evidence must be a list")
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            report["skipped"].append(f"evidence[{index}] is not an object")
            continue
        kind, ref, excerpt = row.get("kind"), row.get("ref"), row.get("excerpt") or ""
        if kind not in _EVIDENCE_KINDS or not isinstance(ref, str) or not ref.strip():
            report["skipped"].append(f"evidence[{index}] has invalid kind {kind!r} or empty ref")
            continue
        existing = wm._evidence_id(kind, ref, excerpt)  # same package
        if existing is not None:
            evidence_ids.append(existing)
            report["evidence_reused"] += 1
            continue
        evidence_ids.append(
            wm.add_evidence(
                kind,
                ref,
                excerpt=excerpt if isinstance(excerpt, str) else "",
                artifact_path=row.get("artifact_path"),
                author=row.get("author"),
                cycle=cycle,
            )
        )
        report["evidence_added"] += 1
    return evidence_ids


def _commit_claims(
    wm: WorldModel,
    candidates: dict[str, Any],
    cycle: int,
    evidence_ids: list[int],
    report: dict[str, Any],
) -> None:
    rows = candidates.get("claims")
    if rows is None:
        return
    if not isinstance(rows, list):
        raise ValueError("candidates.claims must be a list")
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            report["skipped"].append(f"claims[{index}] is not an object")
            continue
        statement = row.get("statement")
        if row.get("kind") not in _CLAIM_KINDS or not isinstance(statement, str) or not statement.strip():
            report["skipped"].append(f"claims[{index}] has invalid kind or empty statement")
            continue
        status = row.get("status") or "open"
        if status not in _CLAIM_STATUSES:
            report["skipped"].append(f"claims[{index}] has invalid status {status!r}")
            continue
        linked = _linked_ids(row.get("evidence"), evidence_ids, index, report)
        if status == "supported" and not linked:
            report["claims_downgraded"].append(statement)
            status = "open"
        claim_id = wm._claim_id(statement)  # same package
        if claim_id is not None:
            for eid in linked:
                wm.attach_evidence(claim_id, eid)
            if status != "open":
                wm.set_claim_status(claim_id, status)
            report["claims_reused"] += 1
            continue
        wm.add_claim(
            row["kind"],
            statement,
            status=status,
            confidence=_confidence(row.get("confidence")),
            cycle=cycle,
            evidence_ids=linked,
        )
        report["claims_added"] += 1


def _linked_ids(
    raw: Any, evidence_ids: list[int], index: int, report: dict[str, Any]
) -> list[int]:
    """The evidence ids a claim's index list resolves to, in order."""
    if raw is None:
        return []
    if not isinstance(raw, list):
        report["skipped"].append(f"claims[{index}].evidence must be a list of indices")
        return []
    linked: list[int] = []
    for value in raw:
        if isinstance(value, bool) or not isinstance(value, int):
            report["skipped"].append(f"claims[{index}].evidence holds a non-index {value!r}")
            continue
        if not 0 <= value < len(evidence_ids):
            report["skipped"].append(f"claims[{index}].evidence index {value} is out of range")
            continue
        linked.append(evidence_ids[value])
    return linked


def _confidence(raw: Any) -> float | None:
    if raw is None:
        return None
    return raw if isinstance(raw, (int, float)) and not isinstance(raw, bool) and 0.0 <= raw <= 1.0 else None


def _commit_refutes(wm: WorldModel, candidates: dict[str, Any], cycle: int, report: dict[str, Any]) -> None:
    rows = candidates.get("refutes")
    if rows is None:
        return
    if not isinstance(rows, list):
        raise ValueError("candidates.refutes must be a list")
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            report["skipped"].append(f"refutes[{index}] is not an object")
            continue
        src, dst = row.get("src"), row.get("dst")
        if isinstance(src, bool) or not isinstance(src, int) or isinstance(dst, bool) or not isinstance(dst, int):
            report["skipped"].append(f"refutes[{index}] requires integer claim ids, got src={src!r} dst={dst!r}")
            continue
        if wm._edge_exists(src, dst, "refutes"):  # same package
            continue
        try:
            wm.add_edge(src, dst, "refutes", note=row.get("note"), cycle=cycle)
        except ValueError as e:
            report["skipped"].append(f"refutes[{index}]: {e}")
            continue
        report["edges_added"] += 1
