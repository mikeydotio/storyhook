//! Cargo target and executable observations are not causal authority.

mod executable;
use super::{RustCase, RustTarget};
pub use executable::RustExecutable;
use serde::Deserialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

const JSON_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct Metadata {
    version: u32,
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    source: Option<String>,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Target {
    kind: Vec<String>,
    crate_types: Vec<String>,
    name: String,
    src_path: PathBuf,
    edition: String,
    test: bool,
}

/// An exact native Cargo target resolved within an owned materialization.
/// These parsed facts are useful only when the executor establishes their origin.
pub struct CargoTarget {
    package: String,
    manifest: PathBuf,
    target: Target,
}

impl CargoTarget {
    /// Resolve exactly one local package and target, with the standard test harness.
    pub fn resolve(root: &Path, case: &RustCase, metadata: &[u8]) -> Result<Self, String> {
        if metadata.len() > JSON_LIMIT {
            return Err("Cargo metadata exceeds observation limit".into());
        }
        let metadata: Metadata =
            serde_json::from_slice(metadata).map_err(|e| format!("Cargo metadata: {e}"))?;
        let canonical = root
            .canonicalize()
            .map_err(|e| format!("Cargo input root: {e}"))?;
        if metadata.version != 1 || metadata.workspace_root != canonical {
            return Err("Cargo metadata has a foreign workspace or unsupported version".into());
        }
        let packages: Vec<_> = metadata
            .packages
            .iter()
            .filter(|p| p.name == case.package)
            .collect();
        let [package] = packages.as_slice() else {
            return Err("Cargo package is absent or ambiguous".into());
        };
        if package.source.is_some()
            || package.id.is_empty()
            || metadata
                .workspace_members
                .iter()
                .filter(|id| **id == package.id)
                .count()
                != 1
        {
            return Err("Cargo package is not one local workspace member".into());
        }
        let manifest = regular_within(root, &package.manifest_path)?;
        let targets: Vec<_> = package
            .targets
            .iter()
            .filter(|t| match &case.target {
                RustTarget::Library => t.kind == ["lib"],
                RustTarget::Integration(name) => t.kind == ["test"] && &t.name == name,
                RustTarget::Binary(name) => t.kind == ["bin"] && &t.name == name,
            })
            .collect();
        let [target] = targets.as_slice() else {
            return Err("Cargo target is absent or ambiguous".into());
        };
        if !target.test
            || !matches!(target.edition.as_str(), "2015" | "2018" | "2021" | "2024")
            || target.crate_types
                != [if matches!(case.target, RustTarget::Library) {
                    "lib"
                } else {
                    "bin"
                }]
        {
            return Err("Cargo target does not use a supported native test profile".into());
        }
        regular_within(root, &target.src_path)?;
        native_manifest(&manifest, &case.target, target)?;
        Ok(Self {
            package: package.id.clone(),
            manifest: package.manifest_path.clone(),
            target: (*target).clone(),
        })
    }

    /// Require a complete successful build and one matching native test executable.
    /// Arbitrary tool output, failed builds and duplicate matching artifacts are refused.
    pub fn executable(
        &self,
        output: &Path,
        stream: &[u8],
        truncated: bool,
        exit: Option<i32>,
        deadline: Instant,
    ) -> Result<RustExecutable, String> {
        if truncated || exit != Some(0) || stream.len() > JSON_LIMIT {
            return Err("Cargo build did not complete with a full successful observation".into());
        }
        let text =
            std::str::from_utf8(stream).map_err(|e| format!("Cargo build is not UTF-8: {e}"))?;
        let mut finished = false;
        let mut path = None;
        for line in text.lines() {
            if finished {
                return Err("Cargo emitted output after build-finished".into());
            }
            let message: CargoEvent = serde_json::from_str(line)
                .map_err(|e| format!("unsupported Cargo message: {e}"))?;
            match message {
                CargoEvent::BuildFinished { success: true } => finished = true,
                CargoEvent::BuildFinished { success: false } => {
                    return Err("Cargo build failed".into());
                }
                CargoEvent::CompilerArtifact {
                    package_id,
                    manifest_path,
                    target,
                    profile,
                    executable,
                    filenames,
                } => {
                    if target.name != self.target.name || target.kind != self.target.kind {
                        continue;
                    }
                    if package_id != self.package {
                        continue;
                    }
                    if *target != self.target || manifest_path != self.manifest || !profile.test {
                        return Err("Cargo test artifact does not match the resolved target".into());
                    }
                    let executable = executable.ok_or("Cargo test artifact has no executable")?;
                    if filenames.iter().filter(|p| **p == executable).count() != 1
                        || path.replace(executable).is_some()
                    {
                        return Err(
                            "Cargo test artifact is absent from filenames or ambiguous".into()
                        );
                    }
                }
                CargoEvent::CompilerMessage { message } => {
                    if matches!(message.level.as_str(), "error" | "warning" | "failure-note") {
                        return Err(format!(
                            "Cargo compiler diagnostic prevents diagnosis: {}",
                            message.level
                        ));
                    }
                }
                CargoEvent::BuildScriptExecuted {} => {}
            }
        }
        if !finished {
            return Err("Cargo build-finished observation is missing".into());
        }
        let path = regular_within(
            output,
            &path.ok_or("Cargo did not produce the exact test executable")?,
        )?;
        RustExecutable::retain(output, path, deadline)
    }
}

#[derive(Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum CargoEvent {
    BuildFinished {
        success: bool,
    },
    CompilerArtifact {
        package_id: String,
        manifest_path: PathBuf,
        target: Box<Target>,
        profile: Profile,
        executable: Option<PathBuf>,
        filenames: Vec<PathBuf>,
    },
    CompilerMessage {
        message: Diagnostic,
    },
    BuildScriptExecuted {},
}
#[derive(Deserialize)]
struct Profile {
    test: bool,
}
#[derive(Deserialize)]
struct Diagnostic {
    level: String,
}

fn native_manifest(path: &Path, kind: &RustTarget, target: &Target) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("Cargo manifest: {e}"))?;
    if bytes.len() > JSON_LIMIT {
        return Err("Cargo manifest exceeds observation limit".into());
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|e| format!("Cargo manifest is not UTF-8: {e}"))?;
    let manifest: toml::Value = toml::from_str(text).map_err(|e| format!("Cargo manifest: {e}"))?;
    let key = match kind {
        RustTarget::Library => "lib",
        RustTarget::Integration(_) => "test",
        RustTarget::Binary(_) => "bin",
    };
    let declarations: Vec<_> = if key == "lib" {
        manifest.get(key).into_iter().collect()
    } else {
        manifest
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|v| v.get("name").and_then(toml::Value::as_str) == Some(&target.name))
                    .collect()
            })
            .unwrap_or_default()
    };
    if declarations.len() > 1 {
        return Err("Cargo manifest has ambiguous target declarations".into());
    }
    for declaration in declarations {
        if declaration
            .get("harness")
            .is_some_and(|v| v.as_bool() != Some(true))
            || declaration
                .get("test")
                .is_some_and(|v| v.as_bool() != Some(true))
            || declaration
                .get("proc-macro")
                .is_some_and(|v| v.as_bool() != Some(false))
        {
            return Err("Cargo custom or disabled test harness is unsupported".into());
        }
        if let Some(source) = declaration.get("path") {
            let source = source.as_str().ok_or("Cargo target path is not a string")?;
            let directory = path.parent().ok_or("Cargo manifest lacks a parent")?;
            if regular_within(directory, &directory.join(source))? != target.src_path {
                return Err("Cargo target source disagrees with its manifest".into());
            }
        }
    }
    Ok(())
}

fn regular_within(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical = root
        .canonicalize()
        .map_err(|e| format!("owned Cargo directory: {e}"))?;
    let relative = path
        .strip_prefix(root)
        .or_else(|_| path.strip_prefix(&canonical))
        .map_err(|_| "Cargo path is outside the owned directory")?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Cargo path is empty or ambiguous".into());
    }
    let mut current = canonical;
    let components: Vec<_> = relative.components().collect();
    for (i, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|e| format!("Cargo path {}: {e}", current.display()))?;
        if if i + 1 == components.len() {
            !metadata.is_file()
        } else {
            !metadata.is_dir()
        } {
            return Err("Cargo path contains a link or unsupported file type".into());
        }
    }
    Ok(current)
}
