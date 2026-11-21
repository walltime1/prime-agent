"""Two-cycle end-to-end test for the discovery-loop python states.

Drives the real `world_model.factory_states` callables through the exact
input shapes the machine binds: init opens the scoped store, scripted
plan/candidates payloads stand in for the machine's agent states, and the
final report is built the way the synthesize prompt instructs. The
verifier is the invariant the machine exists for: every claim the report
presents as supported links to a real evidence row, and no write
duplicated anything.
"""

from __future__ import annotations

import os
import re
import sys
import tempfile
import unittest
from pathlib import Path

_SRC = Path(__file__).resolve().parents[1] / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))

from world_model import WorldModel  # noqa: E402
from world_model.factory_states import commit, init  # noqa: E402

_SCOPE_ENV = (
    "PRIME_AGENT_CODING_AGENT_DIR",
    "PI_CODING_AGENT_DIR",
    "RLM_SESSION_DIR",
    "RLM_WORKSPACE_HARNESS_STATE_DIR",
)

_PLAN_CYCLE_1 = {
    "plan": {"done": False, "reason": "open hypotheses remain"},
    "tasks": [
        {"role": "codebase-explorer", "prompt": "Trace how the gate binds evidence"},
        {"role": "reproducer-benchmarker", "prompt": "Run the concurrency suite"},
    ],
}
_PLAN_DONE = {"plan": {"done": True, "reason": "objective met"}, "tasks": []}

_CANDIDATES_CYCLE_1 = {
    "evidence": [
        {"kind": "code_run", "ref": "python -m unittest discover -s tests", "excerpt": "37 tests OK"},
        {
            "kind": "file_slice",
            "ref": "skills/world-model/src/world_model/world_model.py",
            "excerpt": "a claim with status supported requires at least one evidence_id",
        },
    ],
    "claims": [
        {
            "kind": "finding",
            "statement": "the provenance gate rejects unsupported claims",
            "status": "supported",
            "confidence": 0.9,
            "evidence": [1],
        },
        {
            "kind": "finding",
            "statement": "workers never hit lock failures",
            "status": "supported",
            "evidence": [],
        },
    ],
    "refutes": [{"src": 2, "dst": 1, "note": "the seeded counter-claim stands"}],
}

_REPORT_MARKER = re.compile(r"\[e(\d+)\]")


class DiscoveryLoopEndToEndTest(unittest.TestCase):
    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        saved = {name: os.environ.get(name) for name in _SCOPE_ENV}
        self.addCleanup(lambda: self._restore(saved))
        for name in _SCOPE_ENV:
            os.environ.pop(name, None)
        self.agent_dir = Path(temp.name) / "agent"
        self.agent_dir.mkdir()
        os.environ["PRIME_AGENT_CODING_AGENT_DIR"] = str(self.agent_dir)
        (self.agent_dir / "active_workspace").write_text("research\n")
        self.db_path = self.agent_dir / "workspaces" / "research" / "world_model.db"

        # The session seeds the objective and prior claims before the run.
        wm = WorldModel(self.db_path)
        wm.add_hypothesis("Objective: verify the provenance gate end to end")
        seed_evidence = wm.add_evidence("code_run", "the seeded run", excerpt="gate held")
        wm.add_claim(
            "finding", "the gate held in the seeded run", status="supported", evidence_ids=[seed_evidence]
        )
        wm.add_claim("finding", "the gate never fails anywhere")

    @staticmethod
    def _restore(saved: dict[str, str | None]) -> None:
        for name, value in saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def test_two_cycles_produce_a_provenance_clean_report(self) -> None:
        entry = init({})
        self.assertEqual(entry["db_path"], str(self.db_path))
        self.assertIn("Objective: verify the provenance gate end to end", entry["digest"])

        # Cycle 1: the scripted plan and skeptic payloads commit.
        first = commit({"db_path": entry["db_path"], "plan": None, "candidates": None})
        self.assertFalse(first["done"])

        cycle_one = commit(
            {"db_path": entry["db_path"], "plan": _PLAN_CYCLE_1["plan"], "candidates": _CANDIDATES_CYCLE_1}
        )
        self.assertFalse(cycle_one["done"])
        report = cycle_one["report"]
        self.assertEqual(report["cycle"], 1)
        self.assertEqual(report["evidence_added"], 2)
        self.assertEqual(report["claims_added"], 2)
        self.assertEqual(report["claims_downgraded"], ["workers never hit lock failures"])
        self.assertEqual(report["edges_added"], 1)

        # A retry or resume re-runs the same commit: nothing duplicates.
        replay = commit(
            {"db_path": entry["db_path"], "plan": _PLAN_CYCLE_1["plan"], "candidates": _CANDIDATES_CYCLE_1}
        )
        self.assertEqual(replay["report"]["evidence_added"], 0)
        self.assertEqual(replay["report"]["claims_added"], 0)
        self.assertEqual(replay["report"]["edges_added"], 0)
        self.assertFalse(replay["done"])

        # Cycle 2: the planner declares the objective met.
        closing = commit({"db_path": entry["db_path"], "plan": _PLAN_DONE["plan"], "candidates": _CANDIDATES_CYCLE_1})
        self.assertTrue(closing["done"])

        # The synthesize step, built as its prompt instructs: every
        # supported claim cites its evidence rows.
        wm = WorldModel(self.db_path)
        lines = ["# Report", ""]
        for claim in wm.unrefuted():
            if claim["status"] != "supported":
                continue
            links = self._evidence_links(claim["id"])
            self.assertTrue(links, f"supported claim c{claim['id']} has no evidence")
            markers = " ".join(f"[e{eid}]" for eid in links)
            lines.append(f"- {claim['statement']} {markers}")
        report_text = "\n".join(lines)

        # Every [eNN] marker in the report names a real evidence row.
        markers = [int(m) for m in _REPORT_MARKER.findall(report_text)]
        self.assertTrue(markers)
        for eid in markers:
            self.assertTrue(self._evidence_exists(eid), f"report cites missing evidence e{eid}")
        # The downgraded leap never reached the supported set.
        self.assertNotIn("workers never hit lock failures", report_text)

        # Store totals: seed + one cycle, nothing duplicated by the replay.
        self.assertEqual(self._count("SELECT COUNT(*) FROM evidence"), 3)
        self.assertEqual(self._count("SELECT COUNT(*) FROM claims"), 4)
        self.assertEqual(self._count("SELECT COUNT(*) FROM edges"), 1)

    def _evidence_links(self, claim_id: int) -> list[int]:
        wm = WorldModel(self.db_path)
        with wm._conn() as conn:
            rows = conn.execute(
                "SELECT evidence_id FROM claim_evidence WHERE claim_id = ? ORDER BY evidence_id",
                (claim_id,),
            ).fetchall()
        return [row["evidence_id"] for row in rows]

    def _evidence_exists(self, evidence_id: int) -> bool:
        wm = WorldModel(self.db_path)
        with wm._conn() as conn:
            row = conn.execute("SELECT 1 FROM evidence WHERE id = ?", (evidence_id,)).fetchone()
        return row is not None

    def _count(self, sql: str) -> int:
        import sqlite3

        conn = sqlite3.connect(self.db_path)
        self.addCleanup(conn.close)
        return conn.execute(sql).fetchone()[0]

    def test_init_fails_fast_when_nothing_is_pinned(self) -> None:
        os.environ.pop("PRIME_AGENT_CODING_AGENT_DIR")
        os.environ["PRIME_AGENT_CODING_AGENT_DIR"] = str(self.agent_dir / "nowhere")
        (self.agent_dir / "nowhere").mkdir()
        with self.assertRaises(RuntimeError):
            init({})

    def test_commit_rejects_malformed_inputs(self) -> None:
        with self.assertRaises(ValueError):
            commit({"db_path": "", "plan": None, "candidates": None})
        with self.assertRaises(ValueError):
            commit({"db_path": str(self.db_path), "plan": ["not", "an", "object"], "candidates": None})
        with self.assertRaises(ValueError):
            commit({"db_path": str(self.db_path), "plan": None, "candidates": "nope"})


if __name__ == "__main__":
    unittest.main()
