---
name: discovery-loop
description: Run cyclic discovery with parallel fresh-context workers over a shared SQLite world model - plan, execute, skeptic-audit, commit, then synthesize a provenance-cited report.
version: 1
author: Prime Agent
---

# discovery-loop

## What this machine does

Runs the generalized discovery loop over the bundled world-model skill: a
persistent SQLite store decoupled from any session context. Each cycle the
planner reads ONLY the compact world-model digest (never chat history),
emits 3-8 parallel tasks, fresh-context workers execute them and save raw
artifacts to disk, a skeptic checks every candidate claim against the raw
artifacts and sources, and the commit state writes the verified updates to
the store under its hard provenance gate. The guarded back-edge
`commit -> plan-cycle` repeats while `done` is false, bounded by
`max_entries` (5 planner cycles), `max_transitions`, and the run budget;
when the planner judges the objective met, `commit -> synthesize` compiles
the final deliverable, in which every claim cites the evidence rows that
support it.

The python states (`init`, `commit`) are the callables
`world_model.factory_states.init` / `.commit` from the bundled world-model
skill; every write path is idempotent, so an escalate pause and resume
never duplicates rows.

## Prerequisites

1. Pin a workspace first (`/workspace <name>`): the store lives at
   `<agent_dir>/workspaces/<name>/world_model.db`, shared across sessions.
   Without a pinned workspace the init state fails fast with the world
   model's scoping error instead of silently scattering the run.
2. Record the objective before starting, as an open hypothesis:

```python
from world_model import WorldModel
wm = WorldModel()
wm.add_hypothesis("Objective: <what this run must answer or build>")
```

The planner and the synthesizer orient from the store; nothing else
carries over.

## How to run it

    result = await rlm.factory.run("discovery-loop")

Then watch progress with `await rlm.factory.status(result["run_id"])`. The
run finishes at quiescence after `synthesize` settles; the report is the
synthesize state's latest settle answer, and the committed state of the
run is the world model itself - `WorldModel().digest()` or
`export_markdown()` reads it anytime, including from other sessions.

## The cycle

`init -> commit -> plan-cycle -> execute -> skeptic-audit -> commit -> ...`

- `init` *(python)*: opens the scoped store and renders the starting digest.
- `commit` *(python)*: validates provenance (a supported claim without
  linked evidence is downgraded to open and reported, never written),
  commits the skeptic's verified evidence, claims, and refutes edges
  idempotently, renders the next-cycle digest, and evaluates the stop
  condition from the planner's judgment.
- `plan-cycle` *(subagent)*: reads only the digest; emits the cycle's task
  list and `plan.done`. While `plan.done` is false the tasks fan out;
  when the planner judges the objective met the guarded exit skips the
  worker pass and returns to `commit` directly.
- `execute` *(foreach subagent, `run.max_parallel: 8` caps concurrency)*:
  one fresh-context worker per task writes raw artifacts to disk and
  returns candidate evidence, claims, and dead ends.
- `skeptic-audit` *(subagent)*: checks candidates against the raw
  artifacts and primary sources, downgrades leaps, flags contradictions
  against the digest's existing claims as refutes edges.
- `synthesize` *(subagent)*: compiles the final report, citing `[eNN]`
  evidence rows that exist in the store.

## Domain role profiles

The machine is domain-neutral; the planner picks `role` values per the
workspace's profile. Three worked profiles:

- **research**: `literature-scout` (find and verify sources for a
  hypothesis), `data-analyst` (compute numbers from datasets, save
  artifacts), `skeptic-auditor` (re-derive a claim's numbers from the raw
  data). Evidence kinds: `paper`, `web`, `dataset`.
- **coding/audit**: `codebase-explorer` (trace a hypothesis through the
  tree), `reproducer-benchmarker` (write and run a reproducer, save the
  log), `skeptic-reviewer` (re-run the reproducer against the saved
  artifact). Evidence kinds: `code_run`, `file_slice`, `test_run`.
- **news/diligence**: `source-gatherer` (collect primary sources),
  `quant-modeler` (compute the diligence figures), `red-teamer` (attack
  the emerging conclusion). Evidence kinds: `web`, `dataset`.

Store each profile's role prompts as workspace `subagent` harness entries
or prompt snippets; the planner inlines them into task instructions.

```machine-spec
{
  "run": {
    "budget_ms": 3600000,
    "failure_policy": "escalate",
    "max_parallel": 8,
    "max_transitions": 48
  },
  "states": [
    {
      "id": "init",
      "entry": true,
      "python": {
        "import": "world_model.factory_states",
        "callable": "init"
      },
      "outputs": [
        {
          "name": "digest",
          "type": "text"
        },
        {
          "name": "db_path",
          "type": "text"
        }
      ],
      "budget_ms": 120000
    },
    {
      "id": "commit",
      "python": {
        "import": "world_model.factory_states",
        "callable": "commit"
      },
      "inputs": [
        {
          "name": "db_path",
          "type": "text",
          "from": "init.db_path"
        },
        {
          "name": "plan",
          "type": "json",
          "from": "plan-cycle.plan",
          "optional": true
        },
        {
          "name": "candidates",
          "type": "json",
          "from": "skeptic-audit.candidates",
          "optional": true
        }
      ],
      "outputs": [
        {
          "name": "digest",
          "type": "text"
        },
        {
          "name": "done",
          "type": "json"
        },
        {
          "name": "report",
          "type": "json"
        }
      ],
      "max_entries": 6,
      "budget_ms": 120000
    },
    {
      "id": "plan-cycle",
      "subagent": {
        "prompt": "You are planning the next cycle of a discovery run. Read ONLY the world-model digest below - never chat history or source files - and emit a fenced json block of the form {\\\"plan\\\": {\\\"done\\\": <true|false>, \\\"reason\\\": \\\"one sentence\\\"}, \\\"tasks\\\": [{\\\"role\\\": \\\"<role>\\\", \\\"prompt\\\": \\\"one concrete, independently executable instruction\\\", \\\"hypothesis\\\": \\\"the hypothesis this task probes\\\"}]}. Emit 3-8 parallel tasks that most reduce uncertainty about the open hypotheses. Set plan.done true only when the digest shows every open hypothesis is resolved (supported or refuted) and no evidence gap remains worth a worker; then emit an empty tasks list. Output only the json block.\\n\\n{digest}",
        "name": "discovery-planner"
      },
      "inputs": [
        {
          "name": "digest",
          "type": "text",
          "from": "commit.digest"
        }
      ],
      "outputs": [
        {
          "name": "plan",
          "type": "json"
        },
        {
          "name": "tasks",
          "type": "json"
        }
      ],
      "max_entries": 5,
      "budget_ms": 240000
    },
    {
      "id": "execute",
      "subagent": {
        "prompt": "Execute exactly one discovery task with a fresh context and no chat history:\\n\\n{tasks}\\n\\nDo the work concretely - run code, read files, search sources - and save every raw artifact to disk under a path you report. Then reply with a fenced json block of the form {\\\"findings\\\": {\\\"evidence\\\": [{\\\"kind\\\": \\\"code_run|file_slice|test_run|paper|web|dataset\\\", \\\"ref\\\": \\\"the command, path, DOI or URL this came from\\\", \\\"excerpt\\\": \\\"short quote or number\\\", \\\"artifact_path\\\": \\\"saved artifact path or null\\\"}], \\\"claims\\\": [{\\\"kind\\\": \\\"finding|decision|dead_end\\\", \\\"statement\\\": \\\"...\\\", \\\"status\\\": \\\"open|supported\\\", \\\"confidence\\\": 0.0-1.0, \\\"evidence\\\": [0]}]}, \\\"dead_ends\\\": [\\\"one sentence per dead end\\\"]} where each claims evidence field lists indices into your own evidence array. Mark a claim supported only when one of your evidence rows actually shows it. Output only the json block.",
        "name": "discovery-worker"
      },
      "inputs": [
        {
          "name": "tasks",
          "type": "json",
          "from": "plan-cycle.tasks"
        }
      ],
      "outputs": [
        {
          "name": "findings",
          "type": "text"
        }
      ],
      "foreach": {
        "over": "tasks",
        "max": 8
      },
      "max_entries": 5,
      "budget_ms": 480000
    },
    {
      "id": "skeptic-audit",
      "subagent": {
        "prompt": "You are the skeptic auditor for one discovery cycle. Check every candidate claim in the worker findings below directly against the raw artifacts and primary sources: open each artifact_path, re-run what is cheap, and compare excerpts. Build the verified update set: keep evidence that is real; keep a claim only when its cited evidence actually supports it, and rewrite any leap to status \\\"open\\\"; flag contradictions between new claims and the existing claims [cNN] in the digest as refutes entries. Reply with a fenced json block of the form {\\\"candidates\\\": {\\\"evidence\\\": [...same shape as the workers emitted...], \\\"claims\\\": [{\\\"kind\\\": \\\"...\\\", \\\"statement\\\": \\\"...\\\", \\\"status\\\": \\\"open|supported|refuted\\\", \\\"confidence\\\": 0.0-1.0, \\\"evidence\\\": [0]}], \\\"refutes\\\": [{\\\"src\\\": <existing claim id>, \\\"dst\\\": <existing claim id>, \\\"note\\\": \\\"why src refutes dst\\\"}]}} where claims.evidence lists indices into this candidates.evidence array and refutes reference claim ids from the digest. Output only the json block.\\n\\n## Worker findings\\n\\n{findings}\\n\\n## Planned tasks\\n\\n{tasks}\\n\\n## World model digest\\n\\n{digest}",
        "name": "discovery-skeptic"
      },
      "inputs": [
        {
          "name": "findings",
          "type": "text",
          "from": "execute.findings"
        },
        {
          "name": "tasks",
          "type": "json",
          "from": "plan-cycle.tasks"
        },
        {
          "name": "digest",
          "type": "text",
          "from": "commit.digest"
        }
      ],
      "outputs": [
        {
          "name": "candidates",
          "type": "json"
        }
      ],
      "max_entries": 5,
      "budget_ms": 480000
    },
    {
      "id": "synthesize",
      "subagent": {
        "prompt": "Compile the final deliverable of this discovery run. The world model lives at {db_path}: open it from your Python kernel (from world_model import WorldModel, then WorldModel(db_path)) and read wm.unrefuted(), wm.contradictions(), and wm.export_markdown(). Write the markdown report that answers the run objective: lead with the conclusion, then the supporting findings. Every claim in the report must cite the evidence rows that support it as [eNN] markers, and every marker must name a real evidence row in the store - never state a claim the store does not support. The digest below orients you:\\n\\n{digest}",
        "name": "discovery-synthesizer"
      },
      "inputs": [
        {
          "name": "digest",
          "type": "text",
          "from": "commit.digest"
        },
        {
          "name": "db_path",
          "type": "text",
          "from": "init.db_path"
        }
      ],
      "outputs": [
        {
          "name": "report",
          "type": "text"
        }
      ],
      "budget_ms": 480000
    }
  ],
  "transitions": [
    {
      "from": "init",
      "to": "commit"
    },
    {
      "from": "commit",
      "to": "plan-cycle",
      "when": {
        "output": "done",
        "op": "eq",
        "value": false
      }
    },
    {
      "from": "commit",
      "to": "synthesize",
      "when": {
        "output": "done",
        "op": "eq",
        "value": true
      }
    },
    {
      "from": "plan-cycle",
      "to": "execute",
      "when": {
        "output": "plan",
        "path": "done",
        "op": "eq",
        "value": false
      }
    },
    {
      "from": "plan-cycle",
      "to": "commit",
      "when": {
        "output": "plan",
        "path": "done",
        "op": "eq",
        "value": true
      }
    },
    {
      "from": "execute",
      "to": "skeptic-audit"
    },
    {
      "from": "skeptic-audit",
      "to": "commit"
    }
  ]
}
```
