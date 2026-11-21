"""Unit tests for the bundled world-model skill."""

from __future__ import annotations

import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_SRC = Path(__file__).resolve().parents[1] / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))

from world_model import WorldModel, default_db_path, resolve_workspace  # noqa: E402

# Env vars the path resolution reads; tests control them explicitly.
_SCOPE_ENV = (
    "PRIME_AGENT_CODING_AGENT_DIR",
    "PI_CODING_AGENT_DIR",
    "RLM_SESSION_DIR",
    "RLM_WORKSPACE_HARNESS_STATE_DIR",
)


class StoreTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.db_path = str(Path(self._tmp.name) / "world_model.db")
        self.wm = WorldModel(self.db_path)

    def raw_conn(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path)
        self.addCleanup(conn.close)
        return conn


class PragmaAndSchemaTests(StoreTestCase):
    def test_connection_pragmas(self) -> None:
        conn = self.raw_conn()
        self.assertEqual(conn.execute("PRAGMA journal_mode").fetchone()[0], "wal")
        # busy_timeout and foreign_keys are per-connection settings on the
        # store's own connections; assert them through the store's helper.
        from world_model.world_model import _connect

        store_conn = _connect(Path(self.db_path))
        self.addCleanup(store_conn.close)
        self.assertEqual(store_conn.execute("PRAGMA busy_timeout").fetchone()[0], 5000)
        self.assertEqual(store_conn.execute("PRAGMA foreign_keys").fetchone()[0], 1)

    def test_contract_tables_exist(self) -> None:
        conn = self.raw_conn()
        names = {
            row[0]
            for row in conn.execute("SELECT name FROM sqlite_master WHERE type IN ('table', 'view')")
        }
        self.assertLessEqual(
            {"hypotheses", "evidence", "claims", "claim_evidence", "edges", "tasks"},
            names,
        )
        self.assertIn("claims_fts", names)
        self.assertIn("evidence_fts", names)


class ProvenanceGateTests(StoreTestCase):
    def test_supported_claim_without_evidence_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_claim("finding", "unsupported leap", status="supported")

    def test_supported_claim_with_unknown_evidence_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_claim("finding", "unsupported leap", status="supported", evidence_ids=[999])

    def test_supported_claim_with_evidence_is_written_and_linked(self) -> None:
        eid = self.wm.add_evidence("code_run", "scripts/bench.py", excerpt="p95=1.2ms")
        cid = self.wm.add_claim(
            "finding", "writes stay fast", status="supported", evidence_ids=[eid]
        )
        conn = self.raw_conn()
        links = conn.execute("SELECT claim_id, evidence_id FROM claim_evidence").fetchall()
        self.assertEqual(links, [(cid, eid)])

    def test_open_claim_without_evidence_is_allowed(self) -> None:
        cid = self.wm.add_claim("decision", "use sqlite")
        row = self.raw_conn().execute("SELECT status FROM claims WHERE id = ?", (cid,)).fetchone()
        self.assertEqual(row[0], "open")

    def test_promotion_without_links_is_rejected_then_allowed(self) -> None:
        cid = self.wm.add_claim("finding", "maybe")
        with self.assertRaises(ValueError):
            self.wm.set_claim_status(cid, "supported")
        eid = self.wm.add_evidence("web", "https://example.org/report")
        self.wm.attach_evidence(cid, eid)
        self.wm.set_claim_status(cid, "supported")
        row = self.raw_conn().execute("SELECT status FROM claims WHERE id = ?", (cid,)).fetchone()
        self.assertEqual(row[0], "supported")

    def test_downgrade_is_always_allowed(self) -> None:
        eid = self.wm.add_evidence("test_run", "pytest -q")
        cid = self.wm.add_claim("finding", "holds", status="supported", evidence_ids=[eid])
        self.wm.set_claim_status(cid, "refuted")
        row = self.raw_conn().execute("SELECT status FROM claims WHERE id = ?", (cid,)).fetchone()
        self.assertEqual(row[0], "refuted")


class ValidationTests(StoreTestCase):
    def test_bad_enums_are_rejected(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_hypothesis("h", status="bogus")
        with self.assertRaises(ValueError):
            self.wm.add_evidence("gossip", "someone said")
        with self.assertRaises(ValueError):
            self.wm.add_claim("opinion", "x")
        with self.assertRaises(ValueError):
            self.wm.add_claim("finding", "x", confidence=1.5)
        with self.assertRaises(ValueError):
            self.wm.add_edge(1, 2, "hates")

    def test_empty_statements_are_rejected(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_hypothesis("  ")
        with self.assertRaises(ValueError):
            self.wm.add_evidence("web", "")
        with self.assertRaises(ValueError):
            self.wm.add_task("role", "")

    def test_edge_endpoints_must_exist(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_edge(1, 2, "supports")

    def test_attach_evidence_requires_existing_rows(self) -> None:
        eid = self.wm.add_evidence("web", "https://example.org")
        with self.assertRaises(ValueError):
            self.wm.attach_evidence(999, eid)
        with self.assertRaises(ValueError):
            self.wm.attach_evidence(1, 999)
        with self.assertRaises(ValueError):
            self.wm.attach_evidence(1, "not an int")

    def test_blocked_by_ids_must_exist(self) -> None:
        with self.assertRaises(ValueError):
            self.wm.add_task("worker", "do things", blocked_by=[999])


class TaskTests(StoreTestCase):
    def test_claim_next_returns_oldest_then_none(self) -> None:
        first = self.wm.add_task("worker", "first")
        second = self.wm.add_task("worker", "second")
        task = self.wm.claim_next()
        assert task is not None
        self.assertEqual(task["id"], first)
        self.assertEqual(task["status"], "in_progress")
        task = self.wm.claim_next()
        assert task is not None
        self.assertEqual(task["id"], second)
        self.assertEqual(self.wm.claim_next(), None)

    def test_role_filter_skips_other_roles(self) -> None:
        self.wm.add_task("writer", "draft report")
        skeptic = self.wm.add_task("skeptic", "audit claim")
        task = self.wm.claim_next(role="skeptic")
        assert task is not None
        self.assertEqual(task["id"], skeptic)
        self.assertEqual(self.wm.claim_next(role="skeptic"), None)

    def test_blocked_task_waits_for_completion(self) -> None:
        blocker = self.wm.add_task("worker", "gather data")
        dependent = self.wm.add_task("worker", "summarize", blocked_by=[blocker])
        claimed = self.wm.claim_next()
        assert claimed is not None
        self.assertEqual(claimed["id"], blocker)
        # While the blocker is in progress the dependent is not claimable.
        self.assertEqual(self.wm.claim_next(), None)
        self.wm.complete_task(blocker, result="data gathered")
        task = self.wm.claim_next()
        assert task is not None
        self.assertEqual(task["id"], dependent)
        self.assertEqual(task["blocked_by"], [blocker])

    def test_cancelled_blocker_unblocks_dependents(self) -> None:
        blocker = self.wm.add_task("worker", "gather data")
        dependent = self.wm.add_task("worker", "summarize", blocked_by=[blocker])
        claimed = self.wm.claim_next()
        assert claimed is not None
        self.wm.cancel_task(blocker, result="obsolete")
        task = self.wm.claim_next()
        assert task is not None
        self.assertEqual(task["id"], dependent)

    def test_claim_next_assigns_and_parses_json_columns(self) -> None:
        tid = self.wm.add_task("worker", "do things", assignee="seed")
        task = self.wm.claim_next(assignee="t6")
        assert task is not None
        self.assertEqual(task["id"], tid)
        self.assertEqual(task["assignee"], "t6")
        self.assertEqual(task["blocked_by"], [])
        self.assertEqual(task["artifacts"], [])

    def test_complete_task_stores_result_and_artifacts(self) -> None:
        tid = self.wm.add_task("worker", "run bench")
        claimed = self.wm.claim_next()
        assert claimed is not None
        self.wm.complete_task(tid, result="p95=1.2ms", artifacts=["/tmp/bench.json"])
        row = self.raw_conn().execute("SELECT * FROM tasks WHERE id = ?", (tid,)).fetchone()
        self.assertEqual(row[6], "completed")  # status column
        self.assertEqual(row[7], '["/tmp/bench.json"]')  # artifacts_json column

    def test_complete_task_rejects_unclaimed_tasks(self) -> None:
        tid = self.wm.add_task("worker", "never claimed")
        with self.assertRaises(ValueError):
            self.wm.complete_task(tid)


class SearchTests(StoreTestCase):
    def test_search_matches_all_words_across_entities(self) -> None:
        eid = self.wm.add_evidence("test_run", "pytest -q", excerpt="8 processes, 0 lock failures")
        self.wm.add_claim("finding", "writers never fail under contention", evidence_ids=[eid])
        self.assertEqual(
            [(row["entity"], row["id"]) for row in self.wm.search("processes failures")],
            [("evidence", eid)],
        )
        claims = [(row["entity"], row["id"]) for row in self.wm.search("writers contention")]
        self.assertIn(("claim", 1), claims)

    def test_search_passes_explicit_fts5_syntax_through(self) -> None:
        eid = self.wm.add_evidence("paper", "arXiv:2511.02824")
        self.wm.add_claim("finding", "kosmos generalizes well", evidence_ids=[eid])
        self.assertEqual(len(self.wm.search("kosmos generalizes")), 1)
        # A phrase that does not exist verbatim matches nothing.
        self.assertEqual(self.wm.search('"generalizes kosmos"'), [])

    def test_search_no_match(self) -> None:
        self.assertEqual(self.wm.search("nothing recorded"), [])


class ContradictionTests(StoreTestCase):
    def test_refutes_edge_creates_a_contradiction(self) -> None:
        eid = self.wm.add_evidence("test_run", "pytest -q")
        holds = self.wm.add_claim("finding", "wal holds", status="supported", evidence_ids=[eid])
        breaks = self.wm.add_claim("finding", "wal breaks")
        self.wm.add_edge(breaks, holds, "refutes", note="NFS replay")
        rows = self.wm.contradictions()
        self.assertEqual(len(rows), 1)
        self.assertEqual((rows[0]["src_id"], rows[0]["dst_id"]), (breaks, holds))
        self.assertEqual(rows[0]["note"], "NFS replay")

    def test_unrefuted_excludes_refuted_and_targeted_claims(self) -> None:
        eid = self.wm.add_evidence("test_run", "pytest -q")
        holds = self.wm.add_claim("finding", "wal holds", status="supported", evidence_ids=[eid])
        breaks = self.wm.add_claim("finding", "wal breaks")
        refuted = self.wm.add_claim("finding", "wal refuted")
        self.wm.set_claim_status(refuted, "refuted")
        self.wm.add_edge(breaks, holds, "refutes")
        ids = {row["id"] for row in self.wm.unrefuted()}
        self.assertEqual(ids, {breaks})

    def test_supports_edge_does_not_exclude(self) -> None:
        eid = self.wm.add_evidence("test_run", "pytest -q")
        holds = self.wm.add_claim("finding", "wal holds", status="supported", evidence_ids=[eid])
        extra = self.wm.add_claim("finding", "also holds")
        self.wm.add_edge(extra, holds, "supports")
        ids = {row["id"] for row in self.wm.unrefuted()}
        self.assertEqual(ids, {holds, extra})


class DigestAndExportTests(StoreTestCase):
    def test_digest_reports_counts_and_respects_max_chars(self) -> None:
        eid = self.wm.add_evidence("code_run", "scripts/bench.py")
        self.wm.add_hypothesis("H one")
        self.wm.add_claim("finding", "C one", status="supported", evidence_ids=[eid])
        self.wm.add_task("skeptic", "audit c1")
        digest = self.wm.digest()
        self.assertIn("Hypotheses: 1 open", digest)
        self.assertIn("Claims: 1 supported", digest)
        self.assertIn("Open hypotheses:", digest)
        self.assertIn("Supported claims (unrefuted):", digest)
        short = self.wm.digest(max_chars=50)
        self.assertLessEqual(len(short), 50)
        self.assertIn("[digest truncated", short)

    def test_export_markdown_covers_every_section(self) -> None:
        eid = self.wm.add_evidence("code_run", "scripts/bench.py", excerpt="p95=1.2ms")
        cid = self.wm.add_claim("finding", "fast", status="supported", evidence_ids=[eid])
        other = self.wm.add_claim("finding", "slow")
        self.wm.add_edge(other, cid, "refutes")
        tid = self.wm.add_task("skeptic", "audit")
        task = self.wm.claim_next()
        assert task is not None
        self.wm.complete_task(tid, result="ok", artifacts=["/tmp/log.txt"])
        text = self.wm.export_markdown()
        for section in ("# World Model", "## Hypotheses", "## Evidence", "## Claims",
                        "## Edges", "## Tasks"):
            self.assertIn(section, text)
        self.assertIn("- c2 --refutes--> c1", text)
        self.assertIn("- evidence: e1", text)
        self.assertIn("artifact: `/tmp/log.txt`", text)


class PathResolutionTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.agent_dir = Path(self._tmp.name) / "agent"
        self.agent_dir.mkdir()
        self.session_dir = Path(self._tmp.name) / "session-artifacts" / "s1"
        self.session_dir.mkdir(parents=True)
        saved = {name: os.environ.get(name) for name in _SCOPE_ENV}
        self.addCleanup(lambda: self._restore_env(saved))
        for name in _SCOPE_ENV:
            os.environ.pop(name, None)
        os.environ["PRIME_AGENT_CODING_AGENT_DIR"] = str(self.agent_dir)

    @staticmethod
    def _restore_env(saved: dict[str, str | None]) -> None:
        for name, value in saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value

    def test_active_workspace_marker_pins_the_store(self) -> None:
        (self.agent_dir / "active_workspace").write_text("research\n")
        self.assertEqual(
            default_db_path(),
            self.agent_dir / "workspaces" / "research" / "world_model.db",
        )

    def test_session_record_overrides_the_marker(self) -> None:
        (self.agent_dir / "active_workspace").write_text("research\n")
        (self.session_dir / "workspace").write_text("coding\n")
        os.environ["RLM_SESSION_DIR"] = str(self.session_dir)
        self.assertEqual(
            default_db_path(),
            self.agent_dir / "workspaces" / "coding" / "world_model.db",
        )

    def test_empty_session_record_means_session_local(self) -> None:
        (self.agent_dir / "active_workspace").write_text("research\n")
        (self.session_dir / "workspace").write_text("")
        os.environ["RLM_SESSION_DIR"] = str(self.session_dir)
        self.assertEqual(default_db_path(), self.session_dir / "world_model.db")

    def test_kernel_workspace_env_pins_the_store(self) -> None:
        harness_dir = self.agent_dir / "workspaces" / "inherited" / "harness"
        harness_dir.mkdir(parents=True)
        os.environ["RLM_WORKSPACE_HARNESS_STATE_DIR"] = str(harness_dir)
        self.assertEqual(
            default_db_path(),
            self.agent_dir / "workspaces" / "inherited" / "world_model.db",
        )

    def test_no_workspace_no_session_dir_raises(self) -> None:
        with self.assertRaises(RuntimeError):
            default_db_path()

    def test_invalid_marker_name_is_ignored(self) -> None:
        (self.agent_dir / "active_workspace").write_text("Not A Name!\n")
        os.environ["RLM_SESSION_DIR"] = str(self.session_dir)
        self.assertEqual(default_db_path(), self.session_dir / "world_model.db")

    def test_resolve_workspace_reads_the_marker(self) -> None:
        (self.agent_dir / "active_workspace").write_text("research\n")
        self.assertEqual(resolve_workspace(), "research")


if __name__ == "__main__":
    unittest.main()
