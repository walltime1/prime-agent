//! In-memory skill index behind `rlm.skills`: clusters plus local BM25 search.
//!
//! Built from the skills discovery already loaded, so searching never calls a model
//! and never touches the disk.

use std::collections::{BTreeSet, HashMap};

use super::clusters::{
    assign_clusters, cluster_order_key, inline_cluster, is_inline_skill, normalize_cluster_label,
};
use super::Skill;

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;
const NAME_WEIGHT: f64 = 3.0;
const LABEL_WEIGHT: f64 = 2.0;
const DESCRIPTION_WEIGHT: f64 = 1.0;
/// Query terms at least this long also match longer words that start with them
/// (`review` finds `reviewer`).
const MIN_PREFIX_MATCH_LEN: usize = 4;

/// One searchable skill: the skill, its cluster, and its weighted term counts.
pub(crate) struct IndexedSkill {
    pub(crate) skill: Skill,
    pub(crate) cluster: String,
    term_weights: HashMap<String, f64>,
    length: f64,
}

pub(crate) struct SkillIndex {
    entries: Vec<IndexedSkill>,
    average_length: f64,
}

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn term_weight(term_weights: &HashMap<String, f64>, query_term: &str) -> f64 {
    if query_term.len() >= MIN_PREFIX_MATCH_LEN {
        term_weights
            .iter()
            .filter(|(term, _)| term.starts_with(query_term))
            .map(|(_, weight)| weight)
            .sum()
    } else {
        term_weights.get(query_term).copied().unwrap_or_default()
    }
}

impl IndexedSkill {
    fn new(skill: Skill, cluster: String) -> Self {
        let mut term_weights: HashMap<String, f64> = HashMap::new();
        let fields = [
            (NAME_WEIGHT, skill.name.clone()),
            (
                LABEL_WEIGHT,
                format!(
                    "{cluster} {} {}",
                    skill.category.as_deref().unwrap_or_default(),
                    skill.tags.join(" ")
                ),
            ),
            (DESCRIPTION_WEIGHT, skill.description.clone()),
        ];
        let mut length = 0.0;
        for (weight, text) in fields {
            for token in tokenize(&text) {
                *term_weights.entry(token).or_default() += weight;
                length += weight;
            }
        }
        Self {
            skill,
            cluster,
            term_weights,
            length,
        }
    }
}

impl SkillIndex {
    /// Index the skills the model may invoke (`disable-model-invocation` skills stay out).
    pub(crate) fn new(skills: &[Skill]) -> Self {
        let (inline, grouped): (Vec<&Skill>, Vec<&Skill>) = skills
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .partition(|skill| is_inline_skill(skill));
        let grouped_clusters = assign_clusters(&grouped);
        let mut entries: Vec<IndexedSkill> = inline
            .into_iter()
            .map(|skill| (skill, inline_cluster(skill).to_string()))
            .chain(grouped.into_iter().zip(grouped_clusters))
            .map(|(skill, cluster)| IndexedSkill::new(skill.clone(), cluster))
            .collect();
        entries.sort_by(|left, right| {
            cluster_order_key(&left.cluster)
                .cmp(&cluster_order_key(&right.cluster))
                .then_with(|| left.skill.name.cmp(&right.skill.name))
        });
        let average_length = if entries.is_empty() {
            0.0
        } else {
            entries.iter().map(|entry| entry.length).sum::<f64>() / entries.len() as f64
        };
        Self {
            entries,
            average_length,
        }
    }

    pub(crate) fn find(&self, name: &str) -> Option<&Skill> {
        self.entries
            .iter()
            .map(|entry| &entry.skill)
            .find(|skill| skill.name == name)
    }

    /// Every cluster label, `general` last.
    pub(crate) fn cluster_labels(&self) -> Vec<&str> {
        let labels: BTreeSet<(bool, &str)> = self
            .entries
            .iter()
            .map(|entry| cluster_order_key(&entry.cluster))
            .collect();
        labels.into_iter().map(|(_, label)| label).collect()
    }

    /// Skills of one cluster (`None`: all), ordered by cluster then name. `None` result when
    /// the cluster label is unknown.
    pub(crate) fn list(&self, cluster: Option<&str>) -> Option<Vec<&IndexedSkill>> {
        let Some(cluster) = cluster else {
            return Some(self.entries.iter().collect());
        };
        let wanted = normalize_cluster_label(cluster)?;
        let members: Vec<&IndexedSkill> = self
            .entries
            .iter()
            .filter(|entry| entry.cluster == wanted)
            .collect();
        (!members.is_empty()).then_some(members)
    }

    /// Up to `limit` skills ranked by BM25 over name, cluster/tags, and description;
    /// ties break by name so the order is deterministic.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<(&IndexedSkill, f64)> {
        let query_terms: BTreeSet<String> = tokenize(query).into_iter().collect();
        let document_count = self.entries.len() as f64;
        let idf_and_terms: Vec<(f64, &String)> = query_terms
            .iter()
            .map(|term| {
                let matching = self
                    .entries
                    .iter()
                    .filter(|entry| term_weight(&entry.term_weights, term) > 0.0)
                    .count() as f64;
                let idf = (1.0 + (document_count - matching + 0.5) / (matching + 0.5)).ln();
                (idf, term)
            })
            .collect();
        let mut scored: Vec<(&IndexedSkill, f64)> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let length_norm = 1.0 - BM25_B + BM25_B * entry.length / self.average_length;
                let score: f64 = idf_and_terms
                    .iter()
                    .map(|(idf, term)| {
                        let weight = term_weight(&entry.term_weights, term);
                        idf * weight * (BM25_K1 + 1.0) / (weight + BM25_K1 * length_norm)
                    })
                    .sum();
                (score > 0.0).then_some((entry, score))
            })
            .collect();
        scored.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.skill.name.cmp(&right.0.skill.name))
        });
        scored.truncate(limit);
        scored
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::skills::{create_synthetic_source_info, SkillKind, SourceScope};

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            file_path: PathBuf::from("/skills").join(name).join("SKILL.md"),
            base_dir: PathBuf::from("/skills").join(name),
            source_info: create_synthetic_source_info("/s", "user", SourceScope::User, None),
            disable_model_invocation: false,
            kind: SkillKind::Markdown,
            python: None,
            category: None,
            tags: Vec::new(),
        }
    }

    fn sample_index() -> SkillIndex {
        let mut hidden = skill("secret-skill", "Hidden latex helper");
        hidden.disable_model_invocation = true;
        let mut tagged = skill("handoff", "Compact the conversation into a note");
        tagged.tags = vec!["session".to_string(), "continuity".to_string()];
        SkillIndex::new(&[
            skill(
                "oleafly-latex-build",
                "Fix LaTeX compile errors until the build is clean",
            ),
            skill(
                "oleafly-verify-claims",
                "Audit claims against cited sources",
            ),
            skill("scientific-slides", "Build slide decks for research talks"),
            skill(
                "scientific-writing",
                "Draft manuscripts with cited evidence",
            ),
            tagged,
            hidden,
        ])
    }

    fn names(results: &[(&IndexedSkill, f64)]) -> Vec<String> {
        results.iter().map(|(e, _)| e.skill.name.clone()).collect()
    }

    #[test]
    fn search_ranks_by_name_label_and_description_and_skips_hidden_skills() {
        let index = sample_index();
        assert_eq!(
            names(&index.search("latex compile errors", 10)),
            ["oleafly-latex-build"]
        );
        assert_eq!(
            names(&index.search("slides", 10)),
            ["scientific-slides"],
            "a name hit"
        );
        assert_eq!(
            names(&index.search("continuity", 10)),
            ["handoff"],
            "a tag hit"
        );
        assert!(
            index.search("secret", 10).is_empty(),
            "hidden skills stay out"
        );
        assert!(index.search("zebra", 10).is_empty());
    }

    #[test]
    fn long_query_terms_match_word_prefixes_and_limit_truncates() {
        let index = sample_index();
        assert_eq!(
            names(&index.search("manuscript", 10)),
            ["scientific-writing"]
        );
        assert_eq!(index.search("cited", 1).len(), 1);
    }

    #[test]
    fn search_ties_break_by_name() {
        let index = SkillIndex::new(&[skill("beta", "alpha topic"), skill("alpha", "alpha topic")]);
        assert_eq!(names(&index.search("topic", 10)), ["alpha", "beta"]);
    }

    #[test]
    fn list_filters_by_cluster_and_reports_unknown_clusters() {
        let index = sample_index();
        let listed = |cluster| -> Vec<String> {
            index
                .list(cluster)
                .expect("known cluster")
                .iter()
                .map(|entry| entry.skill.name.clone())
                .collect()
        };
        assert_eq!(
            listed(None),
            [
                "oleafly-latex-build",
                "oleafly-verify-claims",
                "scientific-slides",
                "scientific-writing",
                "handoff"
            ]
        );
        assert_eq!(
            listed(Some("Scientific")),
            ["scientific-slides", "scientific-writing"]
        );
        assert!(index.list(Some("nope")).is_none());
        assert_eq!(index.cluster_labels(), ["oleafly", "scientific", "session"]);
    }

    #[test]
    fn find_returns_visible_skills_by_exact_name() {
        let index = sample_index();
        assert!(index.find("handoff").is_some());
        assert!(index.find("secret-skill").is_none());
    }
}
