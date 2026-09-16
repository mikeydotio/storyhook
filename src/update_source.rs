//! Installation source metadata bound to the exact installed executable.

use crate::error::AppError;
use crate::github_access::ReleaseSource;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    version: u32,
    source: String,
    sha256: String,
}

/// Adjacent metadata path for the resolved executable.
pub(crate) fn sidecar(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_os_string();
    name.push(".source.json");
    PathBuf::from(name)
}

fn digest(exe: &Path) -> Result<String, AppError> {
    let mut file = fs::File::open(exe)?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Loads matching metadata or validates an explicit recovery source.
pub(crate) fn resolve(exe: &Path, explicit: Option<&str>) -> Result<ReleaseSource, AppError> {
    if let Some(value) = explicit {
        return ReleaseSource::parse(value);
    }
    let read = || -> Result<ReleaseSource, AppError> {
        let metadata: Metadata = serde_json::from_slice(&fs::read(sidecar(exe))?)
            .map_err(|e| AppError::Validation(format!("invalid source metadata: {e}")))?;
        if metadata.version != 1 || metadata.sha256 != digest(exe)? {
            return Err(AppError::Validation(
                "source metadata does not match this binary".into(),
            ));
        }
        ReleaseSource::parse(&metadata.source)
    };
    read().map_err(|e| e.with_context("cannot establish installation source; use story update --source HOST/OWNER/REPO (add --force to reinstall)"))
}

/// Writes source metadata for a staged binary before publication.
pub(crate) fn stage(
    exe: &Path,
    source: &ReleaseSource,
    destination: &Path,
) -> Result<(), AppError> {
    let metadata = Metadata {
        version: 1,
        source: source.qualified(),
        sha256: digest(exe)?,
    };
    let bytes = serde_json::to_vec(&metadata).map_err(|e| AppError::Storage(e.to_string()))?;
    fs::write(destination, bytes)?;
    Ok(())
}

/// Serializes binary and source publication with the bootstrap installer.
pub(crate) fn lock(exe: &Path) -> Result<fs::File, AppError> {
    use fs4::FileExt;
    let mut name = exe.as_os_str().to_os_string();
    name.push(".install.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(name)?;
    file.try_lock_exclusive().map_err(|e| {
        AppError::Storage(format!(
            "another installation holds the update lock for {}: {e}",
            exe.display()
        ))
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_bound_to_binary_and_explicit_recovery_is_required() {
        let dir = storyhook_test_support::scratch_dir();
        let exe = dir.path().join("story");
        fs::write(&exe, b"first binary").unwrap();
        assert!(
            resolve(&exe, None)
                .unwrap_err()
                .to_string()
                .contains("--source")
        );
        let source = ReleaseSource::parse("github.pie.apple.com/acme/storyhook").unwrap();
        stage(&exe, &source, &sidecar(&exe)).unwrap();
        assert_eq!(resolve(&exe, None).unwrap().qualified(), source.qualified());
        fs::write(&exe, b"replacement after interrupted publication").unwrap();
        assert!(resolve(&exe, None).is_err());
        assert!(resolve(&exe, Some("github.com/other/storyhook")).is_ok());
        fs::write(sidecar(&exe), b"bad json").unwrap();
        assert!(resolve(&exe, None).is_err());
    }

    #[test]
    fn installer_lock_refuses_concurrent_publication_and_releases_on_drop() {
        let dir = storyhook_test_support::scratch_dir();
        let exe = dir.path().join("story");
        let held = lock(&exe).unwrap();
        assert!(lock(&exe).is_err());
        drop(held);
        assert!(lock(&exe).is_ok());
    }
}
