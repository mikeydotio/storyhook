//! Closed native Rust inputs; unsupported programs cannot establish detector equivalence.
mod syntax;
#[cfg(test)]
mod tests;
use super::{PreparedDirectory, RustCase, RustTarget};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

/// The isolated difference between two otherwise identical input trees.
#[derive(Debug, PartialEq, Eq)]
pub enum RustIntervention {
    /// Only the dependency-free library implementation differs.
    Behavior,
    /// Only these literal assertion fixtures differ.
    Fixture(Vec<String>),
}

/// Syntax-checked deterministic inputs, not proof of execution or responsibility.
pub struct RustInputs {
    intervention: RustIntervention,
    detector: String,
}

impl RustInputs {
    /// Validate complete native materializations, retaining the assertion on both sides.
    pub fn validate(
        candidate: &PreparedDirectory,
        control: &PreparedDirectory,
        case: &RustCase,
    ) -> Result<Self, String> {
        let RustTarget::Integration(target) = &case.target else {
            return Err("closed Rust inputs currently require an integration target".into());
        };
        candidate.verify_unchanged().map_err(|e| e.to_string())?;
        control.verify_unchanged().map_err(|e| e.to_string())?;
        let test = format!("tests/{target}.rs");
        let a = candidate.fingerprints();
        let b = control.fingerprints();
        if a.keys().ne(b.keys()) {
            return Err("the input trees have different file inventories".into());
        }
        for path in a.keys() {
            if path.starts_with(".cargo")
                || path == Path::new("build.rs")
                || path == Path::new("rust-toolchain")
                || path == Path::new("rust-toolchain.toml")
                || path.starts_with("src") && path != Path::new("src/lib.rs")
                || path.starts_with("tests") && path != Path::new(&test)
                || path.starts_with("examples")
                || path.starts_with("benches")
            {
                return Err(format!("unsupported compilation input {}", path.display()));
            }
        }
        for fixed in ["Cargo.toml", "Cargo.lock", &test] {
            if !a.contains_key(Path::new(fixed))
                || a.get(Path::new(fixed)) != b.get(Path::new(fixed))
            {
                return Err(format!("assertion or compilation input changed: {fixed}"));
            }
        }
        let source =
            |root: &PreparedDirectory, path: &str| root.source(path).map_err(|e| e.to_string());
        manifest(
            &source(candidate, "Cargo.toml")?,
            &source(candidate, "Cargo.lock")?,
            &case.package,
        )?;
        let candidate_library = source(candidate, "src/lib.rs")?;
        let control_library = source(control, "src/lib.rs")?;
        let assertion = source(candidate, &test)?;
        let fixtures = syntax::check(&candidate_library, &assertion, case)?;
        if fixtures != syntax::check(&control_library, &assertion, case)? {
            return Err("control changed the detector's literal input closure".into());
        }
        for fixture in &fixtures {
            source(candidate, fixture)?;
            source(control, fixture)?;
        }
        let changed: BTreeSet<_> = a
            .iter()
            .filter(|(path, value)| b.get(*path) != Some(*value))
            .map(|(path, _)| path.to_string_lossy().into_owned())
            .collect();
        let intervention = if changed == BTreeSet::from(["src/lib.rs".into()])
            && candidate_library != control_library
        {
            RustIntervention::Behavior
        } else if !changed.is_empty()
            && changed.is_subset(&fixtures)
            && candidate_library == control_library
        {
            // A mode-only change is not a fixture intervention.
            for path in &changed {
                if source(candidate, path)? == source(control, path)? {
                    return Err("fixture content did not change".into());
                }
            }
            RustIntervention::Fixture(changed.iter().cloned().collect())
        } else {
            return Err(
                "comparison does not isolate production behavior or literal fixture data".into(),
            );
        };
        let mut digest = Sha256::new();
        digest.update(b"rust-inputs-v1\0");
        // Variable inputs are already bound by the exact trees. This digest names fixed inputs.
        for (path, (mode, bytes)) in &a {
            if changed.contains(path.to_str().ok_or("non-UTF-8 input path")?) {
                continue;
            }
            let name = path.to_str().ok_or("non-UTF-8 input path")?;
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update(mode.to_le_bytes());
            digest.update(bytes);
        }
        candidate.verify_unchanged().map_err(|e| e.to_string())?;
        control.verify_unchanged().map_err(|e| e.to_string())?;
        Ok(Self {
            intervention,
            detector: format!("rust-inputs-v1:{:x}", digest.finalize()),
        })
    }

    /// Which program input changed while the assertion remained fixed.
    pub fn intervention(&self) -> &RustIntervention {
        &self.intervention
    }

    /// Digest of the retained assertion and its fixed compilation inputs.
    pub fn detector(&self) -> &str {
        &self.detector
    }
}

fn manifest(source: &str, lock: &str, name: &str) -> Result<(), String> {
    let value: toml::Value = toml::from_str(source).map_err(|e| format!("manifest: {e}"))?;
    let root = value.as_table().ok_or("manifest is not a table")?;
    if root.len() != 1 || !root.contains_key("package") {
        return Err(
            "closed Rust inputs require a standalone dependency-free default library package"
                .into(),
        );
    }
    let package = root["package"].as_table().ok_or("package is not a table")?;
    if package
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "version" | "edition" | "build"))
        || package.get("name").and_then(toml::Value::as_str) != Some(name)
        || !matches!(
            package.get("edition").and_then(toml::Value::as_str),
            Some("2018" | "2021" | "2024")
        )
        || package
            .get("build")
            .is_some_and(|v| v.as_bool() != Some(false))
    {
        return Err("manifest can introduce unsupported compilation or target inputs".into());
    }
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or("missing package version")?;
    let lock: toml::Value = toml::from_str(lock).map_err(|e| format!("lockfile: {e}"))?;
    let lock = lock.as_table().ok_or("lockfile is not a table")?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("lockfile has no package")?;
    if lock.len() != 2
        || !matches!(
            lock.get("version").and_then(toml::Value::as_integer),
            Some(3 | 4)
        )
        || packages.len() != 1
    {
        return Err("lockfile contains unsupported dependencies or settings".into());
    }
    let retained = packages[0].as_table().ok_or("invalid locked package")?;
    if retained.len() != 2
        || retained.get("name").and_then(toml::Value::as_str) != Some(name)
        || retained.get("version").and_then(toml::Value::as_str) != Some(version)
    {
        return Err("lockfile does not bind the single local package".into());
    }
    Ok(())
}
