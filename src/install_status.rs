//! What storyhook tooling is installed on this machine, and how far the
//! checkout has run ahead of it (SH-530).
//!
//! # Why this is a verb and not a warning
//!
//! Closing the leak from a checkout into an installation creates its mirror.
//! Once the installed set comes only from a release, the checkout and the
//! installation *legitimately* diverge, and if nothing says so the divergence
//! is indistinguishable from a bug: an agent edits `plugins/story/bin/story.sh`,
//! sees no effect, and either gets confused or "fixes" it by pointing the
//! marketplace back at the checkout — reinstating the exact defect. That is the
//! SH-306 pressure shape, a gate whose only escape is the thing it forbids.
//!
//! So this reports, and what it reports is meant to make the answer obvious:
//! nothing is lost, because git is the record and the installation is only a
//! projection of a release.
//!
//! # Two rules it obeys
//!
//! **Silence reads as unknown, never as a pass** (SH-418). A row this cannot
//! positively confirm prints a named negative — `unknown`, `unregistered`,
//! `not recorded` — never a blank and never `ok`. A provider CLI that is not
//! installed is a row this tool could not check, which is a fact about the
//! report's completeness rather than an all-clear.
//!
//! **It never says "revert."** A change sitting in the checkout is a change
//! aimed at the next release, so the remedy named is always the release, never
//! throwing the work away.
//!
//! # What this deliberately does NOT answer
//!
//! Whether the newest *published* release could open this store. That is the
//! sharpest question SH-530 raises — on the filing machine the store sat at
//! schema 21 while the newest release understood 18, so no release could open
//! it at all — and answering it needs a network call to the releases API. This
//! verb makes none: it must work on a machine that is offline, and a detector
//! that sometimes hangs on DNS is a detector people stop running. The `binary`
//! and `store` rows together are what a reader uses instead, and the gap is
//! named here rather than papered over.
//!
//! # Store-free by construction
//!
//! The single most important thing this can report is that the store will not
//! open, or opens read-only. A verb that needed the store first could never
//! deliver its own headline, which is why `Invocation::DoctorInstall` sits on
//! `needs_no_store` and why the schema below is read through a **read-only**
//! connection that never migrates.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::env::Environment;
use crate::error::AppError;

/// One line of the report.
struct Row {
    label: &'static str,
    value: String,
    /// `None` when there is nothing to flag.
    finding: Option<String>,
}

impl Row {
    fn ok(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            finding: None,
        }
    }

    fn flagged(label: &'static str, value: impl Into<String>, finding: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            finding: Some(finding.into()),
        }
    }
}

/// The `story` this machine's `$PATH` resolves, and what it reports.
fn installed_binary() -> Row {
    let Some(path) = crate::path_identity::installed_story() else {
        return Row::flagged(
            "binary",
            "unknown",
            "no `story` on $PATH — this build is not the one this machine runs",
        );
    };
    // The spelling, not the canonical form: `~/.local/bin/story` is what
    // survives the next upgrade, where a version-pinned realpath does not
    // (`path_identity`'s own rule).
    let spelling = path.spelling.clone();
    let version = std::process::Command::new(&spelling)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let running = crate::version::full();
    match version {
        Some(reported) if reported == running => {
            Row::ok("binary", format!("{reported}  [{}]", spelling.display()))
        }
        Some(reported) => Row::flagged(
            "binary",
            format!("{reported}  [{}]", spelling.display()),
            format!("the build answering you now is `{running}` — a different one"),
        ),
        None => Row::flagged(
            "binary",
            spelling.display().to_string(),
            "could not be asked for its version",
        ),
    }
}

/// The store's recorded schema, read without migrating it.
fn store_row(env: &Environment) -> Row {
    let path = env.store_path();
    if !path.exists() {
        return Row::ok("store", format!("not created yet  [{}]", path.display()));
    }
    let supported = crate::store::current_schema_version();
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let Ok(conn) = rusqlite::Connection::open_with_flags(path, flags) else {
        return Row::flagged(
            "store",
            path.display().to_string(),
            "could not be opened read-only to ask its schema version",
        );
    };
    let Ok(found) = crate::store::migrate::schema_version(&conn) else {
        return Row::flagged(
            "store",
            path.display().to_string(),
            "could not be asked for its schema version",
        );
    };
    if found > supported {
        return Row::flagged(
            "store",
            format!("schema {found}, this build understands {supported}"),
            format!(
                "READ-ONLY: written by a newer storyhook. Reads are served, writes are \
                 refused. Install a build that understands schema {found}"
            ),
        );
    }
    if found < supported {
        return Row::flagged(
            "store",
            format!("schema {found}, this build understands {supported}"),
            format!(
                "a migration to {supported} is PENDING and is one-way; it runs on the next \
                 command that opens this store"
            ),
        );
    }
    Row::ok("store", format!("schema {found}"))
}

#[derive(Clone, Copy)]
enum ProviderConfig {
    Claude,
    Codex,
}

fn configured_source(body: &str, format: ProviderConfig) -> Result<Option<String>, String> {
    match format {
        ProviderConfig::Claude => {
            let value: serde_json::Value = serde_json::from_str(body)
                .map_err(|error| format!("its configuration is invalid JSON: {error}"))?;
            let Some(marketplace) = value.get("storyhook") else {
                return Ok(None);
            };
            let source = marketplace
                .get("source")
                .ok_or_else(|| "its storyhook marketplace has no `source` record".to_string())?;
            if let Some(source) = source.as_str() {
                return Ok(Some(source.to_string()));
            }
            for key in ["path", "repo", "url"] {
                if let Some(source) = source.get(key).and_then(serde_json::Value::as_str) {
                    return Ok(Some(source.to_string()));
                }
            }
            Err("its storyhook marketplace source has no path, repository or URL".to_string())
        }
        ProviderConfig::Codex => {
            let value: toml::Value = toml::from_str(body)
                .map_err(|error| format!("its configuration is invalid TOML: {error}"))?;
            let Some(marketplace) = value
                .get("marketplaces")
                .and_then(|value| value.get("storyhook"))
            else {
                return Ok(None);
            };
            marketplace
                .get("source")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
                .map(Some)
                .ok_or_else(|| "its storyhook marketplace has no string `source`".to_string())
        }
    }
}

/// A provider's registered marketplace source, read from the provider's own
/// configuration rather than by invoking it — this must answer on a machine
/// where the provider CLI is not installed, and must not pay a subprocess.
fn provider_row(label: &'static str, config: &Path, format: ProviderConfig) -> Row {
    if !config.exists() {
        return Row::ok(label, "not registered");
    }
    let Ok(body) = std::fs::read_to_string(config) else {
        return Row::flagged(label, "unknown", "its configuration could not be read");
    };
    let source = match configured_source(&body, format) {
        Ok(Some(source)) => source,
        Ok(None) => return Row::ok(label, "not registered"),
        Err(finding) => return Row::flagged(label, "unknown", finding),
    };
    if source.contains('/') && !source.contains("mikeydotio/storyhook") {
        return Row::flagged(
            label,
            source,
            "sourced from a CHECKOUT, not a release — every edit in that tree is \
             live here immediately",
        );
    }
    Row::ok(label, source)
}

/// Whether `hooks/protect-install.sh` has anything to protect with.
fn hook_row() -> Row {
    match crate::plugin::managed_paths_file() {
        Ok(file) if file.exists() => Row::ok("edit guard", format!("armed  [{}]", file.display())),
        Ok(file) => Row::flagged(
            "edit guard",
            "not recorded",
            format!(
                "no managed-path manifest at {} — run `story plugin install` to arm the \
                 hook that refuses edits to installed copies",
                file.display()
            ),
        ),
        Err(_) => Row::flagged(
            "edit guard",
            "unknown",
            "its manifest path could not be resolved",
        ),
    }
}

/// The whole report.
pub fn report() -> Result<String, AppError> {
    let env = Environment::from_process(None)?;
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());

    let rows = vec![
        Row::ok("running", crate::version::full()),
        installed_binary(),
        store_row(&env),
        provider_row(
            "claude plugin",
            &home.join(".claude/plugins/known_marketplaces.json"),
            ProviderConfig::Claude,
        ),
        provider_row(
            "codex plugin",
            &home.join(".codex/config.toml"),
            ProviderConfig::Codex,
        ),
        hook_row(),
    ];

    let mut out = String::new();
    let width = rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
    for row in &rows {
        let _ = writeln!(out, "{:<width$}  {}", row.label, row.value, width = width);
        if let Some(finding) = &row.finding {
            let _ = writeln!(out, "{:<width$}  ! {finding}", "", width = width);
        }
    }

    let findings = rows.iter().filter(|r| r.finding.is_some()).count();
    if findings == 0 {
        out.push_str("\nevery component agrees.\n");
    } else {
        let _ = write!(
            out,
            "\n{findings} finding(s). Nothing in your checkout is lost by any of them: \
             the checkout is the record and the installation is only a projection of a \
             release. The remedy is to cut and install the next release, never to revert \
             the work.\n"
        );
    }
    Ok(out)
}
