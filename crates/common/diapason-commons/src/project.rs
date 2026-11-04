#![expect(
    clippy::let_underscore_must_use,
    reason = "Project rendering intentionally ignores infallible writes into String buffers."
)]

//! Project-related utilities and structures

use crate::utils::{extract_readme_excerpt, extract_toml_str};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Lightweight project overview extracted from workspace files
pub struct ProjectOverview {
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub readme_excerpt: Option<String>,
    pub root: PathBuf,
}

impl ProjectOverview {
    pub fn short_for_display(&self) -> String {
        let mut out = String::new();
        if let Some(name) = &self.name {
            let _ = write!(out, "Project: {name}");
        }
        if let Some(ver) = &self.version {
            if !out.is_empty() {
                out.push(' ');
            }
            let _ = write!(out, "v{ver}");
        }
        if !out.is_empty() {
            out.push('\n');
        }
        if let Some(desc) = &self.description {
            out.push_str(desc);
            out.push('\n');
        }
        let _ = write!(out, "Root: {}", self.root.display());
        out
    }

    pub fn as_prompt_block(&self) -> String {
        let mut s = String::new();
        if let Some(name) = &self.name {
            let _ = writeln!(s, "- Name: {name}");
        }
        if let Some(ver) = &self.version {
            let _ = writeln!(s, "- Version: {ver}");
        }
        if let Some(desc) = &self.description {
            let _ = writeln!(s, "- Description: {desc}");
        }
        let _ = writeln!(s, "- Workspace Root: {}", self.root.display());
        if let Some(excerpt) = &self.readme_excerpt {
            s.push_str("- README Excerpt: \n");
            s.push_str(excerpt);
            if !excerpt.ends_with('\n') {
                s.push('\n');
            }
        }
        s
    }
}

/// Build a minimal project overview from Cargo.toml and README.md
pub async fn build_project_overview(root: &Path) -> Option<ProjectOverview> {
    let cargo_toml_path = root.join("Cargo.toml");
    let readme_path = root.join("README.md");
    let (cargo_toml, readme) = tokio::join!(fs::read_to_string(cargo_toml_path), fs::read_to_string(readme_path));

    let metadata = cargo_toml.ok().map(|contents| ProjectMetadata::from_cargo_toml(&contents));
    let readme_excerpt = match readme {
        Ok(contents) => Some(extract_readme_excerpt(&contents, 1200)),
        Err(_) => read_readme_fallback(root).await,
    };

    let overview = ProjectOverview {
        name: metadata.as_ref().and_then(|metadata| metadata.name.clone()),
        version: metadata.as_ref().and_then(|metadata| metadata.version.clone()),
        description: metadata.and_then(|metadata| metadata.description),
        readme_excerpt,
        root: root.to_path_buf(),
    };

    (overview.name.is_some()
        || overview.version.is_some()
        || overview.description.is_some()
        || overview.readme_excerpt.is_some())
    .then_some(overview)
}

struct ProjectMetadata {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
}

impl ProjectMetadata {
    fn from_cargo_toml(contents: &str) -> Self {
        Self {
            name: extract_toml_str(contents, "name"),
            version: extract_toml_str(contents, "version"),
            description: extract_toml_str(contents, "description"),
        }
    }
}

async fn read_readme_fallback(root: &Path) -> Option<String> {
    for alt in ["QUICKSTART.md", "user-context.md"] {
        let path = root.join(alt);
        if let Ok(contents) = fs::read_to_string(path).await {
            return Some(extract_readme_excerpt(&contents, 800));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn overview_keeps_metadata_only_projects() {
        let root = tempdir().expect("temporary project directory");
        fs::write(root.path().join("Cargo.toml"), "version = \"1.2.3\"\ndescription = \"metadata\"\n")
            .expect("write Cargo.toml");

        let overview = build_project_overview(root.path())
            .await
            .expect("metadata should produce an overview");

        assert_eq!(overview.version.as_deref(), Some("1.2.3"));
        assert_eq!(overview.description.as_deref(), Some("metadata"));
    }
}
