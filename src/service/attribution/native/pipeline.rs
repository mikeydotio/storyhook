//! Native helper execution and strict output validation.
use super::*;
use crate::process::{TerminationPolicy, run_captured_cancellable};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    process::Command,
};

const OUTPUT_LIMIT: u64 = 16 * 1024 * 1024;
const EXECUTABLE_LIMIT: u64 = 512 * 1024 * 1024;

pub(super) fn execute(
    native: &NativeRustComparison,
    side: ProbeSide,
    binding: &NativeProbeBinding<'_>,
) -> Result<ProbeResult, AppError> {
    let start = Instant::now();
    let source = if side == ProbeSide::Candidate {
        &native.candidate
    } else {
        &native.control
    };
    fs::DirBuilder::new()
        .mode(0o700)
        .create(binding.output)
        .map_err(|e| invalid(&format!("create fresh output: {e}")))?;
    let output = binding
        .output
        .canonicalize()
        .map_err(|e| invalid(&format!("resolve output: {e}")))?;
    let mut cleanup_complete = true; // No child has been launched yet.
    let observed = (|| -> Result<(RustCaseObservation, ProbeEnvironment), String> {
        let bundle = crate::daemon::verifier_bundle::materialize(&native.environment)
            .map_err(|e| e.to_string())?;
        let RustTarget::Integration(target) = &native.case.target else {
            return Err("unsupported native target".into());
        };
        let lock_root = std::env::var_os("STORYHOOK_LOCK_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local/state/storyhook/locks"))
            })
            .filter(|p| p.is_absolute())
            .ok_or("machine compiler lock root is unavailable")?;
        let request = json!({"pipeline": {
            "version": 1, "clock": "CLOCK_MONOTONIC", "source": source.path(), "output": output,
            "package": native.case.package, "target": target, "case": native.case.name,
            "tools": null, "wrapper": bundle.join("rustc-slot.py"), "lock_root": lock_root,
            "deadline": posix_deadline(native.deadline)?,
        }, "project": binding.project, "binding": identity(binding), "journal": binding.journal,
            "request_id": binding.request});
        let path = output.join("request.json");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("create request: {e}"))?;
        file.write_all(&serde_json::to_vec(&request).map_err(|e| e.to_string())?)
            .map_err(|e| format!("write request: {e}"))?;
        file.sync_all().map_err(|e| format!("sync request: {e}"))?;
        let mut command = Command::new("bash");
        command
            .arg(bundle.join("python-runtime.sh"))
            .arg("--")
            .arg(bundle.join("python-bin/python3"))
            .arg("-B");
        #[cfg(test)]
        if let Some(fixture) = &native.fixture {
            command.arg(super::tests::FIXTURE).arg(fixture);
        }
        command
            .arg(bundle.join("attribution-rust.py"))
            .arg(&path)
            .current_dir(&output);
        // The helper receives no store, credentials, source runtime overrides or inherited grant.
        command.env_clear();
        for key in ["PATH", "HOME", "RUSTUP_HOME", "STORYHOOK_LOCK_DIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        cleanup_complete = false;
        let child = run_captured_cancellable(
            command,
            remaining(native.deadline)?,
            TerminationPolicy::TerminateThenKill {
                grace: binding.termination_grace,
            },
            &native.cancellation,
            |_| Ok(()),
        );
        // The broker, not a reaped driver PID, proves that the managed session settled.
        let resource = read_json(&output.join("resource.json"));
        if let Ok(resource) = &resource {
            cleanup_complete = resource["cleanup_complete"] == true
                && resource["binding"] == identity(binding)
                && resource["lease"]["state"] == "released";
        }
        let child = child.map_err(|e| format!("native driver: {}", e.detail()))?;
        fs::write(output.join("driver.stdout"), &child.stdout)
            .map_err(|e| format!("retain driver stdout: {e}"))?;
        fs::write(output.join("driver.stderr"), &child.stderr)
            .map_err(|e| format!("retain driver stderr: {e}"))?;
        if !child.status.success()
            || child.stdout_truncated
            || !child.stdout.is_empty()
            || !child.stderr.is_empty()
        {
            return Err(format!(
                "native driver did not complete cleanly ({}): {}",
                child.status,
                String::from_utf8_lossy(&child.stderr)
            ));
        }
        source.verify_unchanged().map_err(|e| e.to_string())?;
        native
            .candidate
            .verify_unchanged()
            .map_err(|e| e.to_string())?;
        native
            .control
            .verify_unchanged()
            .map_err(|e| e.to_string())?;
        if native.cancellation.is_cancelled() {
            return Err("native diagnostic authority cancelled".into());
        }
        remaining(native.deadline)?;
        validate(native, source, &output, binding, &resource?)
    })();
    let (executions, outcome, environment) = match observed {
        Ok((observation, environment)) => (
            observation.executions,
            observation.outcome,
            Some(environment),
        ),
        Err(detail) => (0, ProbeOutcome::Unavailable { detail }, None),
    };
    Ok(ProbeResult {
        tree: if side == ProbeSide::Candidate {
            native.trees.trees().0
        } else {
            native.trees.trees().1
        }
        .into(),
        detector: native.inputs.detector().into(),
        executions,
        outcome,
        environment,
        log: output.display().to_string(),
        execution_id: binding.execution.into(),
        cleanup_complete,
        milliseconds: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

fn validate(
    native: &NativeRustComparison,
    source: &PreparedDirectory,
    output: &Path,
    binding: &NativeProbeBinding<'_>,
    resource: &Value,
) -> Result<(RustCaseObservation, ProbeEnvironment), String> {
    let observed = read_json(&output.join("observation.json"))?;
    if observed["version"] != 1 || observed.get("error").is_some() {
        return Err(format!("native pipeline incomplete: {observed}"));
    }
    let stage = |name: &str| -> Result<i32, String> {
        observed[name]["exit"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
            .ok_or_else(|| format!("missing native {name} exit"))
    };
    for name in [
        "metadata",
        "build",
        "listing",
        "cargo-version",
        "rustc-version",
    ] {
        if stage(name)? != 0 {
            return Err(format!("native {name} failed"));
        }
    }
    let target = CargoTarget::resolve(
        source.path(),
        &native.case,
        &read(&output.join("metadata.stdout"), OUTPUT_LIMIT)?,
    )?;
    let executable = target.executable(
        &output.join("target"),
        &read(&output.join("build.stdout"), OUTPUT_LIMIT)?,
        false,
        Some(stage("build")?),
        native.deadline,
    )?;
    if observed["executable"].as_str() != executable.path().to_str()
        || observed["artifact_before"] != observed["artifact_after"]
    {
        return Err("native executed artifact differs from the complete Cargo build".into());
    }
    fingerprint(&observed["artifact_after"], native.deadline)?;
    executable.verify_unchanged()?;
    let expected = |args: Vec<String>| {
        json!(
            std::iter::once(executable.path().display().to_string())
                .chain(args)
                .collect::<Vec<_>>()
        )
    };
    if observed["listing"]["argv"] != expected(native.case.list_arguments())
        || observed["run"]["argv"] != expected(native.case.run_arguments())
    {
        return Err("native exact selection arguments changed".into());
    }
    native.case.validate_listing(
        &read(&output.join("listing.stdout"), OUTPUT_LIMIT)?,
        &read(&output.join("listing.stderr"), OUTPUT_LIMIT)?,
        false,
        Some(stage("listing")?),
    )?;
    let result = native.case.observe(
        &read(&output.join("run.stdout"), OUTPUT_LIMIT)?,
        &read(&output.join("run.stderr"), OUTPUT_LIMIT)?,
        false,
        Some(stage("run")?),
    );
    let tools = observed["tools"]
        .as_object()
        .ok_or("native toolchain evidence is missing")?;
    if tools.len() != 4
        || ["cargo", "rustc", "python", "wrapper"]
            .iter()
            .any(|k| !tools.contains_key(*k))
    {
        return Err("native toolchain is incomplete".into());
    }
    for tool in tools.values() {
        fingerprint(tool, native.deadline)?;
    }
    let mut toolchain = Sha256::new();
    toolchain.update(serde_json::to_vec(tools).map_err(|e| e.to_string())?);
    for name in ["cargo-version", "rustc-version"] {
        let version = read(&output.join(format!("{name}.stdout")), OUTPUT_LIMIT)?;
        if version.is_empty()
            || !read(&output.join(format!("{name}.stderr")), OUTPUT_LIMIT)?.is_empty()
        {
            return Err("native version output unavailable".into());
        }
        toolchain.update(version);
    }
    let supported = supported(resource, binding)?;
    Ok((
        result,
        ProbeEnvironment {
            toolchain: format!("native-tools-v1:{:x}", toolchain.finalize()),
            fixtures: native.inputs.detector().into(),
            resource_policy: format!(
                "{}:{}:{}",
                resource["authority"].as_str().unwrap_or_default(),
                resource["boot"].as_str().unwrap_or_default(),
                resource["policy"].as_str().unwrap_or_default()
            ),
            grant: format!(
                "{}#{}",
                output.join("resource.json").display(),
                binding.request
            ),
            supported,
        },
    ))
}

fn supported(value: &Value, binding: &NativeProbeBinding<'_>) -> Result<bool, String> {
    if value["version"] != 1
        || value["entry"] != "causal-rust"
        || value["worker_exit"] != 0
        || value["binding"] != identity(binding)
        || value["lease"]["binding"] != identity(binding)
        || value["lease"]["id"] != binding.request
        || value["lease"]["state"] != "released"
        || value["cleanup_complete"] != true
    {
        return Err("resource identity or cleanup evidence is incomplete".into());
    }
    if ["authority", "boot", "policy"]
        .into_iter()
        .any(|key| value[key].as_str().is_none_or(str::is_empty))
    {
        return Err("resource authority identity is missing".into());
    }
    let events = value["events"]
        .as_array()
        .ok_or("resource events are missing")?;
    let mut names = std::collections::BTreeSet::new();
    let mut supported = value["supported"] == true;
    for event in events {
        if ["authority", "boot", "policy"]
            .into_iter()
            .any(|key| event[key] != value[key])
        {
            supported = false;
        }
        let name = event["event"]
            .as_str()
            .ok_or("resource event has no name")?;
        if event["lease"] == binding.request {
            if event["binding"] != identity(binding) {
                return Err("foreign resource event binding".into());
            }
            names.insert(name);
        }
        if matches!(name, "cancel" | "quarantine" | "recovery")
            || name == "pressure" && event["reason"] != "ready"
        {
            supported = false;
        }
    }
    for name in ["grant", "attach", "usage", "cleanup", "release"] {
        supported &= names.contains(name);
    }
    for name in ["cpu", "memory"] {
        supported &= matches!((value["lease"]["peaks"][name].as_u64(), value["lease"]["resources"][name].as_u64()), (Some(peak), Some(limit)) if limit>0 && peak<=limit);
    }
    Ok(supported)
}

fn identity(binding: &NativeProbeBinding<'_>) -> Value {
    json!({"attempt_id": binding.attempt, "execution_id": binding.execution, "generation": binding.generation})
}

fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&read(path, OUTPUT_LIMIT)?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let info = file.metadata().map_err(|e| e.to_string())?;
    if !info.is_file() || info.len() > limit {
        return Err(format!(
            "{} is not a complete bounded regular file",
            path.display()
        ));
    }
    let mut bytes = vec![];
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != info.len() {
        return Err(format!("{} changed while reading", path.display()));
    }
    Ok(bytes)
}

fn fingerprint(value: &Value, deadline: Instant) -> Result<(), String> {
    remaining(deadline)?;
    let path = Path::new(
        value["path"]
            .as_str()
            .ok_or("native fingerprint has no path")?,
    );
    if !path.is_absolute() {
        return Err("relative native executable path".into());
    }
    let info = fs::symlink_metadata(path).map_err(|e| format!("executable metadata: {e}"))?;
    let identity = json!([
        info.dev(),
        info.ino(),
        info.len(),
        info.mode(),
        info.mtime() * 1_000_000_000 + info.mtime_nsec(),
        info.ctime() * 1_000_000_000 + info.ctime_nsec()
    ]);
    if identity != value["identity"] || !info.is_file() || info.mode() & 0o111 == 0 {
        return Err("native executable identity changed".into());
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open native executable: {e}"))?;
    if info.len() > EXECUTABLE_LIMIT
        || file.metadata().map_err(|e| e.to_string())?.ino() != info.ino()
    {
        return Err("native executable is too large or replaced".into());
    }
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut count = 0u64;
    loop {
        remaining(deadline)?;
        let n = file
            .read(&mut bytes)
            .map_err(|e| format!("read native executable: {e}"))?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > EXECUTABLE_LIMIT {
            return Err("native executable grew beyond limit".into());
        }
        digest.update(&bytes[..n]);
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    if count != info.len()
        || after.len() != info.len()
        || after.mode() != info.mode()
        || after.mtime() != info.mtime()
        || after.mtime_nsec() != info.mtime_nsec()
        || after.ctime() != info.ctime()
        || after.ctime_nsec() != info.ctime_nsec()
        || value["sha256"] != format!("{:x}", digest.finalize())
    {
        return Err("native executable bytes changed".into());
    }
    remaining(deadline)?;
    Ok(())
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err("active diagnosis allowance exhausted".into())
    } else {
        Ok(left)
    }
}

fn posix_deadline(deadline: Instant) -> Result<f64, String> {
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock points to an initialized timespec; this reads the named native clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } != 0 {
        return Err(format!(
            "CLOCK_MONOTONIC: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(clock.tv_sec as f64
        + clock.tv_nsec as f64 / 1_000_000_000.0
        + remaining(deadline)?.as_secs_f64())
}
