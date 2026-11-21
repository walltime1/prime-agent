# Prime Agent Fork — User FAQ

A practical reference for how **Workspaces**, **Agent Blueprints**, **Sessions**, **Teams**, and the **SQLite World Model / Discovery Loop** work together.

---

## 1. Mental Model: Workspaces vs. Blueprints vs. Sessions

### Q: What is the difference between a Workspace, an Agent Blueprint, and a Session?

| Concept | What it is | Where it lives | Lifespan |
|---|---|---|---|
| **Workspace** (`news`, `research`) | Shared **domain brain**: memories, prompt rules, subagent specs, factory machines, and `world_model.db` | `~/.prime/agent/workspaces/<name>/` | Permanent across sessions |
| **Blueprint** (`trader`, `researcher`) | Reusable **launcher**: default `workspace`, `cwd`, `model`, `thinking`, extra `skills` paths, role instructions, and an optional `machine` prompt directive | `~/.prime/agent/agents/<name>.md` (or `<repo>/.prime/agent/agents/<name>.md`) | Permanent template |
| **Session** | One **chat & REPL instance** | `~/.prime/agent/sessions/` | Temporary run *or* starred standing agent |

### Q: When should I start a new session vs. reuse an old session?
- **Start a NEW session** (`prime-agent --workspace news` or `prime-agent --agent trader`) whenever you start a **new task, question, or investigation**. It automatically loads your workspace's promoted memories/rules and World Model without dragging old chat history or stale REPL variables.
- **Reuse an existing session** only when the **live ongoing thread itself** is what matters (e.g., an active multi-day goal, running heartbeats, or live in-memory REPL objects). Star those sessions (`Ctrl+P`) so you can filter to them (`Ctrl+S`).

---

## 2. Managing Sessions (Star, Archive, Retire)

### Q: How do I keep the Agents View (`prime-agent agents` / `prime-agent -r`) clean?

| Key | Action |
|---|---|
| `Ctrl+P` | **Star / unstar** the selected top-level session (`★`) — use for standing agents you return to |
| `Ctrl+S` | **Filter to starred** sessions only (press again to show all) |
| `Ctrl+A` | **Archive / unarchive** a saved top-level session (hides it and all its subagents without deleting history or cost) |
| `Ctrl+E` | **Show / hide archived** sessions (non-running archived rows show `○`; running ones keep their live pulse icon) |
| `Ctrl+F` *(or `Alt+W`)* | **Filter to the active workspace** |
| `Ctrl+R` | **Rename** the selected session |
| `Ctrl+X` *(twice)* | **Stop** a running session or **permanently delete** a saved session |

*Notes on archiving:*
- `Ctrl+A` only works on **saved top-level** session rows (not unsaved `--no-session` runs or child subagent rows, and the archive filter is not applied while drilled into a session's subagent scope).
- To **unarchive** a hidden session, press `Ctrl+E` first to reveal archived rows, select it, and press `Ctrl+A`.
- The archive keys are not shown in the 120-column bottom hint bar (to avoid crowding out `Ctrl+X stop`), but appear on the status line when toggled.

### Q: How do I cleanly finish ("deprecate") a session after a good run?
Inside the chat (once the current turn/work has finished), run:
```text
/retire
```
This does two things:
1. Submits **`/refine --global`** to extract reusable lessons from the session into your session's pinned workspace (or the global store if unpinned).
2. Immediately **archives** the session (`~/.prime/agent/archived-sessions.json`) at submit time so it leaves your default Agents View list while keeping its transcript and cost logs on disk. *(If the refinement fails or you want it back, press `Ctrl+E` then `Ctrl+A` in Agents View to unarchive it. Unsaved sessions cannot be retired.)*

---

## 3. Workspaces & Promoting Knowledge

### Q: How do I use and inspect workspaces?
```text
/workspace                  # Show active workspace, default for new sessions, and all workspaces
/workspace news             # Switch to (or create) workspace 'news'
/workspace info             # List 'news'-specific harness entries, 90-day (review due) flags, and world_model.db byte size
/workspace off              # Clear active workspace (use global store directly)
/workspace rename old new   # Rename a workspace
/workspace delete old       # Delete a workspace
```
*(Note: `info` and `promote` are subcommands, so avoid naming a workspace `info` or `promote`.)*

Or launch directly into a workspace from your shell:
```bash
prime-agent --workspace news
```

### Q: How does workspace layering work?
When a session is pinned to workspace `news`:
- **Reads** see **`news` layered over `global`** (if both have an entry with the same `kind` and `id`, `news` wins).
- **Agent writes** (`rlm.harness.*`, `refine.run()`) always go to the **session-local** store so the agent can't pollute shared state on its own.
- **User `/refine --global`** writes to the **`news` workspace store** (leaving `global` untouched).

### Q: Something I refined in `news` is useful everywhere. How do I promote it to Global?
1. Inside a session pinned to `news`, run `/workspace info` to see the workspace's own entries (`- <kind> <id>: <title>`) and find the entry's `<id>`.
2. Run:
   ```text
   /workspace promote <id>
   ```
   *(Both `/workspace info` and `/workspace promote` run as session commands, so their output is added to the transcript and visible in model context.)*

What `/workspace promote <id>` does:
- Writes the entry into the global store first (replacing any existing global entry with the same `kind` + `id`, and giving it a fresh 90-day review date so `(review due)` clears), then deletes the shadowing copy from `news`.
- Errors if `<id>` exists under multiple kinds in `news` (rename one first), or if promoting a `factory` entry while `/factory on` is disabled.
- **Rolling back a promotion:** Promotion records **two** refinement events — a global create/update in `~/.prime/agent/harness/refinement_history.jsonl` and a workspace delete in `~/.prime/agent/workspaces/<name>/harness/refinement_history.jsonl`:
  - To restore the workspace copy, run `/refine --global rollback <workspace-refinement-id>` from a session **pinned to that workspace**.
  - To undo the global write, switch to an **unpinned** session first (`/workspace off`) and run `/refine --global rollback <global-refinement-id>`.

### Q: What is `(review due)` in `/workspace info`?
Every rule written by refinement gets a **90-day review date**. Once it passes 90 days, `/workspace info` marks it `(review due)` and the ranker demotes it behind fresh rules — reminding you to either **keep/update** it, **promote** it to global (which refreshes its 90-day timer), or **delete** it.

---

## 4. Agent Blueprints (`agents/<name>.md`)

### Q: How do I turn a session I've dialed in (like `Trader`) into a reusable agent?
Inside that session, run:
```text
/agent save trader
```
(Add `--force` before or after the name to overwrite an existing `trader.md`.)
- Always writes to the global `~/.prime/agent/agents/trader.md`.
- Captures the session's pinned `workspace` (if any), absolute `cwd`, `model` (`provider/id`), `thinking`, and the session's **appended system instructions** (`APPEND_SYSTEM.md` + `--append-system-prompt` + any prior blueprint body) — **not** chat history, `AGENTS.md`, or refined memories. *(If `APPEND_SYSTEM.md` was active in that directory, its text is included in the saved body; trim it if you keep `APPEND_SYSTEM.md` on disk so it isn't appended twice on relaunch.)*

You can also create or edit `~/.prime/agent/agents/trader.md` (or `<repo>/.prime/agent/agents/trader.md`, which overrides the global one of the same filename) in any editor:
```markdown
---
name: trader
description: News-driven market analyst and thesis tracker
workspace: news
cwd: ~/Stuff/News
model: z-ai/glm-5.3
thinking: max
machine: discovery-loop
---

You are Trader. Monitor incoming news in `~/Stuff/News`, log verified evidence
and market claims in `WorldModel()`, and challenge weak theses before acting.
```

*Frontmatter field notes:*
- **Lookup is by filename** (`<name>.md`, allowing ASCII letters, digits, `-`, `_`, `.`, no leading dot); the `name:` field is display metadata.
- **`model:`** use `provider/id` (or `inherit` to leave unset).
- **`workspace:`** must be a valid workspace name — omit the key to use global state (`off` or `global` is rejected in blueprints). Setting `workspace:` in a blueprint pins that session without changing your global default-workspace pointer.
- **`cwd:`** changes the process working directory (`~` is expanded). Note that `--agent` searches `<current-dir or --cwd>/.prime/agent/agents/` and `~/.prime/agent/agents/` *before* switching to the blueprint's `cwd:`.
- **`skills:`** optional list (or comma-separated string) of **extra skill file/directory paths** (like `--skill`, resolved relative to the session's `cwd`). Bundled skills (`websearch`, `world-model`, `edit`, etc.) already load automatically, so do **not** list bare bundled skill names under `skills:`.
- **`machine:`** does **not** auto-launch a factory run on startup; it appends `Operating machine: this agent's work runs through the factory machine "<machine>" (await rlm.factory.run("<machine>")).` to the system prompt (you still need `/factory on`).

### Q: How do I list or launch blueprints?
- Inside chat: `/agent list`
- From terminal (for new sessions):
  ```bash
  prime-agent --agent trader
  ```
  - Explicit `--workspace`, `--cwd`, `--model`, and `--thinking` CLI flags **override** the blueprint's defaults.
  - Explicit `--skill` and `--append-system-prompt` flags **add to** the blueprint's values.
  - Avoid combining `--agent` with `--resume` / `--continue`, as the blueprint's `workspace:` will re-pin the resumed session.

---

## 5. Teams, World Model, & Generalized Kosmos Loop

### Q: Which multi-agent mode should I use for a task?

| Scenario | What to use |
|---|---|
| **Quick 2–4 role collaboration** (1 turn/session) | **Ad-hoc sibling team (`rlm.spawn`):** ask the lead to spawn named siblings using one of the 3 core prompt patterns:<br>1. **Breadth fan-out + synthesizer** (parallel scouts on disjoint angles → one synthesizer)<br>2. **Rival hypotheses / red-team** (advocate vs. skeptic messaging directly via `agent_message.send(..., receiver_role="sibling", receiver_name="<teammate>")`)<br>3. **Disk-artifact fan-in** (workers write full outputs to files and return short summaries with paths) |
| **Persistent facts, hypotheses, citations & task queue across sessions** | **`world-model` skill** (`from world_model import WorldModel`) — backed by SQLite WAL at `~/.prime/agent/workspaces/<name>/world_model.db`. |
| **Deep multi-cycle autonomous research, codebase audit, or due diligence** (up to 5 cycles × 8 parallel workers ≈ 40 rollouts + skeptic verification) | **`discovery-loop` factory machine** (`/factory on` + `await rlm.factory.run("discovery-loop")`). |

### Q: How do I use the World Model directly in a session?
You can ask the agent in plain English (*"track our hypotheses, evidence, and claims in the world model"*), or call it in the REPL:
```python
from world_model import WorldModel

wm = WorldModel()  # uses <agent_dir>/workspaces/<active>/world_model.db (or session-local if unpinned)
h1 = wm.add_hypothesis("Rate cuts will compress bank NIMs in Q4")
e1 = wm.add_evidence("paper", "https://...", excerpt="NIM fell 18 bps...")
c1 = wm.add_claim("finding", "Q4 NIM compression averages 15-20 bps",
                  status="supported", evidence_ids=[e1])  # hard gate: requires e1!

print(wm.digest())           # compact summary for planning
print(wm.search("NIM Q4"))   # FTS5 token-AND search across claims and evidence
print(wm.contradictions())   # claim pairs linked by 'refutes' edges
print(wm.unrefuted())        # supported/open claims with no refutations
print(wm.export_markdown())  # full citation-backed report
```
*(In `wm.search(...)`, a plain multi-word query is an `AND` over tokens — matching rows that contain every word, not an exact phrase — while explicit FTS5 syntax such as quotes, `AND`/`OR`/`NOT`/`NEAR`, `*`, or column filters passes through verbatim.)*

### Q: How do I run the generalized Kosmos `discovery-loop`?
1. Pin a workspace if you want findings to persist across sessions (`/workspace research` or `--workspace research`). *(Without a pinned workspace, `WorldModel()` inside a session kernel falls back to `<session_artifact_dir>/world_model.db` via `RLM_SESSION_DIR`, writing a session-local DB that other sessions won't see.)*
2. Turn on the factory gate once:
   ```text
   /factory on
   ```
3. Seed your objective as an open hypothesis and start `discovery-loop` (or ask the agent to do it):
   ```python
   from world_model import WorldModel
   wm = WorldModel()
   wm.add_hypothesis("Objective: Determine why X happens and compare A vs B")
   run = await rlm.factory.run("discovery-loop")
   ```
4. What happens automatically each cycle:
   - **`init` → `commit`:** opens `world_model.db` and renders a compact digest.
   - **`plan-cycle`:** reads *only* the digest (never raw chat history) and plans 3–8 parallel tasks (or sets `plan.done = true` to skip straight to `commit` when open hypotheses are resolved).
   - **`execute` (`foreach`):** spawns fresh-context workers (up to 8 in parallel) to run code/searches and write raw artifacts to disk.
   - **`skeptic-audit`:** checks every worker claim against the raw artifacts/sources, downgrades unsupported leaps, and adds `refutes` edges for contradictions.
   - **`commit`:** enforces the provenance gate (`supported` claims without evidence are downgraded to `open`), writes idempotently to SQLite, and loops back to `plan-cycle` (up to 5 planner cycles) until `done == true`.
   - **`synthesize`:** compiles the final report citing `[eNN]` evidence IDs from the store.
