//! `--agent <name>`: launch from an agent blueprint by filling the launch flags the user did not
//! give. Explicit flags always win over the blueprint.

use std::path::Path;

use pa_core::resources::blueprints::{load_blueprint, Blueprint};

use crate::args::{parse_thinking_level, Args};

/// Apply the blueprint named by `--agent` (a no-op without it) to `args`.
///
/// # Errors
///
/// Error when the blueprint cannot be loaded, or names a workspace or thinking level the CLI
/// would reject as a flag.
pub(crate) fn apply_agent_blueprint(args: &mut Args, agent_dir: &Path) -> Result<(), String> {
    let Some(name) = args.agent.clone() else {
        return Ok(());
    };
    let launch_cwd = match args.cwd.as_deref() {
        Some(cwd) => std::path::PathBuf::from(cwd),
        None => std::env::current_dir().map_err(|error| format!("cannot read the cwd: {error}"))?,
    };
    let blueprint = load_blueprint(&launch_cwd, agent_dir, &name)?;
    fill_launch_flags(args, &blueprint, agent_dir)
}

fn fill_launch_flags(
    args: &mut Args,
    blueprint: &Blueprint,
    agent_dir: &Path,
) -> Result<(), String> {
    if args.workspace.is_none() {
        if let Some(workspace) = &blueprint.workspace {
            pa_types::workspace::validate_workspace_name(workspace).map_err(|error| {
                format!("agent {:?}: invalid workspace: {error}", blueprint.name)
            })?;
            args.workspace = Some(workspace.clone());
        }
    }
    if args.cwd.is_none() {
        args.cwd.clone_from(&blueprint.cwd);
    }
    if args.model.is_none() {
        args.model.clone_from(&blueprint.model);
    }
    if args.thinking.is_none() {
        if let Some(level) = &blueprint.thinking {
            args.thinking = Some(parse_thinking_level(level).ok_or_else(|| {
                format!(
                    "agent {:?}: invalid thinking level {level:?}",
                    blueprint.name
                )
            })?);
        }
    }
    args.skills.extend(blueprint.skills.iter().cloned());
    let appended = blueprint.appended_prompt();
    if !appended.is_empty() {
        // An explicit append list replaces the discovered APPEND_SYSTEM.md, so keep that file
        // ahead of the blueprint's instructions.
        if args.append_system_prompt.is_empty() {
            let launch_cwd = args.cwd.as_deref().map(std::path::PathBuf::from);
            let discovered = launch_cwd
                .or_else(|| std::env::current_dir().ok())
                .and_then(|cwd| {
                    pa_core::resources::discover_append_system_prompt_file(&cwd, agent_dir)
                });
            args.append_system_prompt
                .extend(discovered.map(|path| path.to_string_lossy().to_string()));
        }
        args.append_system_prompt.push(appended);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blueprint() -> Blueprint {
        Blueprint {
            name: "trader".to_string(),
            description: None,
            workspace: Some("markets".to_string()),
            cwd: Some("/work/trading".to_string()),
            model: Some("openai/gpt-5".to_string()),
            thinking: Some("high".to_string()),
            skills: vec!["/skills/edit".to_string()],
            machine: None,
            instructions: "You trade carefully.".to_string(),
        }
    }

    #[test]
    fn explicit_flags_win_and_the_blueprint_fills_the_rest() {
        let mut args = Args {
            model: Some("anthropic/claude".to_string()),
            ..Args::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        fill_launch_flags(&mut args, &blueprint(), tmp.path()).unwrap();
        assert_eq!(
            (
                args.model.as_deref(),
                args.workspace.as_deref(),
                args.cwd.as_deref(),
                args.thinking,
                args.skills,
                args.append_system_prompt,
            ),
            (
                Some("anthropic/claude"),
                Some("markets"),
                Some("/work/trading"),
                parse_thinking_level("high"),
                vec!["/skills/edit".to_string()],
                vec!["You trade carefully.".to_string()],
            )
        );
    }
}
