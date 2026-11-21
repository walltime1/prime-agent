//! Agent blueprints: reusable launchable agent definitions kept as Markdown files with YAML
//! frontmatter, `<agent dir>/agents/<name>.md` (global) and `<cwd>/.prime/agent/agents/<name>.md`
//! (project, wins over global). The keys follow the `.claude/agents/*.md` convention; the body is
//! the role instructions appended to the session system prompt.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::settings::CONFIG_DIR_NAME;
use crate::skills::frontmatter::parse_frontmatter;

/// Directory name holding blueprint files under an agent dir or a project config dir.
pub const BLUEPRINTS_DIR_NAME: &str = "agents";

/// One parsed blueprint. Unset keys leave the matching launch setting at its own default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blueprint {
    pub name: String,
    pub description: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub skills: Vec<String>,
    /// A factory machine the agent is told to run; it is never launched automatically.
    pub machine: Option<String>,
    /// The role instructions (the Markdown body).
    pub instructions: String,
}

impl Blueprint {
    /// The text appended to the session system prompt: the role instructions, then the machine
    /// directive when the blueprint names one.
    #[must_use]
    pub fn appended_prompt(&self) -> String {
        let Some(machine) = &self.machine else {
            return self.instructions.clone();
        };
        let directive = format!(
            "Operating machine: this agent's work runs through the factory machine \"{machine}\" \
             (`await rlm.factory.run(\"{machine}\")`)."
        );
        if self.instructions.is_empty() {
            directive
        } else {
            format!("{}\n\n{directive}", self.instructions)
        }
    }
}

/// The blueprint as file text: YAML frontmatter for the set keys, then the instructions.
///
/// # Errors
///
/// Error when the frontmatter cannot be serialized.
fn render_blueprint(blueprint: &Blueprint) -> Result<String, String> {
    let mut keys = serde_yaml::Mapping::new();
    let mut put = |key: &str, value: serde_yaml::Value| {
        keys.insert(serde_yaml::Value::from(key), value);
    };
    put("name", blueprint.name.clone().into());
    let optional = [
        ("description", &blueprint.description),
        ("workspace", &blueprint.workspace),
        ("cwd", &blueprint.cwd),
        ("model", &blueprint.model),
        ("thinking", &blueprint.thinking),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            put(key, value.clone().into());
        }
    }
    if !blueprint.skills.is_empty() {
        put("skills", blueprint.skills.clone().into());
    }
    if let Some(machine) = &blueprint.machine {
        put("machine", machine.clone().into());
    }
    let frontmatter = serde_yaml::to_string(&keys)
        .map_err(|error| format!("cannot serialize the blueprint: {error}"))?;
    Ok(format!(
        "---\n{frontmatter}---\n\n{}\n",
        blueprint.instructions
    ))
}

/// Write `blueprint` to `<agent dir>/agents/<name>.md`. An existing file is only replaced when
/// `overwrite` is set.
///
/// # Errors
///
/// Error when the name is invalid, the file exists and `overwrite` is not set, or the write fails.
pub fn save_blueprint(
    agent_dir: &Path,
    blueprint: &Blueprint,
    overwrite: bool,
) -> Result<PathBuf, String> {
    validate_blueprint_name(&blueprint.name)?;
    let dir = agent_dir.join(BLUEPRINTS_DIR_NAME);
    let path = dir.join(format!("{}.md", blueprint.name));
    if path.exists() && !overwrite {
        return Err(format!(
            "{} already exists; pass --force to replace it",
            path.display()
        ));
    }
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    crate::settings::storage::atomic_write_with(
        &path,
        &render_blueprint(blueprint)?,
        crate::settings::storage::AtomicWriteOptions { fsync: false },
    )
    .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

/// The `/agent list` text: one line per blueprint name (the project file shadows the global one
/// of the same name), with an unreadable file reported rather than skipped.
#[must_use]
pub fn format_blueprint_list(cwd: &Path, agent_dir: &Path) -> String {
    let mut names = std::collections::BTreeSet::new();
    for dir in blueprint_dirs(cwd, agent_dir) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        names.extend(entries.filter_map(Result::ok).filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "md" && validate_blueprint_name(&stem).is_ok()).then_some(stem)
        }));
    }
    if names.is_empty() {
        return "No agent blueprints. Save one with /agent save <name>.".to_string();
    }
    let lines = names
        .into_iter()
        .map(|name| match load_blueprint(cwd, agent_dir, &name) {
            Ok(blueprint) => match blueprint.description {
                Some(description) => format!("- {name}: {description}"),
                None => format!("- {name}"),
            },
            Err(error) => format!("- {name}: unreadable ({error})"),
        });
    format!(
        "Agent blueprints (launch with prime-agent --agent <name>):\n{}",
        lines.collect::<Vec<_>>().join("\n")
    )
}

/// A blueprint name is a file stem: no separators, no leading dot.
fn validate_blueprint_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "invalid agent name {name:?}: use letters, digits, '-', '_' and '.'"
        ))
    }
}

/// The directories searched for blueprints, highest precedence first.
fn blueprint_dirs(cwd: &Path, agent_dir: &Path) -> [PathBuf; 2] {
    [
        cwd.join(CONFIG_DIR_NAME).join(BLUEPRINTS_DIR_NAME),
        agent_dir.join(BLUEPRINTS_DIR_NAME),
    ]
}

/// Load the blueprint `name`: the project file when present, else the global one.
///
/// # Errors
///
/// Error when `name` is not a valid blueprint name, no file exists, or the file cannot be read
/// or has unreadable frontmatter.
pub fn load_blueprint(cwd: &Path, agent_dir: &Path, name: &str) -> Result<Blueprint, String> {
    validate_blueprint_name(name)?;
    let dirs = blueprint_dirs(cwd, agent_dir);
    let Some(path) = dirs
        .iter()
        .map(|dir| dir.join(format!("{name}.md")))
        .find(|path| path.is_file())
    else {
        let searched: Vec<String> = dirs.iter().map(|dir| dir.display().to_string()).collect();
        return Err(format!(
            "no agent blueprint named {name:?} (looked in {})",
            searched.join(", ")
        ));
    };
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_blueprint(name, &content).map_err(|error| format!("{}: {error}", path.display()))
}

/// Parse blueprint text; `fallback_name` names it when the frontmatter has no `name`.
fn parse_blueprint(fallback_name: &str, content: &str) -> Result<Blueprint, String> {
    let (frontmatter, body) = parse_frontmatter(content);
    let keys = frontmatter.as_object().filter(|keys| !keys.is_empty());
    if keys.is_none() && content.trim_start_matches('\u{feff}').starts_with("---") {
        return Err("the frontmatter is not valid YAML".to_string());
    }
    let text = |key: &str| {
        keys.and_then(|keys| keys.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Ok(Blueprint {
        name: text("name").unwrap_or_else(|| fallback_name.to_string()),
        description: text("description"),
        workspace: text("workspace"),
        cwd: text("cwd"),
        // `.claude/agents` files say `inherit` for "no override".
        model: text("model").filter(|model| model != "inherit"),
        thinking: text("thinking"),
        skills: keys
            .and_then(|keys| keys.get("skills"))
            .map(skill_names)
            .unwrap_or_default(),
        machine: text("machine"),
        instructions: body,
    })
}

/// `skills` as a YAML list or a comma-separated string (the `.claude/agents` `tools` style).
fn skill_names(value: &Value) -> Vec<String> {
    let names: Vec<&str> = match value {
        Value::String(list) => list.split(',').collect(),
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    names
        .into_iter()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_blueprint_wins_over_global_and_maps_every_key() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        let cwd = tmp.path().join("proj");
        let global = agent_dir.join(BLUEPRINTS_DIR_NAME);
        let project = cwd.join(CONFIG_DIR_NAME).join(BLUEPRINTS_DIR_NAME);
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(global.join("trader.md"), "---\nname: global\n---\nold").unwrap();
        std::fs::write(
            project.join("trader.md"),
            "---\nname: trader\ndescription: Watches the book\nworkspace: markets\n\
             cwd: /work/trading\nmodel: inherit\nthinking: high\nskills: [websearch, edit]\n\
             machine: discovery-loop\n---\nYou trade carefully.\n",
        )
        .unwrap();
        let blueprint = load_blueprint(&cwd, &agent_dir, "trader").unwrap();
        assert_eq!(
            blueprint,
            Blueprint {
                name: "trader".to_string(),
                description: Some("Watches the book".to_string()),
                workspace: Some("markets".to_string()),
                cwd: Some("/work/trading".to_string()),
                model: None,
                thinking: Some("high".to_string()),
                skills: vec!["websearch".to_string(), "edit".to_string()],
                machine: Some("discovery-loop".to_string()),
                instructions: "You trade carefully.".to_string(),
            }
        );
        assert_eq!(
            blueprint.appended_prompt(),
            "You trade carefully.\n\nOperating machine: this agent's work runs through the \
             factory machine \"discovery-loop\" (`await rlm.factory.run(\"discovery-loop\")`)."
        );
    }

    #[test]
    fn a_saved_blueprint_loads_back_and_lists_but_never_overwrites_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        let blueprint = Blueprint {
            name: "trader".to_string(),
            description: Some("Watches: the book".to_string()),
            workspace: Some("markets".to_string()),
            cwd: Some("/work/trading".to_string()),
            model: Some("openai/gpt-5".to_string()),
            thinking: Some("high".to_string()),
            skills: vec![],
            machine: None,
            instructions: "You trade carefully.".to_string(),
        };
        save_blueprint(&agent_dir, &blueprint, false).unwrap();
        assert_eq!(
            load_blueprint(tmp.path(), &agent_dir, "trader").unwrap(),
            blueprint
        );
        assert_eq!(
            format_blueprint_list(tmp.path(), &agent_dir),
            "Agent blueprints (launch with prime-agent --agent <name>):\n\
             - trader: Watches: the book"
        );
        let error = save_blueprint(&agent_dir, &blueprint, false).unwrap_err();
        assert!(error.ends_with("already exists; pass --force to replace it"));
        save_blueprint(&agent_dir, &blueprint, true).unwrap();
    }

    #[test]
    fn a_missing_blueprint_a_bad_name_and_broken_frontmatter_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        std::fs::create_dir_all(agent_dir.join(BLUEPRINTS_DIR_NAME)).unwrap();
        std::fs::write(
            agent_dir.join(BLUEPRINTS_DIR_NAME).join("broken.md"),
            "---\nname: [unclosed\n---\nbody",
        )
        .unwrap();
        let load = |name: &str| load_blueprint(tmp.path(), &agent_dir, name).unwrap_err();
        assert!(load("nope").starts_with("no agent blueprint named \"nope\""));
        assert!(load("../escape").starts_with("invalid agent name"));
        assert!(load("broken").ends_with("the frontmatter is not valid YAML"));
    }
}
