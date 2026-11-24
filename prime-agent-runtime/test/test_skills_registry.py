from __future__ import annotations

import asyncio
import importlib
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch


rlm_module = importlib.import_module("rlm")

SKILL_ROW = {
    "name": "oleafly-latex-build",
    "kind": "markdown",
    "cluster": "oleafly",
    "description": "Fix LaTeX compile errors",
    "path": "/skills/oleafly-latex-build/SKILL.md",
}


class RlmSkillsRegistryTest(unittest.TestCase):
    def test_search_sends_the_query_and_parses_rows(self) -> None:
        host_request = AsyncMock(
            return_value={
                "skills": [
                    {**SKILL_ROW, "score": 1.25},
                    {
                        "name": "world-model",
                        "kind": "python",
                        "cluster": "python",
                        "description": "SQLite world model",
                        "path": "/skills/world-model/SKILL.md",
                        "python_import": "world_model",
                    },
                ]
            }
        )

        with patch.object(rlm_module, "host_request", host_request):
            skills = asyncio.run(rlm_module.rlm.skills.search("latex", limit=5))

        self.assertEqual(
            skills,
            [
                rlm_module.RLMSkill(
                    name="oleafly-latex-build",
                    kind="markdown",
                    cluster="oleafly",
                    description="Fix LaTeX compile errors",
                    path=Path("/skills/oleafly-latex-build/SKILL.md"),
                    score=1.25,
                ),
                rlm_module.RLMSkill(
                    name="world-model",
                    kind="python",
                    cluster="python",
                    description="SQLite world model",
                    path=Path("/skills/world-model/SKILL.md"),
                    python_import="world_model",
                ),
            ],
        )
        host_request.assert_awaited_once_with("rlm.skills.search", {"query": "latex", "limit": 5})

    def test_list_defaults_to_every_skill_and_forwards_a_cluster(self) -> None:
        host_request = AsyncMock(return_value={"skills": [SKILL_ROW]})

        with patch.object(rlm_module, "host_request", host_request):
            everything = asyncio.run(rlm_module.rlm.skills.list())
            in_cluster = asyncio.run(rlm_module.rlm.skills.list("oleafly"))

        self.assertEqual(everything, in_cluster)
        self.assertEqual(
            [call.args for call in host_request.await_args_list],
            [
                ("rlm.skills.list", {"cluster": None}),
                ("rlm.skills.list", {"cluster": "oleafly"}),
            ],
        )

    def test_read_returns_the_body_and_base_dir(self) -> None:
        host_request = AsyncMock(
            return_value={
                "name": "handoff",
                "path": "/skills/handoff/SKILL.md",
                "base_dir": "/skills/handoff",
                "body": "Write the note.",
            }
        )

        with patch.object(rlm_module, "host_request", host_request):
            doc = asyncio.run(rlm_module.rlm.skills.read("handoff"))

        self.assertEqual(
            doc,
            rlm_module.RLMSkillDoc(
                name="handoff",
                path=Path("/skills/handoff/SKILL.md"),
                base_dir=Path("/skills/handoff"),
                body="Write the note.",
            ),
        )
        host_request.assert_awaited_once_with("rlm.skills.read", {"name": "handoff"})

    def test_rejects_invalid_arguments_and_replies(self) -> None:
        with self.assertRaisesRegex(TypeError, "query must be str"):
            asyncio.run(rlm_module.rlm.skills.search(123))
        with self.assertRaisesRegex(TypeError, "limit must be int"):
            asyncio.run(rlm_module.rlm.skills.search("latex", limit="3"))
        with self.assertRaisesRegex(TypeError, "limit must be int"):
            asyncio.run(rlm_module.rlm.skills.search("latex", limit=True))
        with self.assertRaisesRegex(TypeError, "cluster must be str or None"):
            asyncio.run(rlm_module.rlm.skills.list(3))
        with self.assertRaisesRegex(TypeError, "name must be str"):
            asyncio.run(rlm_module.rlm.skills.read(None))

        with patch.object(rlm_module, "host_request", AsyncMock(return_value={"skills": [{"name": "x"}]})):
            with self.assertRaisesRegex(RuntimeError, "invalid skill entry"):
                asyncio.run(rlm_module.rlm.skills.search("x"))
        with patch.object(rlm_module, "host_request", AsyncMock(return_value={})):
            with self.assertRaisesRegex(RuntimeError, "invalid skills list"):
                asyncio.run(rlm_module.rlm.skills.list())
            with self.assertRaisesRegex(RuntimeError, "invalid skill"):
                asyncio.run(rlm_module.rlm.skills.read("x"))


if __name__ == "__main__":
    unittest.main()
