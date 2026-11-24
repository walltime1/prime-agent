//! Deterministic skill clustering shared by the passive prompt index and `rlm.skills`.
//!
//! Precedence per skill: frontmatter `category:` -> first frontmatter tag -> shared
//! package directory -> common name prefix -> `general`. Pure string work over data
//! discovery already loaded, so it costs nothing at startup and never depends on
//! directory iteration order.

use std::collections::HashMap;

use super::{Skill, SourceScope};

/// Cluster for skills no rule groups.
pub(crate) const GENERAL_CLUSTER: &str = "general";
/// Shortest name prefix that may name a cluster (`to-spec`/`to-tickets` stay ungrouped).
const MIN_PREFIX_LEN: usize = 3;
/// Fewest skills a derived (directory or prefix) group needs; declared labels need one.
const MIN_DERIVED_GROUP_SIZE: usize = 2;
/// Directory names that are skill roots rather than packages.
const SKILL_ROOT_DIR_NAME: &str = "skills";

/// Lowercase `raw` into a hyphen-joined `[a-z0-9-]` cluster label; `None` when nothing is left.
pub(crate) fn normalize_cluster_label(raw: &str) -> Option<String> {
    let label = raw
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!label.is_empty()).then_some(label)
}

fn declared_label(skill: &Skill) -> Option<String> {
    skill
        .category
        .as_deref()
        .and_then(normalize_cluster_label)
        .or_else(|| {
            skill
                .tags
                .iter()
                .find_map(|tag| normalize_cluster_label(tag))
        })
}

fn directory_group(skill: &Skill) -> Option<String> {
    if skill.file_path.file_name()? != "SKILL.md" {
        return None;
    }
    let parent = skill.base_dir.parent()?.file_name()?.to_str()?;
    if parent.starts_with('.') || parent == SKILL_ROOT_DIR_NAME {
        return None;
    }
    normalize_cluster_label(parent)
}

fn name_prefix(name: &str) -> Option<&str> {
    let prefix = name.split('-').next()?;
    (prefix.len() >= MIN_PREFIX_LEN).then_some(prefix)
}

fn count_by<'a>(keys: impl Iterator<Item = &'a str>) -> HashMap<&'a str, usize> {
    let mut counts = HashMap::new();
    for key in keys {
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

/// The cluster of every skill, aligned with `skills`.
///
/// A directory group that holds every skill carries no information (the user keeps them
/// all in one folder), so it falls through to the name prefix.
pub(crate) fn assign_clusters(skills: &[&Skill]) -> Vec<String> {
    let directory_groups: Vec<Option<String>> =
        skills.iter().map(|skill| directory_group(skill)).collect();
    let directory_counts = count_by(directory_groups.iter().flatten().map(String::as_str));
    let prefix_counts = count_by(skills.iter().filter_map(|skill| name_prefix(&skill.name)));
    skills
        .iter()
        .zip(&directory_groups)
        .map(|(skill, directory)| {
            declared_label(skill)
                .or_else(|| {
                    directory
                        .as_ref()
                        .filter(|group| {
                            let size = directory_counts[group.as_str()];
                            size >= MIN_DERIVED_GROUP_SIZE && size < skills.len()
                        })
                        .cloned()
                })
                .or_else(|| {
                    name_prefix(&skill.name)
                        .filter(|prefix| prefix_counts[prefix] >= MIN_DERIVED_GROUP_SIZE)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| GENERAL_CLUSTER.to_string())
        })
        .collect()
}

/// Python and project-scoped skills stay inline in the prompt inventory: the model must see
/// how to call the former and which project the latter belongs to without a lookup.
pub(crate) fn is_inline_skill(skill: &Skill) -> bool {
    skill.is_python() || skill.source_info.scope == SourceScope::Project
}

/// The `rlm.skills` cluster of an inline skill: a kind facet, since inline skills are not
/// part of the passive cluster index.
pub(crate) fn inline_cluster(skill: &Skill) -> &'static str {
    if skill.is_python() {
        "python"
    } else {
        "project"
    }
}

/// Sort key that orders clusters alphabetically with `general` last.
pub(crate) fn cluster_order_key(cluster: &str) -> (bool, &str) {
    (cluster == GENERAL_CLUSTER, cluster)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::skills::{create_synthetic_source_info, SkillKind, SourceScope};

    fn skill(name: &str, dir: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("Does {name}"),
            file_path: PathBuf::from(dir).join(name).join("SKILL.md"),
            base_dir: PathBuf::from(dir).join(name),
            source_info: create_synthetic_source_info("/s", "user", SourceScope::User, None),
            disable_model_invocation: false,
            kind: SkillKind::Markdown,
            python: None,
            category: None,
            tags: Vec::new(),
        }
    }

    fn clusters_of(skills: &[Skill]) -> Vec<String> {
        let refs: Vec<&Skill> = skills.iter().collect();
        assign_clusters(&refs)
    }

    #[test]
    fn declared_category_beats_tags_directory_and_prefix() {
        let mut declared = skill("oleafly-cli", "/root/pkg-a");
        declared.category = Some("Build Tools".to_string());
        let mut tagged = skill("oleafly-figure", "/root/pkg-a");
        tagged.tags = vec!["Plots & Charts".to_string(), "oleafly".to_string()];
        let plain = skill("oleafly-loop", "/root/pkg-b");
        assert_eq!(
            clusters_of(&[declared, tagged, plain]),
            ["build-tools", "plots-charts", "oleafly"]
        );
    }

    #[test]
    fn a_shared_package_directory_groups_skills_and_a_universal_one_does_not() {
        let grouped = [
            skill("alpha", "/root/pkg-a"),
            skill("beta", "/root/pkg-a"),
            skill("gamma", "/root/pkg-b"),
        ];
        assert_eq!(clusters_of(&grouped), ["pkg-a", "pkg-a", "general"]);

        let everything_in_one_folder = [
            skill("scientific-slides", "/root/my-skills"),
            skill("scientific-writing", "/root/my-skills"),
            skill("handoff", "/root/my-skills"),
        ];
        assert_eq!(
            clusters_of(&everything_in_one_folder),
            ["scientific", "scientific", "general"]
        );
    }

    #[test]
    fn the_skill_root_directory_is_not_a_package_group() {
        let skills = [
            skill("alpha", "/home/u/.agents/skills"),
            skill("beta", "/home/u/.agents/skills"),
            skill("gamma", "/home/u/other/skills"),
        ];
        assert_eq!(clusters_of(&skills), ["general", "general", "general"]);
    }

    #[test]
    fn name_prefixes_group_only_when_shared_and_long_enough() {
        let skills = [
            skill("statistical-analysis", "/skills"),
            skill("statistical-power", "/skills"),
            skill("research", "/skills"),
            skill("research-grants", "/skills"),
            skill("to-spec", "/skills"),
            skill("to-tickets", "/skills"),
            skill("handoff", "/skills"),
        ];
        assert_eq!(
            clusters_of(&skills),
            [
                "statistical",
                "statistical",
                "research",
                "research",
                "general",
                "general",
                "general"
            ]
        );
    }

    #[test]
    fn clusters_do_not_depend_on_input_order() {
        let skills = [
            skill("scientific-slides", "/skills"),
            skill("handoff", "/skills"),
            skill("scientific-writing", "/skills"),
        ];
        let mut reversed = skills.clone();
        reversed.reverse();
        let forward: HashMap<String, String> = skills
            .iter()
            .map(|s| s.name.clone())
            .zip(clusters_of(&skills))
            .collect();
        let backward: HashMap<String, String> = reversed
            .iter()
            .map(|s| s.name.clone())
            .zip(clusters_of(&reversed))
            .collect();
        assert_eq!(forward, backward);
    }
}
