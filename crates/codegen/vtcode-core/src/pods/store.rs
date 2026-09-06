use super::catalogue::PodCatalogue;
use super::state::PodsState;
use anyhow::{Context, Result};
use std::path::PathBuf;
use vtcode_commons::VtCodePaths;
use vtcode_commons::fs::{read_private_json_file, write_private_json_file};

/// Persisted pod storage rooted in the user state directory's `pods` path.
#[derive(Debug, Clone)]
pub struct PodsStore {
    base_dir: PathBuf,
}

impl PodsStore {
    /// Create a store rooted at the given directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into() }
    }

    /// Create a store using the default user-state `pods` directory.
    pub fn default_store() -> Result<Self> {
        let paths = VtCodePaths::resolve().context("failed to resolve VT Code paths")?;
        Ok(Self::new(paths.state_path("pods")?))
    }

    /// Return the path to the `state.json` file.
    pub fn state_path(&self) -> PathBuf {
        self.base_dir.join("state.json")
    }

    /// Return the path to the `catalog.json` file.
    pub fn catalogue_path(&self) -> PathBuf {
        self.base_dir.join("catalog.json")
    }

    /// Create the base directory and seed default files if they do not exist.
    pub async fn ensure_initialized(&self) -> Result<()> {
        ensure_user_dir(&self.base_dir)?;

        if !tokio::fs::try_exists(&self.catalogue_path()).await.unwrap_or(false) {
            self.save_catalogue(&PodCatalogue::embedded_default()).await?;
        }

        if !tokio::fs::try_exists(&self.state_path()).await.unwrap_or(false) {
            self.save_state(&PodsState::default()).await?;
        }

        Ok(())
    }

    /// Load the persisted pod state from disk.
    pub async fn load_state(&self) -> Result<PodsState> {
        self.ensure_initialized().await?;
        read_private_json_file(&self.state_path())
            .await
            .with_context(|| format!("failed to read pod state at {}", self.state_path().display()))
    }

    /// Persist the pod state to disk.
    pub async fn save_state(&self, state: &PodsState) -> Result<()> {
        ensure_user_dir(&self.base_dir)?;
        write_private_json_file(&self.state_path(), state)
            .await
            .with_context(|| format!("failed to write pod state at {}", self.state_path().display()))
    }

    /// Load the deployment catalogue from disk.
    pub async fn load_catalogue(&self) -> Result<PodCatalogue> {
        self.ensure_initialized().await?;
        read_private_json_file(&self.catalogue_path())
            .await
            .with_context(|| format!("failed to read pod catalogue at {}", self.catalogue_path().display()))
    }

    pub async fn save_catalogue(&self, catalogue: &PodCatalogue) -> Result<()> {
        ensure_user_dir(&self.base_dir)?;
        write_private_json_file(&self.catalogue_path(), catalogue)
            .await
            .with_context(|| format!("failed to write pod catalogue at {}", self.catalogue_path().display()))
    }
}

fn ensure_user_dir(path: &std::path::Path) -> Result<()> {
    VtCodePaths::ensure_user_dir(path).context("failed to create private pod state directory")?;
    Ok(())
}
