//! The runtime bootstrap code injected into each kernel right after start or
//! restore: binds the `rlm`, `bash`, and MCP surfaces and binds every Python
//! skill behind a lazy import proxy.

use super::KernelPythonSkill;

/// Line the runtime bootstrap prints when one or more pre-imported Python skills failed to import.
/// The host scans the bootstrap cell's stdout for this marker.
pub const PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER: &str =
    "__PRIME_AGENT_PYTHON_SKILL_IMPORT_ERRORS__";

/// Failed skill imports reported by a bootstrap cell: `(import name, import error)` pairs in import
/// order (TS `UnavailablePythonSkills = Record<string, string>`).
pub type UnavailablePythonSkills = Vec<(String, String)>;

/// Extract the unavailable-skill report a bootstrap cell printed, or `None` when it printed none:
/// the marker must be followed by a JSON object with at least one non-empty string value.
#[must_use]
pub fn parse_unavailable_python_skills(stdout: &str) -> Option<UnavailablePythonSkills> {
    let at = stdout.find(PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER)?;
    let raw = stdout[at + PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER.len()..].trim();
    let parsed = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw).ok()?;
    let errors = parsed
        .into_iter()
        .filter_map(|(name, error)| {
            let error = error.as_str()?;
            (!error.is_empty()).then(|| (name, error.to_string()))
        })
        .collect::<UnavailablePythonSkills>();
    (!errors.is_empty()).then_some(errors)
}

const RLM_BOOTSTRAP_HEADER_CODE: &str =
    "import asyncio\nimport os as _prime_agent_os\n\n_prime_agent_os.environ[\"NO_COLOR\"] = \"1\"";

const RLM_BOOTSTRAP_RUNTIME_HEAD_CODE: &str = "
try:
    import rlm as _prime_agent_rlm_module
    rlm = _prime_agent_rlm_module.rlm
    bash = _prime_agent_rlm_module.bash
    import rlm.mcp as mcp
";

/// One line inside the bootstrap's try block, emitted only when the host
/// registered the [`crate::kernel::shared::BASH_COMPLETED_REQUEST_TYPE`]
/// handler: turns the runtime's bounded `await bash()` on. Without
/// the notice nothing wakes a cell that ended its turn on a degraded command,
/// so daemonless sessions keep the unbounded await.
const RLM_BOOTSTRAP_BASH_NOTICES_LINE_CODE: &str =
    "    _prime_agent_rlm_module._set_completion_notices_wired(True)\n";

const RLM_BOOTSTRAP_RUNTIME_TAIL_CODE: &str = r#"
except Exception as _prime_agent_rlm_error:
    _PRIME_AGENT_RLM_IMPORT_ERROR = str(_prime_agent_rlm_error)

    class _PrimeAgentMissingRlm:
        def _raise_missing(self):
            raise RuntimeError(
                "prime-agent-runtime is not installed in this kernel. "
                "Remove ~/.prime/agent/kernel-venv so prime-agent can rebuild it, or set "
                "PRIME_AGENT_KERNEL_PYTHON to a kernel environment with prime-agent-runtime installed. "
                f"Import error: {_PRIME_AGENT_RLM_IMPORT_ERROR}"
            )

        async def spawn(self, prompt, **kwargs):
            self._raise_missing()

        async def find_models(self, query="", limit=8):
            self._raise_missing()

        async def create_session(self, prompt, **kwargs):
            self._raise_missing()

        async def list_subagents(self):
            self._raise_missing()

        async def delete_subagent(self, target):
            self._raise_missing()

    rlm = _PrimeAgentMissingRlm()

    def bash(command, persistent=False):
        rlm._raise_missing()
"#;

/// The code the session injects right after kernel start/restore: binds the `rlm`, `bash`, and MCP
/// surfaces, then binds every Python skill behind a lazy import proxy. `bash_notices_wired` emits
/// the bounded-await toggle only when the host registers the bash completion notice.
///
/// Skill imports are lazy. The host reads the failure report from this cell's stdout right after it
/// runs (see [`parse_unavailable_python_skills`]), so availability is checked here with
/// `importlib.util.find_spec`, which resolves a module's locations without executing it: no skill
/// module runs at bootstrap (or in any kernel that never touches it), but a missing skill is still
/// reported before the model can reach it, with the same `No module named '...'` text an eager
/// import produced.
///
/// Each importable skill gets an `_PrimeAgentLazySkill` bound under its import name in `__main__`.
/// The first attribute access, call, or `dir()` imports the module, wraps it with
/// `_PrimeAgentCallableSkillModule` (so `await skill(...)` still reaches `run`), and replaces the
/// binding in `__main__` and `sys.modules` with the real module, so every later access is the plain
/// module. A skill that disappears between the spec check and its first use fails then, and raises
/// the same `RuntimeError` an unavailable skill raises.
#[must_use]
pub fn build_rlm_bootstrap_code(
    python_skills: &[KernelPythonSkill],
    bash_notices_wired: bool,
) -> String {
    let mut runtime_code = String::from(RLM_BOOTSTRAP_RUNTIME_HEAD_CODE);
    if bash_notices_wired {
        runtime_code.push_str(RLM_BOOTSTRAP_BASH_NOTICES_LINE_CODE);
    }
    runtime_code.push_str(RLM_BOOTSTRAP_RUNTIME_TAIL_CODE);
    let base_code = format!("{RLM_BOOTSTRAP_HEADER_CODE}\n\n{runtime_code}");
    // TS: `[...new Set(pythonSkills.map(...))]` — first-seen order, so the pre-import loop follows
    // the session's skill discovery order.
    let mut import_names: Vec<&str> = Vec::new();
    for skill in python_skills {
        let import_name = skill.import_name.as_str();
        if !import_names.contains(&import_name) {
            import_names.push(import_name);
        }
    }
    if import_names.is_empty() {
        return base_code;
    }
    let imports_json = serde_json::to_string(&import_names).unwrap_or_else(|_| "[]".to_string());
    format!(
        r#"
{base_code}

import importlib as _prime_agent_importlib
import importlib.util as _prime_agent_importlib_util
import inspect as _prime_agent_inspect
import sys as _prime_agent_sys
import types as _prime_agent_types

class _PrimeAgentCallableSkillModule(_prime_agent_types.ModuleType):
    async def __call__(self, *args, **kwargs):
        result = self.run(*args, **kwargs)
        if _prime_agent_inspect.isawaitable(result):
            return await result
        return result

class _PrimeAgentLazySkill:
    # One Python skill's binding in __main__ until its first touch: cheap to bind, imports on
    # demand, then swaps itself for the real (wrapped) module so later accesses pay nothing.
    # The state snapshot leaves it out: pickling it would import the skill (dill probes an
    # instance dict carrying __name__ as a module's), and the bootstrap rebinds it on revive.
    _prime_agent_snapshot_skip = True

    def __init__(self, import_module, failure):
        self.__name__ = import_module
        self._prime_agent_failure = failure
        self.__doc__ = f"Python skill {{import_module}} bound lazily; it imports on first access."

    def _prime_agent_resolve(self):
        failure = self._prime_agent_failure
        if failure is None:
            try:
                module = _prime_agent_wrap_skill_module(
                    _prime_agent_importlib.import_module(self.__name__)
                )
            except Exception as _prime_agent_skill_error:
                failure = str(_prime_agent_skill_error) or type(_prime_agent_skill_error).__name__
                self._prime_agent_failure = failure
            else:
                # The kernel namespace is reached through sys.modules, not globals(): inside a
                # method body globals() is this module's dict, not __main__.
                _prime_agent_sys.modules["__main__"].__dict__[self.__name__] = module
                return module
        raise RuntimeError(
            f"Python skill {{self.__name__}} is unavailable in this kernel. Import error: {{failure}}"
        )

    async def run(self, *args, **kwargs):
        module = self._prime_agent_resolve()
        run = getattr(module, "run", None)
        if not callable(run):
            raise AttributeError(
                f"Python skill {{self.__name__}} has no run(); access its module attributes directly."
            )
        result = run(*args, **kwargs)
        if _prime_agent_inspect.isawaitable(result):
            return await result
        return result

    async def __call__(self, *args, **kwargs):
        return await self.run(*args, **kwargs)

    def __getattr__(self, name):
        # Dunder probes (inspect.isawaitable, copy/pickle, the debugger) must not import, and an
        # instance built without __init__ (copy.copy) must not recurse through its own fields.
        if (name.startswith("__") and name.endswith("__")) or name.startswith("_prime_agent_"):
            raise AttributeError(name)
        return getattr(self._prime_agent_resolve(), name)

    def __dir__(self):
        if self._prime_agent_failure is not None:
            return [name for name in dir(type(self)) if not name.startswith("_prime_agent_")]
        return dir(self._prime_agent_resolve())

    def __repr__(self):
        if self._prime_agent_failure is not None:
            return f"<unavailable Python skill {{self.__name__!r}}: {{self._prime_agent_failure}}>"
        return f"<lazy Python skill {{self.__name__!r}}: imports on first access>"

class _PrimeAgentUnavailableSkill:
    # Same reason as _PrimeAgentLazySkill: the bootstrap rebinds unavailable placeholders on revive.
    _prime_agent_snapshot_skip = True

    def __init__(self, name, error):
        self.__name__ = name
        self._prime_agent_import_error = error
        self.__doc__ = f"Python skill {{name}} is unavailable: {{error}}"

    async def run(self, *args, **kwargs):
        raise RuntimeError(
            f"Python skill {{self.__name__}} is unavailable in this kernel. "
            f"Import error: {{self._prime_agent_import_error}}"
        )

    async def __call__(self, *args, **kwargs):
        return await self.run()

    def __repr__(self):
        return f"<unavailable Python skill {{self.__name__!r}}: {{self._prime_agent_import_error}}>"

def _prime_agent_wrap_skill_module(module):
    run = getattr(module, "run", None)
    if not callable(run):
        return module
    if isinstance(module, _PrimeAgentCallableSkillModule):
        return module
    wrapped = _PrimeAgentCallableSkillModule(module.__name__)
    wrapped.__dict__.update(module.__dict__)
    try:
        wrapped.__signature__ = _prime_agent_inspect.signature(run)
    except Exception:
        pass
    doc = getattr(run, "__doc__", None)
    if doc:
        wrapped.__doc__ = doc
    _prime_agent_sys.modules[module.__name__] = wrapped
    return wrapped

_PRIME_AGENT_SKILL_IMPORT_ERRORS = {{}}
_prime_agent_main = _prime_agent_sys.modules["__main__"].__dict__

for _prime_agent_skill_name in {imports_json}:
    try:
        if _prime_agent_importlib.util.find_spec(_prime_agent_skill_name) is None:
            raise ModuleNotFoundError(f"No module named {{_prime_agent_skill_name!r}}")
    except Exception as _prime_agent_skill_error:
        # An empty exception message would otherwise be dropped by the
        # host-side parser; fall back to the exception type name.
        _prime_agent_skill_error_text = (
            str(_prime_agent_skill_error) or type(_prime_agent_skill_error).__name__
        )
        _PRIME_AGENT_SKILL_IMPORT_ERRORS[_prime_agent_skill_name] = _prime_agent_skill_error_text
        _prime_agent_main[_prime_agent_skill_name] = _PrimeAgentUnavailableSkill(
            _prime_agent_skill_name,
            _prime_agent_skill_error_text,
        )
    else:
        _prime_agent_main[_prime_agent_skill_name] = _PrimeAgentLazySkill(_prime_agent_skill_name, None)

if _PRIME_AGENT_SKILL_IMPORT_ERRORS:
    import json as _prime_agent_json
    print(
        "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}"
        + _prime_agent_json.dumps(_PRIME_AGENT_SKILL_IMPORT_ERRORS)
    )

"#
    )
    .trim()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bootstrap_code_without_skills_binds_rlm() {
        let code = build_rlm_bootstrap_code(&[], false);
        assert!(code.contains("import rlm as _prime_agent_rlm_module"));
        assert!(!code.contains("_PrimeAgentUnavailableSkill"));
    }

    #[test]
    fn bootstrap_turns_the_bounded_bash_default_on_only_with_notices() {
        let daemonless = build_rlm_bootstrap_code(&[], false);
        let noticed = build_rlm_bootstrap_code(&[], true);
        assert!(
            noticed.contains("_set_completion_notices_wired(True)"),
            "the wired bootstrap must enable the bounded bash default: {noticed}"
        );
        assert!(
            !daemonless.contains("_set_completion_notices_wired"),
            "the daemonless bootstrap must not emit the toggle: {daemonless}"
        );
    }

    #[test]
    fn bootstrap_code_binds_skills_lazily() {
        let code = build_rlm_bootstrap_code(&[skill("edit")], false);
        assert!(code.contains(r#"for _prime_agent_skill_name in ["edit"]"#));
        assert!(
            code.contains("_prime_agent_importlib.util.find_spec(_prime_agent_skill_name)"),
            "the bootstrap must check availability without importing: {code}"
        );
        assert!(
            !code.contains("_prime_agent_importlib.import_module(_prime_agent_skill_name)"),
            "the bootstrap must not import a skill eagerly: {code}"
        );
        assert!(code.contains("_prime_agent_main[_prime_agent_skill_name] = _PrimeAgentLazySkill("));
        assert!(code.is_ascii(), "the bootstrap code must stay ASCII");
    }

    /// A subprocess run of the real bootstrap code: a skill present only as a spec stays out of
    /// `sys.modules` until the kernel touches it, then replaces its binding with the module; a
    /// missing skill reports through the marker before the model can reach it.
    #[test]
    fn bootstrap_imports_skills_on_first_use_and_replaces_the_binding() {
        let Some(python) = kernel_python() else {
            return;
        };
        let dir = tempfile::TempDir::new().expect("temp dir");
        let package = dir.path().join("fakeskill");
        std::fs::create_dir(&package).expect("package dir");
        std::fs::write(
            package.join("__init__.py"),
            "VALUE = 41\n\ndef run(*args, **kwargs):\n    return VALUE\n",
        )
        .expect("skill module");
        // A plain interpreter: `__main__` is this script, as in the kernel after its bootstrap cell.
        let script = format!(
            "{}\n\
             import sys as _prime_agent_test_sys\n\
             _probe = fakeskill\n\
             print('LAZY:', type(_probe).__name__, 'fakeskill' in _prime_agent_test_sys.modules)\n\
             print('VALUE:', _probe.VALUE, type(fakeskill).__name__, 'fakeskill' in _prime_agent_test_sys.modules)\n\
             broken_skill_lane\n",
            build_rlm_bootstrap_code(&[skill("fakeskill"), skill("broken-skill-lane")], false)
        );
        let output = std::process::Command::new(python)
            .arg("-c")
            .arg(&script)
            .current_dir(dir.path())
            .env("PYTHONPATH", dir.path())
            .output()
            .expect("python must run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout.contains("__PRIME_AGENT_PYTHON_SKILL_IMPORT_ERRORS__{\"broken_skill_lane\""),
            "{stdout}"
        );
        assert!(
            stdout.contains("LAZY: _PrimeAgentLazySkill False"),
            "the skill must stay unimported until touched: {stdout}"
        );
        assert!(
            stdout.contains("VALUE: 41 _PrimeAgentCallableSkillModule True"),
            "first access must import and replace the binding: {stdout}"
        );
    }

    /// The kernel with prime-agent-runtime installed (see `kernel_lifecycle.rs`); env override
    /// first, else the default venv; skipped with a note when absent.
    fn kernel_python() -> Option<PathBuf> {
        if let Some(explicit) = std::env::var_os("PA_CORE_KERNEL_PYTHON") {
            let explicit = PathBuf::from(explicit);
            assert!(
                explicit.exists(),
                "PA_CORE_KERNEL_PYTHON {} not found",
                explicit.display()
            );
            return Some(explicit);
        }
        let candidate = PathBuf::from(std::env::var("HOME").map_or_else(
            |_| "/home/ubuntu/.prime/agent/kernel-venv/bin/python".to_string(),
            |home| format!("{home}/.prime/agent/kernel-venv/bin/python"),
        ));
        if candidate.exists() {
            return Some(candidate);
        }
        eprintln!(
            "kernel python {} not found; skipping live lazy-skill test",
            candidate.display()
        );
        None
    }

    fn skill(name: &str) -> KernelPythonSkill {
        KernelPythonSkill {
            name: name.to_string(),
            import_name: name.replace('-', "_"),
            package_path: PathBuf::from(format!("/pkg/{name}")),
            pyproject_path: PathBuf::from(format!("/pkg/{name}/pyproject.toml")),
        }
    }

    /// The TS #2381 parser table: marker+JSON, noise-prefixed marker, no marker, non-JSON payload
    /// all land where the host-side report decides notice vs silence.
    #[test]
    fn parse_unavailable_python_skills_table() {
        let cases: Vec<(String, Option<UnavailablePythonSkills>)> = vec![
            (
                format!(
                    "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"websearch\":\"No module named 'websearch'\"}}\n"
                ),
                Some(vec![("websearch".into(), "No module named 'websearch'".into())]),
            ),
            (
                format!("noise\n{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"edit\":\"boom\"}}"),
                Some(vec![("edit".into(), "boom".into())]),
            ),
            ("some unrelated kernel output".to_string(), None),
            (format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}not json"), None),
            (format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{}}"), None),
        ];
        for (stdout, expected) in cases {
            assert_eq!(
                parse_unavailable_python_skills(&stdout),
                expected,
                "{stdout}"
            );
        }
    }

    #[test]
    fn parse_unavailable_python_skills_drops_non_string_and_empty_values() {
        // Non-string values and empty messages drop (TS keeps only
        // non-empty string entries); ordering follows the marker's JSON.
        let stdout = format!(
            "{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"edit\":\"boom\",\"null_skill\":null,\"list_skill\":[1],\"empty_skill\":\"\",\"websearch\":\"No module named 'websearch'\"}}"
        );
        assert_eq!(
            parse_unavailable_python_skills(&stdout),
            Some(vec![
                ("edit".into(), "boom".into()),
                ("websearch".into(), "No module named 'websearch'".into()),
            ])
        );
        // A report whose every value drops stays silent.
        let stdout = format!("{PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER}{{\"empty_skill\":\"\"}}");
        assert_eq!(parse_unavailable_python_skills(&stdout), None);
    }
}
