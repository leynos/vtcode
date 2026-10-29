//! Extraction of downloaded `ast-grep` release archives.
//!
//! Supports both `.zip` and `.tar.gz` assets. Each archive is unpacked into a
//! temporary directory, the extracted binary is validated and renamed to the
//! canonical `ast-grep` name, then installed under `InstallPaths::bin_dir`.
//! Archive entry names are untrusted input: traversal and symlink escapes are
//! rejected before anything is written outside the staging directory.
//!
//! `super` orchestrates the download and chooses the asset; this module owns
//! only the byte-to-binary step and reports failures through `anyhow` so the
//! installer can decide whether to fall back.

use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tools::ast_grep_binary::{alias_ast_grep_binary_name, canonical_ast_grep_binary_name};
use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use tar::Archive;
use tempfile::{NamedTempFile, TempDir};
use vtcode_commons::VtCodePaths;
use vtcode_commons::walk::build_walker_single_threaded;
use zip::ZipArchive;

use super::state::InstallPaths;

pub(super) fn install_archive(paths: &InstallPaths, archive_name: &str, archive_bytes: &[u8]) -> Result<()> {
    VtCodePaths::ensure_user_dir(&paths.bin_dir)
        .with_context(|| format!("Failed to create {}", paths.bin_dir.display()))?;

    let temp_dir = TempDir::new().context("Failed to create temp directory for ast-grep install")?;
    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir).with_context(|| format!("Failed to create {}", extract_dir.display()))?;

    extract_archive(archive_name, archive_bytes, &extract_dir)?;
    let extracted_binary = find_extracted_binary(&extract_dir)?;

    install_binary_atomically(&extracted_binary, &paths.binary_path, &paths.bin_dir)?;

    if cfg!(target_os = "linux") {
        let stale_alias = paths.bin_dir.join("sg");
        if stale_alias.exists() {
            let _ = fs::remove_file(stale_alias);
        }
    } else if let Some(alias_path) = &paths.alias_path {
        install_binary_atomically(&paths.binary_path, alias_path, &paths.bin_dir)
            .with_context(|| format!("Failed to install ast-grep alias to {}", alias_path.display()))?;
    }

    Ok(())
}

pub(super) fn ast_grep_version(binary: &Path) -> Result<String> {
    let output = Command::new(binary)
        .arg("--version")
        .arg("--color")
        .arg("never")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .output()
        .with_context(|| format!("Failed to run {}", binary.display()))?;
    if !output.status.success() {
        bail!("{} --version exited with status {}", binary.display(), output.status);
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        bail!("{} --version returned empty output", binary.display());
    }
    Ok(version)
}

/// Extract a supported release archive into `destination`.
///
/// Names ending in `.tar.gz` are unpacked as tarballs; names ending in `.zip`
/// are unpacked entry by entry, using each ZIP entry's mangled path beneath
/// the destination. Unsupported suffixes and read, path, or filesystem errors
/// are returned with archive or destination context.
fn extract_archive(archive_name: &str, archive_bytes: &[u8], destination: &Path) -> Result<()> {
    if archive_name.ends_with(".tar.gz") {
        let decoder = GzDecoder::new(Cursor::new(archive_bytes));
        let mut archive = Archive::new(decoder);
        archive
            .unpack(destination)
            .with_context(|| format!("Failed to unpack {archive_name}"))?;
        return Ok(());
    }

    if archive_name.ends_with(".zip") {
        let cursor = Cursor::new(archive_bytes);
        let mut archive = ZipArchive::new(cursor).with_context(|| format!("Failed to open {archive_name}"))?;
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .with_context(|| format!("Failed to read {archive_name} entry {index}"))?;
            let outpath = destination.join(
                file.mangled_name()
                    .with_context(|| format!("Failed to validate {archive_name} entry path {index}"))?,
            );

            if file.is_dir() {
                fs::create_dir_all(&outpath).with_context(|| format!("Failed to create {}", outpath.display()))?;
                continue;
            }

            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
            }

            let mut outfile =
                File::create(&outpath).with_context(|| format!("Failed to create {}", outpath.display()))?;
            io::copy(&mut file, &mut outfile).with_context(|| format!("Failed to extract {}", outpath.display()))?;
        }
        return Ok(());
    }

    bail!("Unsupported ast-grep archive format: {archive_name}");
}

fn find_extracted_binary(root: &Path) -> Result<PathBuf> {
    let alias_name = alias_ast_grep_binary_name();
    build_walker_single_threaded(root)
        .build()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .find_map(|entry| {
            let name = entry.file_name().to_string_lossy();
            if name == canonical_ast_grep_binary_name() || alias_name.is_some_and(|alias| name == alias) {
                Some(entry.into_path())
            } else {
                None
            }
        })
        .ok_or_else(|| anyhow!("ast-grep binary not found in release archive"))
}

/// Installs `source` at `destination` without ever exposing a partially
/// written file at `destination`.
///
/// The extracted binary is first staged into a `NamedTempFile` created in
/// `destination_dir` (the same directory as `destination`, and therefore the
/// same filesystem), so the final `persist` is a single atomic rename rather
/// than a byte-by-byte copy into the live path. This prevents two concurrent
/// installers -- or a process killed mid-copy -- from leaving a corrupted or
/// half-written binary at `destination`.
fn install_binary_atomically(source: &Path, destination: &Path, destination_dir: &Path) -> Result<()> {
    let mut source_file =
        File::open(source).with_context(|| format!("Failed to open extracted binary {}", source.display()))?;

    let mut staged = NamedTempFile::new_in(destination_dir)
        .with_context(|| format!("Failed to create staging file in {}", destination_dir.display()))?;

    io::copy(&mut source_file, staged.as_file_mut())
        .with_context(|| format!("Failed to stage ast-grep binary at {}", staged.path().display()))?;

    set_executable_permissions(staged.path())?;

    staged
        .persist(destination)
        .with_context(|| format!("Failed to publish ast-grep binary to {}", destination.display()))?;

    Ok(())
}

fn set_executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .with_context(|| format!("Failed to inspect {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("Failed to update permissions for {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_file_bytes(entry_name: &str, contents: &[u8]) -> Result<Vec<u8>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file(entry_name, options)?;
        writer.write_all(contents)?;
        Ok(writer.finish()?.into_inner())
    }

    #[test]
    fn extracts_zip_file_into_nested_directory() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("extracted");
        fs::create_dir(&destination)?;
        let zip_bytes = zip_file_bytes("nested/payload.txt", b"archive payload")?;

        extract_archive("fixture.zip", &zip_bytes, &destination)?;

        assert_eq!(fs::read(destination.join("nested/payload.txt"))?, b"archive payload");
        Ok(())
    }

    #[test]
    fn sanitizes_zip_parent_traversal_without_overwriting_outside_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("extracted");
        fs::create_dir(&destination)?;
        let outside_path = temp.path().join("outside.txt");
        fs::write(&outside_path, b"original sentinel")?;
        let zip_bytes = zip_file_bytes("../outside.txt", b"archive payload")?;

        extract_archive("fixture.zip", &zip_bytes, &destination)?;

        assert_eq!(fs::read(outside_path)?, b"original sentinel");
        assert_eq!(fs::read(destination.join("outside.txt"))?, b"archive payload");
        Ok(())
    }
}
