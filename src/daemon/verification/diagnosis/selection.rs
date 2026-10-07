//! Proposals are untrusted hints; only native pinned validation can establish support.
use super::*;
use std::{fs, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

pub(super) fn propose(
    original: &GateExecution,
    checkout: &Path,
) -> Result<RustDiagnosisRequest, String> {
    // The checkout supplies a package-name hint only. The native adapter subsequently
    // validates the manifest, lockfile and complete source from pinned Git objects.
    let path = checkout.join("Cargo.toml");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|e| format!("read package proposal {}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("package proposal must be a regular manifest".into());
    }
    let mut text = String::new();
    (&mut file)
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 1024 * 1024 {
        return Err("package proposal exceeds manifest bound".into());
    }
    let manifest: toml::Value =
        toml::from_str(&text).map_err(|e| format!("package proposal: {e}"))?;
    let package = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or("package proposal has no literal package name")?;
    for failure in &original.failed_cases {
        let (Some(target), Some(name)) = (&failure.target, &failure.name) else {
            continue;
        };
        let Ok(case) = RustCase::new(package, RustTarget::Integration(target.clone()), name) else {
            continue;
        };
        let mut matches = Vec::new();
        for log in &original.logs {
            let request = RustDiagnosisRequest {
                execution: original.id.clone(),
                log: log.into(),
                case: case.clone(),
                intervention: TreeIntervention::MissingDetector(format!("tests/{target}.rs")),
                #[cfg(test)]
                fixture: None,
            };
            if record::select(original, &request).is_ok() {
                matches.push(request);
            }
        }
        // A repeated raw frame is not permission to guess which execution it describes.
        if matches.len() == 1 {
            return Ok(matches.remove(0));
        }
    }
    Err("no recorded case has one supported, complete original Rust failure frame".into())
}
