"""Multi-process WAL concurrency tests for the world-model skill.

Eight worker processes share one store and run the three contended operations
at once: `claim_next()`, `add_evidence()`, and `add_claim()`. Every write
path takes `BEGIN IMMEDIATE` with `busy_timeout=5000`, so the assertions are
zero lock failures, and every task claimed exactly once.
"""

from __future__ import annotations

import json
import multiprocessing
import queue
import sys
import tempfile
import unittest
from pathlib import Path

_SRC = Path(__file__).resolve().parents[1] / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))

from world_model import WorldModel  # noqa: E402

WORKER_COUNT = 8
WRITE_BURSTS = 5
TASK_COUNT = 32


def _worker(db_path: str, worker_id: int, result_queue: multiprocessing.Queue) -> None:
    """One contending writer: bursts of evidence/claim writes plus a
    claim_next/complete loop until the task queue is drained."""
    claimed: list[int] = []
    errors: list[str] = []
    try:
        wm = WorldModel(db_path)
        for i in range(WRITE_BURSTS):
            eid = wm.add_evidence("code_run", f"worker {worker_id} burst {i}")
            wm.add_claim("finding", f"burst observation w{worker_id}-{i}", evidence_ids=[eid])
        while True:
            task = wm.claim_next(assignee=f"w{worker_id}")
            if task is None:
                break
            eid = wm.add_evidence(
                "test_run", f"worker {worker_id} task t{task['id']}", excerpt="no lock failures"
            )
            wm.add_claim(
                "finding",
                f"task t{task['id']} audited by w{worker_id}",
                status="supported",
                evidence_ids=[eid],
            )
            wm.complete_task(task["id"], result="done", artifacts=[f"/tmp/w{worker_id}.log"])
            claimed.append(task["id"])
    except Exception as e:  # reported to the parent; the test fails on any
        errors.append(f"{type(e).__name__}: {e}")
    result_queue.put({"worker": worker_id, "claimed": claimed, "errors": errors})


def _pure_writer(db_path: str, worker_id: int, out: multiprocessing.Queue) -> None:
    """Contending writer with no task queue: evidence plus a supported claim."""
    try:
        wm = WorldModel(db_path)
        for i in range(WRITE_BURSTS):
            eid = wm.add_evidence("dataset", f"ds-{worker_id}-{i}")
            wm.add_claim(
                "finding",
                f"writer {worker_id} observation {i}",
                status="supported",
                evidence_ids=[eid],
                confidence=0.5,
            )
        out.put({"worker": worker_id, "errors": []})
    except Exception as e:
        out.put({"worker": worker_id, "errors": [f"{type(e).__name__}: {e}"]})


class EightProcessConcurrencyTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.db_path = str(Path(self._tmp.name) / "world_model.db")

    def _seed_tasks(self, wm: WorldModel) -> list[int]:
        ids = [wm.add_task("worker", f"gather source {i}", cycle=i // 8) for i in range(TASK_COUNT)]
        # A few dependents: claimable only once their blockers finish.
        wm.add_task("worker", "blocked summary", blocked_by=[ids[0], ids[1]])
        wm.add_task("worker", "second blocked", blocked_by=[ids[10]])
        return ids + [TASK_COUNT + 1, TASK_COUNT + 2]

    def test_eight_processes_claim_and_write_without_lock_failures(self) -> None:
        wm = WorldModel(self.db_path)
        all_task_ids = self._seed_tasks(wm)

        result_queue: multiprocessing.Queue = multiprocessing.Queue()
        ctx = multiprocessing.get_context("spawn")
        processes = [
            ctx.Process(target=_worker, args=(self.db_path, i, result_queue))
            for i in range(WORKER_COUNT)
        ]
        for p in processes:
            p.start()
        results = [result_queue.get(timeout=120) for _ in processes]
        for p in processes:
            p.join(timeout=60)
            self.assertFalse(p.is_alive(), f"worker {p.pid} did not exit")

        errors = [e for r in results for e in r["errors"]]
        self.assertEqual(errors, [], f"worker errors: {errors}")

        # Every task — including the blocked ones — is claimed exactly once.
        claimed = [tid for r in results for tid in r["claimed"]]
        self.assertCountEqual(claimed, all_task_ids)
        self.assertEqual(len(claimed), len(set(claimed)), "a task was claimed twice")

        # Final store state: all tasks completed, write counts exact, and every
        # supported claim carries evidence.
        import sqlite3

        conn = sqlite3.connect(self.db_path)
        self.addCleanup(conn.close)
        statuses = dict(
            conn.execute("SELECT status, COUNT(*) FROM tasks GROUP BY status").fetchall()
        )
        self.assertEqual(statuses, {"completed": len(all_task_ids)})

        expected_evidence = WORKER_COUNT * WRITE_BURSTS + len(all_task_ids)
        self.assertEqual(
            conn.execute("SELECT COUNT(*) FROM evidence").fetchone()[0], expected_evidence
        )
        unsupported = conn.execute(
            "SELECT COUNT(*) FROM claims c WHERE c.status = 'supported' AND NOT EXISTS"
            " (SELECT 1 FROM claim_evidence ce WHERE ce.claim_id = c.id)"
        ).fetchone()[0]
        self.assertEqual(unsupported, 0)
        supported_count = conn.execute(
            "SELECT COUNT(*) FROM claims WHERE status = 'supported'"
        ).fetchone()[0]
        self.assertEqual(supported_count, len(all_task_ids))
        # Claim distribution across workers is timing-dependent (a late-spawned
        # worker may find the queue drained); exactly-once claiming above is
        # the invariant.

    def test_concurrent_writers_all_commit(self) -> None:
        """Pure write contention: every process's evidence and claim rows land."""

        ctx = multiprocessing.get_context("spawn")
        out: multiprocessing.Queue = multiprocessing.Queue()
        processes = [
            ctx.Process(target=_pure_writer, args=(self.db_path, i, out))
            for i in range(WORKER_COUNT)
        ]
        for p in processes:
            p.start()
        results = [out.get(timeout=120) for _ in processes]
        for p in processes:
            p.join(timeout=60)
            self.assertFalse(p.is_alive())

        errors = [e for r in results for e in r["errors"]]
        self.assertEqual(errors, [], f"writer errors: {errors}")

        import sqlite3

        conn = sqlite3.connect(self.db_path)
        self.addCleanup(conn.close)
        self.assertEqual(
            conn.execute("SELECT COUNT(*) FROM evidence").fetchone()[0],
            WORKER_COUNT * WRITE_BURSTS,
        )
        self.assertEqual(
            conn.execute("SELECT COUNT(*) FROM claims").fetchone()[0],
            WORKER_COUNT * WRITE_BURSTS,
        )


if __name__ == "__main__":
    unittest.main()
