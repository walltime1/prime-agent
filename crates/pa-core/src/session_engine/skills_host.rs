//! The `rlm.skills.*` kernel host requests: local search, listing, and on-demand reading of
//! the session's skills (`rlm.skills.search`, `rlm.skills.list`, `rlm.skills.read`).
//!
//! The prompt inventory names every markdown skill only by cluster once there are many; these
//! handlers are how the model resolves a name to a description or a `SKILL.md` body. Wire
//! contract: the Python side (`rlm/__init__.py`) sends typed requests and parses the strict
//! `snake_case` replies below. No model is called; the index lives in memory.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use crate::kernel::shared::{host_handler, HostRequestHandlers};
use crate::skills::frontmatter::strip_frontmatter;
use crate::skills::{prompt_summary, Skill, SkillIndex};

const DEFAULT_SKILL_SEARCH_LIMIT: u64 = 10;
const MAX_SKILL_SEARCH_LIMIT: u64 = 50;

/// Runs once per successful `rlm.skills.read`, so the embedding can count the skill use.
pub(crate) type SkillReadHook = Arc<dyn Fn() + Send + Sync>;

/// One skill row of `rlm.skills.search` and `rlm.skills.list`.
#[derive(Serialize)]
struct SkillRow<'a> {
    name: &'a str,
    kind: &'static str,
    cluster: &'a str,
    description: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    python_import: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
}

fn skill_row<'a>(skill: &'a Skill, cluster: &'a str, score: Option<f64>) -> SkillRow<'a> {
    SkillRow {
        name: &skill.name,
        kind: skill.kind_label(),
        cluster,
        description: prompt_summary(&skill.description),
        path: skill.file_path.display().to_string(),
        python_import: skill
            .python
            .as_ref()
            .map(|python| python.import_name.as_str()),
        score: score.map(|score| (score * 1000.0).round() / 1000.0),
    }
}

fn required_string<'a>(data: &'a Value, field: &str, request: &str) -> anyhow::Result<&'a str> {
    match data.get(field).and_then(Value::as_str).map(str::trim) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => anyhow::bail!("{request} {field} must be a non-empty string"),
    }
}

/// Register the `rlm.skills.*` handlers over the skills the model may invoke.
pub(crate) fn register_skills_host_handlers(
    handlers: &mut HostRequestHandlers,
    skills: &[Skill],
    on_read: SkillReadHook,
) {
    let index = Arc::new(SkillIndex::new(skills));

    let search_index = Arc::clone(&index);
    handlers.register(
        "rlm.skills.search",
        host_handler(move |payload| {
            let index = Arc::clone(&search_index);
            Box::pin(async move {
                let data = &payload.data;
                let query = required_string(data, "query", "rlm.skills.search")?;
                let limit = match data.get("limit") {
                    None | Some(Value::Null) => Some(DEFAULT_SKILL_SEARCH_LIMIT),
                    Some(value) => value
                        .as_u64()
                        .filter(|limit| (1..=MAX_SKILL_SEARCH_LIMIT).contains(limit)),
                };
                let Some(limit) = limit else {
                    anyhow::bail!(
                        "rlm.skills.search limit must be an integer from 1 to {MAX_SKILL_SEARCH_LIMIT}"
                    );
                };
                let rows: Vec<SkillRow<'_>> = index
                    .search(query, limit as usize)
                    .into_iter()
                    .map(|(entry, score)| skill_row(&entry.skill, &entry.cluster, Some(score)))
                    .collect();
                Ok(json!({ "skills": rows }))
            })
        }),
    );

    let list_index = Arc::clone(&index);
    handlers.register(
        "rlm.skills.list",
        host_handler(move |payload| {
            let index = Arc::clone(&list_index);
            Box::pin(async move {
                let cluster = match payload.data.get("cluster") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(cluster)) => Some(cluster.as_str()),
                    Some(_) => anyhow::bail!("rlm.skills.list cluster must be a string or null"),
                };
                let Some(entries) = index.list(cluster) else {
                    anyhow::bail!(
                        "rlm.skills.list unknown cluster {:?}; clusters: {}",
                        cluster.unwrap_or_default(),
                        index.cluster_labels().join(", ")
                    );
                };
                let rows: Vec<SkillRow<'_>> = entries
                    .into_iter()
                    .map(|entry| skill_row(&entry.skill, &entry.cluster, None))
                    .collect();
                Ok(json!({ "skills": rows }))
            })
        }),
    );

    handlers.register(
        "rlm.skills.read",
        host_handler(move |payload| {
            let index = Arc::clone(&index);
            let on_read = Arc::clone(&on_read);
            Box::pin(async move {
                let name = required_string(&payload.data, "name", "rlm.skills.read")?;
                let Some(skill) = index.find(name) else {
                    anyhow::bail!(
                        "rlm.skills.read unknown skill {name:?}; find it with rlm.skills.search"
                    );
                };
                let content =
                    tokio::fs::read_to_string(&skill.file_path)
                        .await
                        .map_err(|error| {
                            anyhow::anyhow!(
                                "rlm.skills.read cannot read {}: {error}",
                                skill.file_path.display()
                            )
                        })?;
                on_read();
                Ok(json!({
                    "name": skill.name,
                    "path": skill.file_path.display().to_string(),
                    "base_dir": skill.base_dir.display().to_string(),
                    "body": strip_frontmatter(&content).trim(),
                }))
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::kernel::shared::HostRequestPayload;
    use crate::skills::load_skills_from_dir;

    fn write_skill(root: &Path, name: &str, description: &str, extra_frontmatter: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\n{extra_frontmatter}---\nBody of {name}.\n"
            ),
        )
        .expect("write skill");
    }

    fn wired(root: &Path) -> (HostRequestHandlers, Arc<AtomicUsize>) {
        write_skill(root, "oleafly-latex-build", "Fix LaTeX compile errors", "");
        write_skill(
            root,
            "oleafly-verify-claims",
            "Audit claims against sources",
            "",
        );
        write_skill(
            root,
            "handoff",
            "Write a continuation note",
            "tags: [session]\n",
        );
        let skills = load_skills_from_dir(root, "user").skills;
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let mut handlers = HostRequestHandlers::default();
        register_skills_host_handlers(
            &mut handlers,
            &skills,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (handlers, reads)
    }

    async fn call(
        handlers: &HostRequestHandlers,
        request_type: &str,
        data: Value,
    ) -> anyhow::Result<Value> {
        let handler = handlers
            .get(request_type)
            .unwrap_or_else(|| panic!("{request_type} handler registered"))
            .clone();
        handler(HostRequestPayload { data }).await
    }

    #[tokio::test]
    async fn search_returns_ranked_rows_and_validates_arguments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (handlers, _) = wired(dir.path());
        let reply = call(
            &handlers,
            "rlm.skills.search",
            json!({ "query": "latex errors" }),
        )
        .await
        .expect("search");
        let skills = reply["skills"].as_array().expect("rows");
        assert_eq!(skills.len(), 1);
        assert_eq!(
            skills[0],
            json!({
                "name": "oleafly-latex-build",
                "kind": "markdown",
                "cluster": "oleafly",
                "description": "Fix LaTeX compile errors",
                "path": dir.path().join("oleafly-latex-build/SKILL.md").display().to_string(),
                "score": skills[0]["score"],
            })
        );
        assert!(skills[0]["score"].as_f64().expect("score") > 0.0);

        for data in [json!({}), json!({ "query": "  " }), json!({ "query": 3 })] {
            let error = call(&handlers, "rlm.skills.search", data)
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "rlm.skills.search query must be a non-empty string"
            );
        }
        let error = call(
            &handlers,
            "rlm.skills.search",
            json!({ "query": "latex", "limit": 0 }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "rlm.skills.search limit must be an integer from 1 to 50"
        );
    }

    #[tokio::test]
    async fn list_filters_by_cluster_and_names_the_clusters_on_a_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (handlers, _) = wired(dir.path());
        let names = |reply: &Value| -> Vec<String> {
            reply["skills"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| row["name"].as_str().expect("name").to_string())
                .collect()
        };
        let all = call(&handlers, "rlm.skills.list", json!({ "cluster": null }))
            .await
            .expect("list all");
        assert_eq!(
            names(&all),
            ["oleafly-latex-build", "oleafly-verify-claims", "handoff"]
        );
        let oleafly = call(
            &handlers,
            "rlm.skills.list",
            json!({ "cluster": "oleafly" }),
        )
        .await
        .expect("list cluster");
        assert_eq!(
            names(&oleafly),
            ["oleafly-latex-build", "oleafly-verify-claims"]
        );

        let error = call(&handlers, "rlm.skills.list", json!({ "cluster": "nope" }))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "rlm.skills.list unknown cluster \"nope\"; clusters: oleafly, session"
        );
    }

    #[tokio::test]
    async fn read_returns_the_body_and_base_dir_and_counts_the_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (handlers, reads) = wired(dir.path());
        let reply = call(&handlers, "rlm.skills.read", json!({ "name": "handoff" }))
            .await
            .expect("read");
        assert_eq!(
            reply,
            json!({
                "name": "handoff",
                "path": dir.path().join("handoff/SKILL.md").display().to_string(),
                "base_dir": dir.path().join("handoff").display().to_string(),
                "body": "Body of handoff.",
            })
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        let error = call(&handlers, "rlm.skills.read", json!({ "name": "missing" }))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "rlm.skills.read unknown skill \"missing\"; find it with rlm.skills.search"
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "a failed read is not a use"
        );
    }
}
