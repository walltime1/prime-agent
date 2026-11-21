---
name: world-model
description: Persistent SQLite world model shared across agent processes (WAL mode) - hypotheses, immutable evidence, provenance-gated claims, claim relations, and tasks with atomic claim_next(). Use when a plan asks to track hypotheses, evidence, claims, or tasks in a shared world model, to keep structured state across compactions or parallel worker sessions, or to coordinate task claiming between parent and spawned agents.
---

# World Model

A dependency-light, multi-process SQLite store for long-horizon work. Parallel
worker sessions commit provenance-backed updates (hypotheses, evidence, claims,
relations, tasks) to one database; the parent reads a compact `digest()` instead
of replaying chat history. WAL mode plus `BEGIN IMMEDIATE` lets parent and
spawned kernels share the file safely with no lock failures.

## Where the store lives

`WorldModel(path=None)` resolves the default database path:

1. An explicit `path` argument always wins.
2. A pinned workspace resolves to `<agent_dir>/workspaces/<name>/world_model.db`
   (cross-session persistence). The session's own workspace record wins, then
   the kernel's resolved workspace env, then `<agent_dir>/active_workspace`.
3. Without a workspace the store is session-local:
   `<session_artifact_dir>/world_model.db` (isolated per run).

Pass an explicit `path` for scratch or experiment-local stores.

## Usage

Call the prepared `world_model` import directly in the Python kernel:

```python
from world_model import WorldModel

wm = WorldModel()                       # default scope, or WorldModel(path=...)
h = wm.add_hypothesis("SQLite WAL handles our write volume")
e = wm.add_evidence("test_run", "pytest -q", excerpt="8 processes, 0 lock failures")
c = wm.add_claim("finding", "WAL writers never fail under contention",
                 status="supported", evidence_ids=[e])
wm.add_task("skeptic", f"audit claim c{c} against the raw log", cycle=1)
task = wm.claim_next()                  # atomically claims one pending task, or None
wm.complete_task(task["id"], result="no unsupported leaps", artifacts=["/tmp/log.txt"])

print(wm.digest(max_chars=2000))        # compact planner summary
print(wm.search("lock contention"))     # FTS5 over claims and evidence
print(wm.contradictions())              # claim pairs joined by refutes edges
print(wm.unrefuted())                   # claims not marked refuted, no refutes edge
print(wm.export_markdown())             # full document for reading or reports
```

Workers spawned with `rlm.spawn` open the same store from their own kernels:
have them `add_evidence` first, then `add_claim(status="supported",
evidence_ids=[...])`, and never write prose summaries into the shared database.

## Invariants

- **Provenance gate (hard):** `add_claim(..., status="supported")` — and
  `set_claim_status(claim_id, "supported")` — is rejected unless at least one
  already-existing `evidence_id` is attached. Record evidence first, or keep
  the claim `open` and attach evidence later.
- **Evidence is immutable:** it records what was actually observed; there is no
  update or delete path. Reference raw artifacts on disk through
  `artifact_path`.
- **Task claiming is atomic:** `claim_next()` runs under `BEGIN IMMEDIATE`, so
  concurrent processes never claim the same task; `blocked_by` task ids hold a
  task back until they reach `completed` or `cancelled`.

## Discovery loop

The bundled `discovery-loop` factory machine runs cyclic discovery over this
store: plan from the digest, execute tasks in parallel, skeptic-audit, then
commit — its `init` and `commit` states are this package's
`world_model.factory_states` callables. See the machine's `MACHINE.md` for
prerequisites (pin a workspace, record the objective first) and the domain
role profiles.

## Storage details

Stdlib `sqlite3` only; every connection sets `journal_mode=WAL`,
`busy_timeout=5000`, and `foreign_keys=ON`. Tables: `hypotheses`, `evidence`,
`claims`, `claim_evidence`, `edges` (claim-to-claim supports/refutes/
supersedes/derived_from), `tasks`. Claims and evidence are indexed with FTS5
(`search()` matches rows containing all query words; explicit FTS5 syntax is
passed through).
