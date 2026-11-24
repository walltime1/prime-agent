//! Skill discovery, validation, and prompt formatting.

mod clusters;
pub mod diagnostics;
pub mod discovery;
pub mod frontmatter;
mod index;
pub mod loader;
pub mod prompt_templates;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// Max name length per the Agent Skills spec.
pub const MAX_NAME_LENGTH: usize = 64;
/// Max description length per the Agent Skills spec.
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;

pub use diagnostics::{ResourceCollision, ResourceDiagnostic};
pub use discovery::load_skills_from_dir;
pub(crate) use index::SkillIndex;
pub use loader::{load_skills, LoadSkillsOptions, LoadSkillsResult};
pub use pa_types::slash_commands::parse_slash_command;
pub use prompt_templates::{
    expand_prompt_template, load_prompt_templates, parse_command_args, substitute_args,
    LoadPromptTemplatesOptions, PromptTemplate,
};

/// Source provenance for a resource (the TS source-info shape).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    pub base_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SourceScope {
    User,
    Project,
    Temporary,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SourceOrigin {
    Package,
    TopLevel,
}

/// Markdown entry test: the file name carries a lowercase `.md` extension
/// (TS `endsWith('.md')` parity: mixed-case suffixes stay undiscovered, and
/// a bare `.md` name has no extension and does not count).
#[must_use]
pub fn skill_markdown_name(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext == "md")
}

pub fn create_synthetic_source_info(
    path: &str,
    source: &str,
    scope: SourceScope,
    base_dir: Option<&str>,
) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: source.to_string(),
        scope,
        origin: SourceOrigin::TopLevel,
        base_dir: base_dir.map(str::to_string),
    }
}

/// Python runtime metadata for a Python-backed skill.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillPythonMetadata {
    pub import_name: String,
    pub package_path: PathBuf,
    pub pyproject_path: PathBuf,
}

/// A discovered skill.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
    pub kind: SkillKind,
    pub python: Option<SkillPythonMetadata>,
    /// Optional frontmatter `category:`, the first key the skill clusters by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional frontmatter `tags:`: the first tag is the second clustering key, all of them
    /// feed `rlm.skills.search`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SkillKind {
    Markdown,
    Python,
}

impl Skill {
    #[must_use]
    pub fn is_python(&self) -> bool {
        self.kind == SkillKind::Python
    }

    /// The kind label the prompt inventory and telemetry share
    /// (the inventory tag in `format_skills_for_prompt`, `skill_kind` in events).
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        if self.is_python() {
            "python"
        } else {
            "markdown"
        }
    }
}

/// Runtime info for kernel-side Python skill preparation.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PythonSkillRuntimeInfo {
    pub name: String,
    pub import_name: String,
    pub package_path: PathBuf,
    pub pyproject_path: PathBuf,
}

/// The runtime info for every Python skill in the list.
///
/// # Panics
///
/// The `expect` cannot fire: a skill is marked `Python` only when its metadata was parsed.
#[must_use]
pub fn get_python_skill_runtime_info(skills: &[Skill]) -> Vec<PythonSkillRuntimeInfo> {
    skills
        .iter()
        .filter(|skill| skill.is_python() && skill.source_info.scope != SourceScope::Project)
        .map(|skill| {
            let python = skill.python.as_ref().expect("python skill has metadata");
            PythonSkillRuntimeInfo {
                name: skill.name.clone(),
                import_name: python.import_name.clone(),
                package_path: python.package_path.clone(),
                pyproject_path: python.pyproject_path.clone(),
            }
        })
        .collect()
}

pub(crate) fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    if name.len() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || name.is_empty()
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

pub(crate) fn validate_description(description: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if description.trim().is_empty() {
        errors.push("description is required".to_string());
    } else if description.len() > MAX_DESCRIPTION_LENGTH {
        errors.push(format!(
            "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            description.len()
        ));
    }
    errors
}

pub(crate) use validate_description as validate_skill_description;
pub(crate) use validate_name as validate_skill_name;

/// Longest skill description the prompt inventory carries; `SKILL.md` holds the full text.
const MAX_PROMPT_SKILL_DESCRIPTION_CHARS: usize = 160;

/// Visible-skill count above which the inventory clusters its markdown skills by name.
const CLUSTERED_INVENTORY_THRESHOLD: usize = 15;

/// One-line, length-capped form of a skill description (`rlm.skills` rows share it).
pub(crate) fn prompt_summary(description: &str) -> String {
    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if description.chars().count() <= MAX_PROMPT_SKILL_DESCRIPTION_CHARS {
        return description;
    }
    let budget = MAX_PROMPT_SKILL_DESCRIPTION_CHARS - 3;
    let cut: String = description.chars().take(budget + 1).collect();
    let head = match cut.rsplit_once(' ') {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => cut.chars().take(budget).collect(),
    };
    format!("{head}...")
}

fn inventory_entry(skill: &Skill) -> String {
    let tag = match &skill.python {
        Some(python) => format!("{}: {}", skill.kind_label(), python.import_name),
        None => skill.kind_label().to_string(),
    };
    format!(
        "- {} [{tag}] ({}): {}",
        skill.name,
        skill.file_path.display(),
        prompt_summary(&skill.description)
    )
}

/// `- <cluster> (<count>): name-1, name-2` lines, clusters alphabetical with `general` last.
fn cluster_index_lines(grouped: &[&Skill]) -> Vec<String> {
    let mut members: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (skill, cluster) in grouped.iter().zip(clusters::assign_clusters(grouped)) {
        members.entry(cluster).or_default().push(&skill.name);
    }
    let mut members: Vec<(String, Vec<&str>)> = members.into_iter().collect();
    members.sort_by(|(left, _), (right, _)| {
        clusters::cluster_order_key(left).cmp(&clusters::cluster_order_key(right))
    });
    members
        .into_iter()
        .map(|(cluster, mut names)| {
            names.sort_unstable();
            format!("- {cluster} ({}): {}", names.len(), names.join(", "))
        })
        .collect()
}

/// Format skills as a compact prompt inventory; skills with disableModelInvocation are
/// excluded. Up to [`CLUSTERED_INVENTORY_THRESHOLD`] skills list one line each. Above it,
/// Python and project-scoped skills keep their one line and the remaining markdown skills
/// collapse into a passive cluster-name index that `rlm.skills` resolves on demand.
#[must_use]
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if visible.is_empty() {
        return String::new();
    }
    let (inline, grouped): (Vec<&Skill>, Vec<&Skill>) =
        if visible.len() > CLUSTERED_INVENTORY_THRESHOLD {
            visible
                .into_iter()
                .partition(|skill| clusters::is_inline_skill(skill))
        } else {
            (visible, Vec::new())
        };
    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        "Inside python_repl, read a skill's SKILL.md when the task matches its description; resolve relative paths in it against the skill directory and use the absolute path in Python or `bash(...)` calls.".to_string(),
        "`[python: <import>]` skills are pre-imported in the Python kernel when available: call `await <import>(...)` or inspect `help(<import>)`. `[markdown]` skills are documentation only.".to_string(),
    ];
    if !grouped.is_empty() {
        lines.push("`- <cluster> (<count>): names` lines list the remaining markdown skills by topic. Inside python_repl, resolve them with `await rlm.skills.search(query)`, `await rlm.skills.list(cluster)`, or `await rlm.skills.read(name)`.".to_string());
    }
    lines.push(String::new());
    lines.push("<available_skills>".to_string());
    lines.extend(inline.into_iter().map(inventory_entry));
    lines.extend(cluster_index_lines(&grouped));
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Expand skill commands (`/skill:<name> [args]`) into the `<skill ...>` message block: a non-skill
/// input, an unknown skill name (the surfaces show their own unknown-command notice), and a skill
/// file that fails to read all pass through. Returns the skill the expansion used.
#[must_use]
pub fn expand_skill_command<'a>(text: &str, skills: &'a [Skill]) -> (String, Option<&'a Skill>) {
    let Some((name, args)) = parse_slash_command(text) else {
        return (text.to_string(), None);
    };
    let Some(skill_name) = name.strip_prefix("skill:") else {
        return (text.to_string(), None);
    };
    let Some(skill) = skills.iter().find(|skill| skill.name == skill_name) else {
        return (text.to_string(), None);
    };
    let Ok(content) = std::fs::read_to_string(&skill.file_path) else {
        return (text.to_string(), None);
    };
    let body = frontmatter::strip_frontmatter(&content).trim().to_string();
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path.display(),
        skill.base_dir.display(),
        body
    );
    let expanded = if args.is_empty() {
        block
    } else {
        format!("{block}\n\n{args}")
    };
    (expanded, Some(skill))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_rules() {
        assert!(validate_name("web-search", "web-search").is_empty());
        assert!(!validate_name("web_search", "web_search").is_empty());
        assert!(!validate_name("-lead", "-lead").is_empty());
        assert!(!validate_name("a--b", "a--b").is_empty());
        assert!(!validate_name("web", "other").is_empty());
    }

    #[test]
    fn description_validation_rules() {
        assert_eq!(validate_description(""), vec!["description is required"]);
        assert!(validate_description("ok").is_empty());
        let long = "x".repeat(MAX_DESCRIPTION_LENGTH + 1);
        assert_eq!(validate_description(&long).len(), 1);
    }

    fn temp_skill(name: &str, dir: &std::path::Path) -> Skill {
        let file_path = dir.join("SKILL.md");
        std::fs::write(
            &file_path,
            format!("---\nname: {name}\ndescription: test skill\n---\nUse {name} well."),
        )
        .expect("write skill file");
        Skill {
            name: name.to_string(),
            description: "test skill".to_string(),
            file_path: file_path.clone(),
            base_dir: dir.to_path_buf(),
            source_info: create_synthetic_source_info(
                &file_path.display().to_string(),
                "user",
                SourceScope::User,
                None,
            ),
            disable_model_invocation: false,
            kind: SkillKind::Markdown,
            python: None,
            category: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn expands_a_known_skill_command_with_and_without_args() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill = temp_skill("web-search", dir.path());
        let skills = [skill];
        let (expanded, used) = expand_skill_command("/skill:web-search", &skills);
        assert_eq!(used.map(|skill| skill.name.as_str()), Some("web-search"));
        let parsed = pa_types::skill_blocks::parse_skill_block(&expanded).expect("parses");
        assert_eq!(parsed.name, "web-search");
        // TS block shape: the references note rides inside the body,
        // ahead of the frontmatter-stripped skill content.
        assert_eq!(
            parsed.content,
            format!(
                "References are relative to {}.\n\nUse web-search well.",
                dir.path().display()
            )
        );
        assert_eq!(parsed.user_message, None);
        assert!(parsed.location.ends_with("SKILL.md"));

        let (expanded, used) = expand_skill_command("/skill:web-search find rust tuis", &skills);
        assert_eq!(used.map(|skill| skill.name.as_str()), Some("web-search"));
        let parsed = pa_types::skill_blocks::parse_skill_block(&expanded).expect("parses");
        assert_eq!(
            parsed.user_message.as_deref(),
            Some("find rust tuis"),
            "args follow the block after a blank line"
        );
    }

    #[test]
    fn an_empty_frontmatter_block_is_a_warned_skip_not_a_panic() {
        // TS no-frontmatter semantics: the whole document is the body, the
        // description is missing, the skill is NOT loaded (warning).
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("bare");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(skill_dir.join("SKILL.md"), "---\n---\nUse it well.").expect("write");
        let result = crate::skills::load_skills_from_dir(dir.path(), "user");
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.iter().any(
            |d| matches!(d, crate::skills::ResourceDiagnostic::Warning { message, .. }
                if message == "description is required")
        ));
    }

    #[test]
    fn an_empty_frontmatter_name_falls_back_to_the_parent_directory() {
        // TS `frontmatter.name || parentDirName`.
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("named-dir");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: \"\"\ndescription: The named dir skill\n---\nbody",
        )
        .expect("write");
        let result = crate::skills::load_skills_from_dir(dir.path(), "user");
        assert_eq!(
            result
                .skills
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["named-dir"]
        );
    }

    #[test]
    fn non_skill_and_unknown_inputs_pass_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills = [temp_skill("web-search", dir.path())];
        for text in [
            "hello world",
            "/compact",
            "/skill:missing do something",
            "/skill:",
        ] {
            let (expanded, used) = expand_skill_command(text, &skills);
            assert_eq!(expanded, text, "{text} must pass through");
            assert!(used.is_none(), "{text} must not report a skill");
        }
    }

    #[test]
    fn an_unreadable_skill_file_passes_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut skill = temp_skill("gone", dir.path());
        std::fs::remove_file(&skill.file_path).expect("remove skill file");
        skill.file_path = dir.path().join("missing-SKILL.md");
        let skills = [skill];
        let (expanded, used) = expand_skill_command("/skill:gone", &skills);
        assert_eq!(expanded, "/skill:gone");
        assert!(used.is_none());
    }

    #[test]
    fn prompt_formatting_excludes_hidden() {
        let skill = |name: &str, disable: bool| Skill {
            name: name.to_string(),
            description: format!("Does {name}"),
            file_path: PathBuf::from("/s/SKILL.md"),
            base_dir: PathBuf::from("/s"),
            source_info: create_synthetic_source_info("/s", "user", SourceScope::User, None),
            disable_model_invocation: disable,
            kind: SkillKind::Markdown,
            python: None,
            category: None,
            tags: Vec::new(),
        };
        let hidden = format_skills_for_prompt(&[skill("hidden", true)]);
        assert_eq!(hidden, "");
        let formatted = format_skills_for_prompt(&[skill("web-search", false)]);
        assert!(formatted.contains("- web-search [markdown] (/s/SKILL.md): Does web-search\n"));
    }

    #[test]
    fn prompt_formatting_tags_python_skills_and_caps_long_descriptions() {
        let long_description = format!("Line one\n  line two {}", "word ".repeat(60));
        let skill = Skill {
            name: "long".to_string(),
            description: long_description,
            file_path: PathBuf::from("/s/SKILL.md"),
            base_dir: PathBuf::from("/s"),
            source_info: create_synthetic_source_info("/s", "user", SourceScope::User, None),
            disable_model_invocation: false,
            kind: SkillKind::Python,
            python: Some(SkillPythonMetadata {
                import_name: "long_skill".to_string(),
                package_path: PathBuf::from("/s"),
                pyproject_path: PathBuf::from("/s/pyproject.toml"),
            }),
            category: None,
            tags: Vec::new(),
        };
        let formatted = format_skills_for_prompt(&[skill]);
        let entry = formatted
            .lines()
            .find(|line| line.starts_with("- long "))
            .expect("the skill entry");
        let expected_summary = format!("Line one line two {}...", "word ".repeat(28).trim_end());
        assert!(expected_summary.chars().count() <= MAX_PROMPT_SKILL_DESCRIPTION_CHARS);
        assert_eq!(
            entry,
            format!("- long [python: long_skill] (/s/SKILL.md): {expected_summary}")
        );
    }

    fn inventory_skill(name: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("Does {name}"),
            file_path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            base_dir: PathBuf::from(format!("/skills/{name}")),
            source_info: create_synthetic_source_info("/s", "user", SourceScope::User, None),
            disable_model_invocation: false,
            kind: SkillKind::Markdown,
            python: None,
            category: None,
            tags: Vec::new(),
        }
    }

    fn inventory_entries(skills: &[Skill]) -> Vec<String> {
        let formatted = format_skills_for_prompt(skills);
        formatted
            .lines()
            .skip_while(|line| *line != "<available_skills>")
            .skip(1)
            .take_while(|line| *line != "</available_skills>")
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_inventory_stays_flat_up_to_the_threshold() {
        let skills: Vec<Skill> = (0..CLUSTERED_INVENTORY_THRESHOLD)
            .map(|index| inventory_skill(&format!("scientific-skill-{index:02}")))
            .collect();
        let entries = inventory_entries(&skills);
        assert_eq!(entries.len(), CLUSTERED_INVENTORY_THRESHOLD);
        assert_eq!(
            entries[0],
            "- scientific-skill-00 [markdown] (/skills/scientific-skill-00/SKILL.md): Does scientific-skill-00"
        );
        assert!(!format_skills_for_prompt(&skills).contains("rlm.skills"));
    }

    #[test]
    fn above_the_threshold_markdown_skills_collapse_into_a_cluster_name_index() {
        let mut project = inventory_skill("repo-conventions");
        project.source_info.scope = SourceScope::Project;
        let mut python = inventory_skill("world-model");
        python.kind = SkillKind::Python;
        python.python = Some(SkillPythonMetadata {
            import_name: "world_model".to_string(),
            package_path: PathBuf::from("/skills/world-model"),
            pyproject_path: PathBuf::from("/skills/world-model/pyproject.toml"),
        });
        let mut declared = inventory_skill("handoff");
        declared.category = Some("Workflow".to_string());
        let mut skills = vec![project, python, declared, inventory_skill("grilling")];
        skills.extend(
            (0..CLUSTERED_INVENTORY_THRESHOLD - 3)
                .map(|index| inventory_skill(&format!("oleafly-{index:02}"))),
        );
        skills.push(inventory_skill("statistical-power"));
        skills.push(inventory_skill("statistical-analysis"));
        assert!(skills.len() > CLUSTERED_INVENTORY_THRESHOLD);

        let entries = inventory_entries(&skills);
        let oleafly_names = (0..CLUSTERED_INVENTORY_THRESHOLD - 3)
            .map(|index| format!("oleafly-{index:02}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            entries,
            [
                "- repo-conventions [markdown] (/skills/repo-conventions/SKILL.md): Does repo-conventions".to_string(),
                "- world-model [python: world_model] (/skills/world-model/SKILL.md): Does world-model".to_string(),
                format!("- oleafly ({}): {oleafly_names}", CLUSTERED_INVENTORY_THRESHOLD - 3),
                "- statistical (2): statistical-analysis, statistical-power".to_string(),
                "- workflow (1): handoff".to_string(),
                "- general (1): grilling".to_string(),
            ]
        );
        let formatted = format_skills_for_prompt(&skills);
        assert!(formatted.contains("`await rlm.skills.search(query)`"));
        assert!(formatted.contains("`await rlm.skills.read(name)`"));
    }

    #[test]
    fn the_cluster_index_does_not_depend_on_discovery_order() {
        let skills: Vec<Skill> = [
            "oleafly-a",
            "scientific-a",
            "handoff",
            "oleafly-b",
            "scientific-b",
            "grilling",
            "oleafly-c",
            "scientific-c",
            "retro",
            "oleafly-d",
            "scientific-d",
            "teach",
            "oleafly-e",
            "scientific-e",
            "wizard",
            "oleafly-f",
        ]
        .iter()
        .map(|name| inventory_skill(name))
        .collect();
        let mut reversed = skills.clone();
        reversed.reverse();
        let forward = inventory_entries(&skills);
        assert_eq!(forward, inventory_entries(&reversed));
        assert_eq!(
            forward,
            [
                "- oleafly (6): oleafly-a, oleafly-b, oleafly-c, oleafly-d, oleafly-e, oleafly-f",
                "- scientific (5): scientific-a, scientific-b, scientific-c, scientific-d, scientific-e",
                "- general (5): grilling, handoff, retro, teach, wizard",
            ]
        );
    }

    #[test]
    fn frontmatter_category_and_tags_are_read_from_a_yaml_list_or_a_comma_string() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, extra) in [
            (
                "listed",
                "category: Writing\ntags: [drafting, ' latex ', '']\n",
            ),
            ("csv", "tags: alpha, beta\n"),
            ("bare", ""),
        ] {
            let skill_dir = dir.path().join(name);
            std::fs::create_dir_all(&skill_dir).expect("mkdir");
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n{extra}---\nbody"),
            )
            .expect("write");
        }
        let mut skills = crate::skills::load_skills_from_dir(dir.path(), "user").skills;
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        let facets: Vec<(&str, Option<&str>, Vec<&str>)> = skills
            .iter()
            .map(|skill| {
                (
                    skill.name.as_str(),
                    skill.category.as_deref(),
                    skill.tags.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            facets,
            [
                ("bare", None, vec![]),
                ("csv", None, vec!["alpha", "beta"]),
                ("listed", Some("Writing"), vec!["drafting", "latex"]),
            ]
        );
    }
}
