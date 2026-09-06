#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use std::{
    env, fs, panic,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{CommandFactory, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(about = "Repository automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Build a release archive with man page and shell completions.
    PackageRelease(PackageReleaseArgs),
    /// Compute and write the next workspace version.
    BumpVersion(BumpVersionArgs),
    /// Verify all inter-crate version pins match the workspace version.
    CheckVersions,
}

#[derive(Debug, Parser)]
struct PackageReleaseArgs {
    /// Rust target triple used to name the output archive.
    #[arg(long)]
    target: String,

    /// Version string without the leading v.
    #[arg(long)]
    version: String,

    /// Path to the already-built release binary for the selected target.
    #[arg(long)]
    binary: PathBuf,

    /// Output directory for generated artefacts and final archives.
    #[arg(long, default_value = "dist")]
    out_dir: PathBuf,
}

#[derive(Debug, Parser)]
struct BumpVersionArgs {
    /// Increment the given component (mutually exclusive with --set).
    #[arg(long, value_enum, conflicts_with = "set")]
    bump: Option<BumpKind>,

    /// Set an explicit version string (e.g. "1.2.3").
    #[arg(long, conflicts_with = "bump")]
    set: Option<semver::Version>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum BumpKind {
    Major,
    Minor,
    Patch,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::PackageRelease(args) => package_release(args)?,
        Commands::BumpVersion(args) => bump_version(args)?,
        Commands::CheckVersions => check_versions()?,
    }

    Ok(())
}

fn package_release(args: PackageReleaseArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("..");
    let out_dir = workspace_root.join(&args.out_dir);
    let stage_dir = out_dir.join(format!("vtcode-{}-v{}", args.target, args.version));

    if stage_dir.exists() {
        fs::remove_dir_all(&stage_dir)?;
    }
    fs::create_dir_all(&stage_dir)?;

    // Keep the binary at archive root so the native updater can discover it.
    fs::copy(workspace_root.join(&args.binary), stage_dir.join("vtcode"))?;

    // Extra files in subdirectories for cargo-binstall and install.sh.
    fs::create_dir_all(stage_dir.join("man/man1"))?;
    fs::create_dir_all(stage_dir.join("completions/bash"))?;
    fs::create_dir_all(stage_dir.join("completions/fish"))?;
    fs::create_dir_all(stage_dir.join("completions/zsh"))?;

    let man_page = vtcode_core::cli::ManPageGenerator::generate_main_man_page()?;
    fs::write(stage_dir.join("man/man1/vtcode.1"), man_page)?;

    match generate_completions(&stage_dir) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("warning: skipping shell completions: {e}");
            let _ = fs::remove_dir_all(stage_dir.join("completions"));
        }
    }

    let archive = out_dir.join(format!("vtcode-{}-v{}.tar.gz", args.target, args.version));

    if archive.exists() {
        fs::remove_file(&archive)?;
    }

    create_archive(&workspace_root, &out_dir, &archive, &stage_dir)?;
    println!("{}", archive.display());
    Ok(())
}

fn generate_completions(stage_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let result = panic::catch_unwind(|| {
        let mut cmd = vtcode_core::Cli::command();
        cmd.build();
        cmd
    });

    let mut cmd = match result {
        Ok(cmd) => cmd,
        Err(_) => {
            return Err("completion generation not supported with current CLI definition".into());
        }
    };

    for (shell, dir, filename) in [
        (clap_complete::Shell::Bash, "completions/bash", "vtcode"),
        (clap_complete::Shell::Fish, "completions/fish", "vtcode.fish"),
        (clap_complete::Shell::Zsh, "completions/zsh", "_vtcode"),
    ] {
        let mut output = Vec::new();
        clap_complete::generate(shell, &mut cmd, "vtcode", &mut output);
        fs::write(stage_dir.join(dir).join(filename), output)?;
    }

    Ok(())
}

const RELEASE_ROOT_ENTRIES: &[&str] = &["completions", "man", "vtcode"];

fn reject_workspace_instruction_files(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        if matches!(entry.file_name().to_str(), Some("AGENTS.md" | "CLAUDE.md")) {
            return Err(format!("workspace instruction file in release stage: {}", entry_path.display()).into());
        }
        if entry.file_type()?.is_dir() {
            reject_workspace_instruction_files(&entry_path)?;
        }
    }

    Ok(())
}

fn create_archive(
    _workspace_root: &Path,
    out_dir: &Path,
    archive: &Path,
    stage_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build list of entries from the stage directory.
    let mut entries: Vec<String> = Vec::new();
    for entry in fs::read_dir(stage_dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        entries.push(name);
    }
    entries.sort();

    for entry in &entries {
        if !RELEASE_ROOT_ENTRIES.contains(&entry.as_str()) {
            return Err(format!("unexpected release entry `{entry}`").into());
        }
    }
    reject_workspace_instruction_files(stage_dir)?;

    // Archive from stage_dir parent so entries have no directory prefix.
    // The native updater discovers the executable by its exact name.
    let mut cmd = Command::new("tar");
    cmd.current_dir(out_dir).arg("-czf").arg(archive).arg("-C").arg(stage_dir);
    for entry in &entries {
        cmd.arg(entry);
    }

    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err("tar command failed".into())
    }
}

fn bump_version(args: BumpVersionArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("..");
    let cargo_toml_path = workspace_root.join("Cargo.toml");

    let content = fs::read_to_string(&cargo_toml_path)?;
    let mut doc: toml_edit::DocumentMut = content.parse().map_err(|e| format!("Failed to parse Cargo.toml: {e}"))?;

    let current_str = doc["workspace"]["package"]["version"]
        .as_str()
        .ok_or("[workspace.package] version not found in root Cargo.toml")?
        .to_string();
    let current = semver::Version::parse(&current_str)?;

    let new_version = match (args.bump, args.set) {
        (Some(kind), None) => {
            let mut v = current.clone();
            match kind {
                BumpKind::Major => {
                    v.major += 1;
                    v.minor = 0;
                    v.patch = 0;
                }
                BumpKind::Minor => {
                    v.minor += 1;
                    v.patch = 0;
                }
                BumpKind::Patch => {
                    v.patch += 1;
                }
            }
            v.pre = semver::Prerelease::EMPTY;
            v.build = semver::BuildMetadata::EMPTY;
            v
        }
        (None, Some(v)) => v,
        _ => return Err("Specify exactly one of --bump or --set".into()),
    };

    let new_str = new_version.to_string();

    // Update both version locations: [package].version and [workspace.package].version.
    doc["workspace"]["package"]["version"] = toml_edit::value(&new_str);
    doc["package"]["version"] = toml_edit::value(&new_str);

    // Update hardcoded inter-crate version pins in the root [dependencies] section.
    // These use the pattern: vtcode-core = { path = "...", version = "0.123.7" }
    if let Some(deps) = doc["dependencies"].as_table_like_mut() {
        for (_dep_name, entry) in deps.iter_mut() {
            if let Some(dep_table) = entry.as_table_like_mut() {
                let has_path = dep_table.get("path").and_then(|v| v.as_str()).is_some();
                let ver = dep_table.get("version").and_then(|v| v.as_str());
                if has_path && ver == Some(current_str.as_str()) {
                    dep_table.insert("version", toml_edit::value(&new_str));
                }
            }
        }
    }

    fs::write(&cargo_toml_path, doc.to_string())?;

    println!("{current_str} -> {new_str}");

    // Verify the bump produced consistent version pins.
    check_versions_inner(&workspace_root)
}

fn check_versions() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("..");
    check_versions_inner(&workspace_root)
}

/// Core version consistency check. Used by both `check-versions` and `bump-version`.
fn check_versions_inner(workspace_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let root_cargo_toml = workspace_root.join("Cargo.toml");

    let content = fs::read_to_string(&root_cargo_toml)?;
    let doc: toml_edit::DocumentMut = content.parse()?;

    let workspace_version_str = doc["workspace"]["package"]["version"]
        .as_str()
        .ok_or("[workspace.package] version not found in root Cargo.toml")?;
    let workspace_version = semver::Version::parse(workspace_version_str)?;

    // Collect member paths from [workspace].members.
    let members: Vec<String> = doc["workspace"]["members"]
        .as_array()
        .ok_or("[workspace].members not found in root Cargo.toml")?
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    let mut mismatches: Vec<String> = Vec::new();

    // Check root Cargo.toml dependencies.
    check_file_deps(&root_cargo_toml, &workspace_version, &mut mismatches)?;

    // Check each member crate.
    for member in &members {
        let member_path = workspace_root.join(member).join("Cargo.toml");
        if member_path.exists() {
            check_file_deps(&member_path, &workspace_version, &mut mismatches)?;
        }
    }

    if mismatches.is_empty() {
        println!("All inter-crate version pins match workspace version {workspace_version}");
        Ok(())
    } else {
        for m in &mismatches {
            eprintln!("ERROR: {m}");
        }
        Err(format!(
            "{} version mismatch(es) found. Expected all pins to match {workspace_version}.",
            mismatches.len(),
        )
        .into())
    }
}

/// Scan `[dependencies]`, `[dev-dependencies]`, and `[build-dependencies]` in a
/// Cargo.toml file for inter-crate path dependencies whose `version` pin does
/// not match the expected workspace version.
fn check_file_deps(
    path: &Path,
    workspace_version: &semver::Version,
    mismatches: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let toml_content = fs::read_to_string(path)?;
    let toml_doc: toml_edit::DocumentMut = toml_content.parse()?;

    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(table) = toml_doc.get(section).and_then(|item| item.as_table_like()) else {
            continue;
        };
        for (dep_name, entry) in table.iter() {
            // Handle inline tables: vtcode-core = { path = "...", version = "..." }
            // Handle standard tables: [dependencies.vtcode-core] with path/version keys.
            let dep_table = entry
                .as_inline_table()
                .map(|t| t as &dyn toml_edit::TableLike)
                .or_else(|| entry.as_table_like());

            let Some(dep_table) = dep_table else {
                continue;
            };

            let has_path = dep_table.get("path").and_then(|v| v.as_str()).is_some();
            if !has_path {
                continue;
            }

            let version_item = dep_table.get("version");

            // Skip `version.workspace = true` entries -- they auto-track and
            // cannot drift. In TOML this appears as a subtable { workspace = true }
            // rather than a plain string value.
            if let Some(item) = version_item
                && item.as_table_like().and_then(|t| t.get("workspace")).and_then(|v| v.as_bool()) == Some(true)
            {
                continue;
            }

            if let Some(ver_str) = version_item.and_then(|v| v.as_str()) {
                let req = semver::VersionReq::parse(ver_str).map_err(|e| {
                    format!("Invalid version req '{ver_str}' for {dep_name} in {}: {e}", path.display())
                })?;
                if !req.matches(workspace_version) {
                    mismatches.push(format!(
                        "{}: {} version \"{ver_str}\" does not match workspace version \
                         {workspace_version}",
                        path.display(),
                        dep_name,
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock").as_nanos();
        env::temp_dir().join(format!("vtcode-xtask-{label}-{}-{timestamp}", std::process::id()))
    }

    #[test]
    fn release_archive_contains_only_distribution_assets() {
        let root = test_root("archive-smoke");
        let stage_dir = root.join("stage");
        let out_dir = root.join("out");
        fs::create_dir_all(stage_dir.join("man/man1")).expect("man directory");
        fs::create_dir_all(stage_dir.join("completions/bash")).expect("completion directory");
        fs::create_dir_all(&out_dir).expect("output directory");
        fs::write(stage_dir.join("vtcode"), "binary").expect("binary");
        fs::write(stage_dir.join("man/man1/vtcode.1"), "man page").expect("man page");
        fs::write(stage_dir.join("completions/bash/vtcode"), "completion").expect("completion");

        let archive = out_dir.join("vtcode-smoke.tar.gz");
        create_archive(&root, &out_dir, &archive, &stage_dir).expect("archive");

        let listing = Command::new("tar")
            .args(["-tzf", archive.to_str().expect("archive path")])
            .output()
            .expect("tar listing");
        assert!(listing.status.success(), "tar listing failed: {:?}", listing.status);
        let entries = String::from_utf8_lossy(&listing.stdout);
        let normalized_entries = entries.lines().map(|entry| entry.trim_end_matches('/'));
        for entry in normalized_entries {
            assert!(
                RELEASE_ROOT_ENTRIES
                    .iter()
                    .any(|root_entry| { entry == *root_entry || entry.starts_with(&format!("{root_entry}/")) }),
                "unexpected archive entry: {entry}"
            );
            assert!(!entry.ends_with("AGENTS.md"));
        }
        assert!(entries.lines().any(|entry| entry.trim_end_matches('/') == "vtcode"));
        assert!(entries.lines().any(|entry| entry.ends_with("man/man1/vtcode.1")));
        assert!(entries.lines().any(|entry| entry.ends_with("completions/bash/vtcode")));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn release_archive_rejects_unexpected_instruction_files() {
        let root = test_root("archive-allowlist");
        let stage_dir = root.join("stage");
        let out_dir = root.join("out");
        fs::create_dir_all(&stage_dir).expect("stage directory");
        fs::create_dir_all(&out_dir).expect("output directory");
        fs::write(stage_dir.join("vtcode"), "binary").expect("binary");
        fs::write(stage_dir.join("AGENTS.md"), "maintainer guidance").expect("instruction file");

        let archive = out_dir.join("vtcode-allowlist.tar.gz");
        let error = create_archive(&root, &out_dir, &archive, &stage_dir).expect_err("unexpected file must fail");
        assert!(error.to_string().contains("unexpected release entry `AGENTS.md`"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn release_archive_rejects_nested_instruction_files() {
        let root = test_root("archive-nested-allowlist");
        let stage_dir = root.join("stage");
        let out_dir = root.join("out");
        fs::create_dir_all(stage_dir.join("man/man1")).expect("man directory");
        fs::create_dir_all(&out_dir).expect("output directory");
        fs::write(stage_dir.join("vtcode"), "binary").expect("binary");
        fs::write(stage_dir.join("man/AGENTS.md"), "maintainer guidance").expect("instruction file");

        let archive = out_dir.join("vtcode-nested-allowlist.tar.gz");
        let error = create_archive(&root, &out_dir, &archive, &stage_dir).expect_err("nested file must fail");
        assert!(error.to_string().contains("workspace instruction file in release stage"));

        fs::remove_dir_all(root).expect("cleanup");
    }
}
