//! Shared literal search semantics for the CLI, terminal and local catalog.

use bb_api::models::{Project, Repository};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CatalogKind {
    Repositories,
    Projects,
}

impl CatalogKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Repositories => "Repositories",
            Self::Projects => "Projects",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Repositories => "repositories",
            Self::Projects => "projects",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogQuery {
    pub workspace: String,
    pub kind: CatalogKind,
    pub project: Option<String>,
    pub text: String,
}

impl CatalogQuery {
    pub fn path(&self) -> String {
        let workspace = crate::app::encode(&self.workspace);
        match self.kind {
            CatalogKind::Repositories => format!("repositories/{workspace}"),
            CatalogKind::Projects => format!("workspaces/{workspace}/projects"),
        }
    }

    pub fn parameters(&self) -> Vec<(String, String)> {
        let mut filters = Vec::new();
        if self.kind == CatalogKind::Repositories
            && let Some(project) = &self.project
        {
            filters.push(format!("project.key={}", crate::app::quoted(project)));
        }
        let text = self.text.trim();
        if !text.is_empty() {
            let fields: &[&str] = match self.kind {
                CatalogKind::Repositories => {
                    &["name", "slug", "full_name", "project.key", "project.name"]
                }
                CatalogKind::Projects => &["name", "key"],
            };
            let literal = crate::app::quoted(text);
            let terms: Vec<_> = fields
                .iter()
                .map(|field| format!("{field} ~ {literal}"))
                .collect();
            filters.push(format!("({})", terms.join(" OR ")));
        }
        let mut parameters = vec![
            ("pagelen".into(), "50".into()),
            ("sort".into(), "name".into()),
        ];
        if !filters.is_empty() {
            parameters.push(("q".into(), filters.join(" AND ")));
        }
        parameters
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum CatalogItem {
    Repository(Box<Repository>),
    Project(Project),
}

impl CatalogItem {
    pub fn key(&self) -> &str {
        match self {
            Self::Repository(repository) => &repository.full_name,
            Self::Project(project) => &project.key,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Repository(repository) => &repository.name,
            Self::Project(project) => &project.name,
        }
    }

    pub fn kind(&self) -> CatalogKind {
        match self {
            Self::Repository(_) => CatalogKind::Repositories,
            Self::Project(_) => CatalogKind::Projects,
        }
    }

    pub fn project(&self) -> Option<&str> {
        match self {
            Self::Repository(repository) => repository
                .project
                .as_ref()
                .map(|project| project.key.as_str()),
            Self::Project(_) => None,
        }
    }

    pub fn search_text(&self) -> String {
        // NUL boundaries prevent a literal query from accidentally spanning two fields.
        match self {
            Self::Repository(repository) => format!(
                "{}\0{}\0{}\0{}\0{}",
                repository.name,
                repository.slug,
                repository.full_name,
                repository
                    .project
                    .as_ref()
                    .map_or("", |project| &project.key),
                repository
                    .project
                    .as_ref()
                    .map_or("", |project| &project.name)
            ),
            Self::Project(project) => format!("{}\0{}", project.name, project.key),
        }
        .to_lowercase()
    }
}
