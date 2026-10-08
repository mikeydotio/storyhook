//! macOS daemon descriptor headroom (SH-800), without changing hard limits.
//!
//! 128 HTTP slots, ten pooled SQLite connections, listeners, journals and
//! subprocess pipes make launchd's observed soft limit of 256 restrictive.
//! 1024 is conservative engineering headroom, not an exhaustion measurement.
//! It is below Darwin's documented OPEN_MAX (10240); never request infinity.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::env::Environment;

const FLOOR: libc::rlim_t = 1024;
const VARIABLE: &str = "STORYHOOK_DAEMON_NOFILE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    soft: libc::rlim_t,
    hard: libc::rlim_t,
}

trait Kernel {
    fn read(&mut self) -> std::io::Result<Limits>;
    fn write(&mut self, limits: Limits) -> std::io::Result<()>;
}

struct Native;

impl Kernel for Native {
    fn read(&mut self) -> std::io::Result<Limits> {
        let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: the valid NOFILE selector writes one rlimit on success.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful getrlimit initialized both fields.
        let limits = unsafe { limits.assume_init() };
        Ok(Limits {
            soft: limits.rlim_cur,
            hard: limits.rlim_max,
        })
    }

    fn write(&mut self, limits: Limits) -> std::io::Result<()> {
        let limits = libc::rlimit {
            rlim_cur: limits.soft,
            rlim_max: limits.hard,
        };
        // SAFETY: one initialized rlimit; called only in this serving process
        // before activity/worker startup. The original hard limit is retained.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

fn automatic(value: Option<&OsStr>) -> Result<bool, String> {
    match value {
        None => Ok(true),
        Some(value) if value == "auto" => Ok(true),
        Some(value) if value == "inherit" => Ok(false),
        // Do not echo an arbitrary environment value into the daemon log.
        Some(_) => Err(format!(
            "{VARIABLE} must be auto or inherit; keeping inherited limits"
        )),
    }
}

fn target(before: Limits) -> Option<Limits> {
    if before.soft == libc::RLIM_INFINITY || before.soft >= FLOOR {
        return None;
    }
    let soft = if before.hard == libc::RLIM_INFINITY {
        FLOOR
    } else {
        FLOOR.min(before.hard)
    };
    (soft > before.soft).then_some(Limits { soft, ..before })
}

/// Only this writer's exact current on-disk service definition permits the
/// default. It cannot prove what an older loaded job inherited. Reusing the
/// strict PATH reader and canonical writer avoids a second partial XML parser
/// guessing at scope.
/// Binary, customized, reformatted, duplicate-key and resource-limit plists
/// are deliberately unknown and preserve their inherited soft limit.
fn canonical_service(text: &str, exe: &Path, env: &Environment) -> Result<(), String> {
    let registered =
        super::agent::registered_exe(text).ok_or("service executable is unreadable")?;
    let execution_path = super::agent::registered_path(text)
        .map_err(|_| "service definition is customized or unreadable")?
        .ok_or("service definition has no explicit PATH")?;
    let registered_identity = std::fs::canonicalize(&registered)
        .map_err(|_| "service executable identity is unavailable")?;
    let actual_identity =
        std::fs::canonicalize(exe).map_err(|_| "daemon executable identity is unavailable")?;
    if registered_identity != actual_identity {
        return Err("service executable differs from this daemon".into());
    }
    if text != super::agent::plist(&registered, env, &execution_path) {
        return Err(
            "service definition carries explicit policy or is not the matching managed definition"
                .into(),
        );
    }
    Ok(())
}

fn permitted(
    env: &Environment,
    owner: Option<&str>,
    value: Option<&OsStr>,
) -> Result<bool, String> {
    if !automatic(value)? {
        return Ok(false);
    }
    if owner == Some("launchd") {
        let path = super::agent::path(env);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| "service definition unavailable; keeping inherited limits")?;
        let metadata = file
            .metadata()
            .map_err(|_| "service definition identity unavailable")?;
        if !metadata.is_file() {
            return Err(
                "service definition is not a bounded regular file; keeping inherited limits".into(),
            );
        }
        let mut text = String::new();
        file.take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "service definition unreadable; keeping inherited limits")?;
        if text.len() > 64 * 1024 {
            return Err(
                "service definition exceeds the read bound; keeping inherited limits".into(),
            );
        }
        let exe = std::env::current_exe()
            .map_err(|_| "daemon executable unavailable; keeping inherited limits")?;
        canonical_service(&text, &exe, env)?;
    }
    Ok(true)
}

fn raise(kernel: &mut impl Kernel, before: Limits) -> Result<Option<Limits>, String> {
    let Some(wanted) = target(before) else {
        return Ok(None);
    };
    kernel.write(wanted).map_err(|error| {
        format!("cannot raise daemon NOFILE soft limit from {} to {}: {error}; no fallback limit change attempted", before.soft, wanted.soft)
    })?;
    let actual = kernel.read().map_err(|error| {
        format!("daemon NOFILE change requested but readback failed: {error}; outcome unverified")
    })?;
    if actual != wanted {
        return Err(format!(
            "daemon NOFILE readback differs: requested soft {}, observed soft {} hard {}; no further change attempted",
            wanted.soft, actual.soft, actual.hard
        ));
    }
    Ok(Some(actual))
}

fn configure(
    kernel: &mut impl Kernel,
    env: &Environment,
    owner: Option<&str>,
    value: Option<&OsStr>,
) -> Result<Option<Limits>, String> {
    let before = kernel
        .read()
        .map_err(|error| format!("cannot read daemon NOFILE: {error}; limits unchanged"))?;
    if !automatic(value)? || target(before).is_none() {
        return Ok(None);
    }
    if !permitted(env, owner, value)? {
        return Ok(None);
    }
    raise(kernel, before)
}

pub(super) fn initialize(env: &Environment, owner: Option<&str>) {
    let value = std::env::var_os(VARIABLE);
    match configure(&mut Native, env, owner, value.as_deref()) {
        Ok(Some(actual)) => eprintln!(
            "storyhook daemon: NOFILE soft limit is {}; inherited hard limit unchanged",
            actual.soft
        ),
        Ok(None) => {}
        Err(reason) => eprintln!("warning: storyhook daemon: {reason}"),
    }
}

#[cfg(test)]
mod tests;
