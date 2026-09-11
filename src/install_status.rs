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
//! report's completeness rather than an all-clear. And **an absent
//! registration is resolved against the disk, never read as "never installed"**
//! (SH-640): a provider whose installed copies are still present lost its
//! registration, and that row is flagged — the quiet `not registered` is
//! reserved for a provider that left nothing behind.
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
use crate::plugin::PluginTarget;
use crate::plugin::registration::{config_path, configured_source};

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
    // Before `$PATH` is consulted at all (SH-630): `$PATH` agreeing with the
    // running binary is the caller's own doing, and this row answered `ok`
    // for exactly the invocation that migrated the production store. A
    // binary still where cargo wrote it is not installed by any mechanism
    // this tree has, and the row says so ahead of anything `$PATH` claims.
    if let Some(build_dir) = crate::path_identity::build_dir()
        && let Some(running) = crate::path_identity::running_exe()
        && crate::path_identity::is_inside_build_dir(&running.canonical, &build_dir)
    {
        return Row::flagged(
            "binary",
            format!(
                "{}  [{}]",
                crate::version::full(),
                running.spelling.display()
            ),
            format!(
                "not installed — still where cargo built it ({}); `make install` copies it out",
                build_dir.display()
            ),
        );
    }
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

/// No registration for storyhook in the provider's configuration — which
/// means one of two things, and the row says which (SH-640, SH-671).
///
/// A provider that was never installed here is the quiet case this row exists
/// for: a Codex-only or Claude-only machine must not carry a finding. A
/// provider that *was* installed here and has since lost its registration —
/// a new session of that provider gets no `/story` at all — is a finding,
/// never an `ok`. Absence is resolved against evidence rather than promoted
/// to "never" (the SH-372 rule), and there are two kinds: the provider's own
/// surviving copies (SH-640), and storyhook's own install receipt (SH-671),
/// which is the one that is still there after the provider sweeps its copies
/// too — as Claude Code 2.1.268 did on 2026-09-10, when this row read a
/// broken machine as a never-installed one.
fn unregistered(label: &'static str, target: PluginTarget) -> Row {
    let residue = match crate::plugin::install_residue(target) {
        Ok(residue) => residue,
        Err(_) => {
            return Row::flagged(
                label,
                "unknown",
                "its installed copies could not be looked for: the home directory could \
                 not be resolved",
            );
        }
    };
    let receipt = match crate::plugin::install_receipt(target) {
        Ok(receipt) => receipt,
        Err(error) => return Row::flagged(label, "unknown", error.to_string()),
    };
    if residue.is_empty() && receipt.is_none() {
        return Row::ok(label, "not registered");
    }
    let mut evidence = Vec::new();
    if let Some(body) = receipt {
        let path = crate::plugin::install_receipt_path(target)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "its install receipt".to_string());
        let installed_at = body
            .lines()
            .find_map(|line| line.strip_prefix("installed_at "))
            .unwrap_or("an unrecorded time");
        evidence.push(format!(
            "`story plugin install {}` recorded an install here at {installed_at} ({path})",
            target.install_token()
        ));
    }
    if !residue.is_empty() {
        let copies = residue
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        evidence.push(format!("installed copies remain at {copies}"));
    }
    Row::flagged(
        label,
        "not registered",
        format!(
            "DEREGISTERED: {} no longer lists the storyhook marketplace, but {} — run \
             `story plugin install {}`",
            target.display_name(),
            evidence.join(", and "),
            target.install_token()
        ),
    )
}

/// A provider's registered marketplace source, read from the provider's own
/// configuration rather than by invoking it — this must answer on a machine
/// where the provider CLI is not installed, and must not pay a subprocess.
fn provider_row(label: &'static str, config: &Path, target: PluginTarget) -> Row {
    if !config.exists() {
        return unregistered(label, target);
    }
    let Ok(body) = std::fs::read_to_string(config) else {
        return Row::flagged(label, "unknown", "its configuration could not be read");
    };
    let source = match configured_source(&body, target) {
        Ok(Some(source)) => source,
        Ok(None) => return unregistered(label, target),
        Err(finding) => return Row::flagged(label, "unknown", finding),
    };
    let expected = crate::plugin::release_marketplace_root().ok();
    if expected.as_deref() == Some(Path::new(&source)) {
        return Row::ok(
            label,
            format!("release {}  [{source}]", env!("CARGO_PKG_VERSION")),
        );
    }
    let releases = crate::plugin::release_marketplaces_root().ok();
    if releases
        .as_deref()
        .is_some_and(|root| Path::new(&source).starts_with(root))
    {
        return Row::flagged(
            label,
            source,
            format!(
                "STALE RELEASE: this build carries plugin {} — run `story plugin install`",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }
    if source.contains("mikeydotio/storyhook")
        || source.starts_with("https://")
        || source.starts_with("git@")
    {
        return Row::flagged(
            label,
            source,
            "UNPINNED Git marketplace — its default branch can change without a release; run \
             `story plugin install`",
        );
    }
    // Any other path is a checkout: every edit, merge and branch switch in
    // that tree changes the installed plugin without a release.
    if source.contains('/') {
        return Row::flagged(
            label,
            source,
            "sourced from a CHECKOUT, not a release — every edit in that tree is \
             live here immediately; run `story plugin install`",
        );
    }
    Row::flagged(label, source, "its marketplace source is not a release")
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
            &config_path(&home, PluginTarget::ClaudeCode),
            PluginTarget::ClaudeCode,
        ),
        provider_row(
            "codex plugin",
            &config_path(&home, PluginTarget::Codex),
            PluginTarget::Codex,
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
