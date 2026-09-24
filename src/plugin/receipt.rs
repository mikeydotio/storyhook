//! The install receipt: storyhook's own record of having registered a
//! provider on this machine, at `<data dir>/provider-installs/<target>`.
//!
//! `story doctor install` reads it to tell a *lost* registration from one that
//! never existed (SH-671). SH-640 answered that question from the provider's
//! own leftovers — its plugin cache — and those went with the registration
//! the next time, so the doctor read a broken machine as a Codex-only one. The
//! receipt is storyhook's file in storyhook's directory: no provider rewrite
//! can take it, and it is per target, which the managed-path manifest (naming
//! both providers on every install) is not.
//!
//! # Never deleted (SH-760)
//!
//! Until SH-760 a deliberate `story plugin uninstall` removed the receipt, so
//! that a machine which had once installed a provider did not read
//! `DEREGISTERED` for ever. The one thing that could then defeat the doctor
//! was an uninstall nobody asked for — and there was one: a test binary
//! running the verb in-process against the developer's real home, on every
//! gate run, which removed the registration *and* the receipt and left the
//! doctor saying `every component agrees`. The receipt is now rewritten as a
//! **tombstone** instead: state, when, and — the part that matters — who.
//! [`Actor`] records the executable and its [`Build`] the same way
//! `plugin::guard` classifies it, and whether the guard's override was set. A
//! tombstone from an installed binary, or from any binary whose operator set
//! the override, is a deliberate uninstall and keeps the doctor quiet; one
//! from a test or checkout build with no override is the guard being
//! bypassed, and the doctor names it. The tombstone can only say this because
//! it is written by the same process that did the removing, before the
//! process forgets what it was.
//!
//! # Format
//!
//! `key value` lines, one per line, in a fixed order. `state` is `installed`
//! or `uninstalled`; a receipt with no `state` line is one written before the
//! tombstone existed and reads as `installed`, so every receipt already on a
//! machine keeps its meaning. Unknown keys are ignored, missing ones read as
//! absent: the doctor quotes what it has and treats an actor it cannot read
//! as one it cannot vouch for (SH-418: silence is never a pass).

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::PluginTarget;
use super::guard::{Build, OVERRIDE_VAR};
use crate::error::AppError;
use crate::path_identity;

/// Who wrote a receipt line: the executable, what kind of build it was, and
/// whether `plugin::guard`'s override was in its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// The canonical executable, as the process reported it; `None` when it
    /// could not name itself.
    pub exe: Option<String>,
    /// The guard's classification of that executable; `None` on a tombstone
    /// this build did not write (or one edited by hand).
    pub build: Option<Build>,
    /// Whether [`OVERRIDE_VAR`] was set.
    pub override_set: bool,
}

impl Actor {
    /// This process, as the receipt will describe it.
    #[must_use]
    pub fn observe() -> Self {
        let exe = path_identity::running_exe().map(|exe| exe.canonical);
        Self {
            build: Some(Build::classify(
                crate::env::is_test_build(),
                exe.as_deref(),
                path_identity::build_dir().as_deref(),
            )),
            exe: exe.map(|exe| exe.display().to_string()),
            override_set: std::env::var_os(OVERRIDE_VAR).is_some(),
        }
    }

    /// Whether an uninstall by this actor was one the operator asked for: an
    /// installed binary is what the operator runs, and the override is the
    /// operator's statement about an uninstalled one. An actor whose build
    /// is unrecorded is not vouched for.
    #[must_use]
    pub fn is_deliberate(&self) -> bool {
        self.build == Some(Build::Installed) || self.override_set
    }

    /// The executable for a message, or what to say when there is none.
    #[must_use]
    pub fn exe_display(&self) -> &str {
        self.exe
            .as_deref()
            .unwrap_or("an executable that could not name itself")
    }
}

/// What the receipt says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    /// `story plugin install <target>` succeeded here.
    Installed {
        /// The release that installed.
        version: String,
        /// When, RFC 3339 UTC; `"an unrecorded time"` only for a receipt
        /// written by hand.
        installed_at: String,
        /// Who — absent on receipts written before it was recorded.
        actor: Option<Actor>,
    },
    /// `story plugin uninstall <target>` ran here after an install.
    Uninstalled {
        /// The release the tombstoned install had recorded, if any.
        version: Option<String>,
        /// When that install had happened, if the receipt said.
        installed_at: Option<String>,
        /// When the uninstall ran.
        uninstalled_at: String,
        /// Who ran it.
        actor: Actor,
    },
}

/// Where the receipt for `target` lives: `<data dir>/provider-installs/<target>`.
pub(crate) fn path(target: PluginTarget) -> Result<PathBuf, AppError> {
    Ok(super::data_dir()?
        .join("provider-installs")
        .join(target.install_token()))
}

/// The receipt, if one was ever written here.
pub(crate) fn read(target: PluginTarget) -> Result<Option<Receipt>, AppError> {
    let path = path(target)?;
    match fs::read_to_string(&path) {
        Ok(body) => Ok(Some(parse(&body))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::Storage(format!(
            "could not read the install receipt at `{}`: {error}",
            path.display()
        ))),
    }
}

/// Reads a receipt body. Total: every body parses as *something*, because the
/// doctor must quote what is there rather than fail to say anything.
#[must_use]
pub fn parse(body: &str) -> Receipt {
    let field = |key: &str| -> Option<String> {
        body.lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|rest| rest.strip_prefix(' '))
            })
            .map(|value| value.trim().to_string())
    };
    let actor = || Actor {
        exe: field("by"),
        build: field("build").as_deref().and_then(Build::parse),
        override_set: field("override").as_deref() == Some("yes"),
    };
    let version = field("version");
    let installed_at = field("installed_at");
    if field("state").as_deref() == Some("uninstalled") {
        return Receipt::Uninstalled {
            version,
            installed_at,
            uninstalled_at: field("uninstalled_at")
                .unwrap_or_else(|| "an unrecorded time".to_string()),
            actor: actor(),
        };
    }
    Receipt::Installed {
        version: version.unwrap_or_else(|| "an unrecorded release".to_string()),
        installed_at: installed_at.unwrap_or_else(|| "an unrecorded time".to_string()),
        // Only a receipt that recorded its actor has one; the two-line
        // format did not, and inventing one would vouch for nobody.
        actor: field("by").is_some().then(actor),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn actor_lines(actor: &Actor) -> String {
    format!(
        "by {}\nbuild {}\noverride {}\n",
        actor.exe.as_deref().unwrap_or("unknown"),
        actor.build.map_or("unknown", Build::token),
        if actor.override_set { "yes" } else { "no" }
    )
}

fn write(
    path: &Path,
    body: &str,
    context: impl Fn(std::io::Error) -> AppError,
) -> Result<(), AppError> {
    let parent = path
        .parent()
        .expect("the receipt path has a parent directory");
    fs::create_dir_all(parent).map_err(&context)?;
    fs::write(path, body).map_err(context)
}

/// Writes the receipt once the provider's registration has succeeded —
/// never before, so a registration that did not land is never claimed, and
/// never on a failed reinstall, so an earlier install keeps being one.
///
/// Loud on failure, on purpose: the registration itself is done by now, and
/// the message says so, but a receipt that could not be written means the
/// doctor can no longer detect this install's loss — the operator must hear
/// that rather than read `every component agrees` over it later.
pub(crate) fn record_install(target: PluginTarget) -> Result<(), AppError> {
    let path = path(target)?;
    let context = |error: std::io::Error| {
        AppError::Storage(format!(
            "the {} plugin is registered, but its install receipt at `{}` could not be \
             written: {error}. Until `story plugin install {}` succeeds again, `story doctor \
             install` cannot tell this registration's loss from a provider that was never \
             installed here.",
            target.display_name(),
            path.display(),
            target.install_token()
        ))
    };
    let body = format!(
        "state installed\nversion {}\ninstalled_at {}\n{}",
        env!("CARGO_PKG_VERSION"),
        now(),
        actor_lines(&Actor::observe())
    );
    write(&path, &body, context)
}

/// Rewrites the receipt as a tombstone on a deliberate uninstall, keeping
/// what the install had recorded. `Ok(Some(path))` names what was written;
/// `Ok(None)` means there was no receipt to tombstone — a provider that was
/// never installed here leaves nothing, so the doctor stays quiet about it.
///
/// A receipt that is already a tombstone is rewritten too: the latest
/// uninstall is the one whose actor matters, and an operator's own uninstall
/// after a bypassed one is what clears the doctor's finding.
pub(crate) fn record_uninstall(target: PluginTarget) -> Result<Option<PathBuf>, AppError> {
    let Some(previous) = read(target)? else {
        return Ok(None);
    };
    let path = path(target)?;
    let (version, installed_at) = match previous {
        Receipt::Installed {
            version,
            installed_at,
            ..
        } => (Some(version), Some(installed_at)),
        Receipt::Uninstalled {
            version,
            installed_at,
            ..
        } => (version, installed_at),
    };
    let mut body = String::from("state uninstalled\n");
    if let Some(version) = version {
        body.push_str(&format!("version {version}\n"));
    }
    if let Some(installed_at) = installed_at {
        body.push_str(&format!("installed_at {installed_at}\n"));
    }
    body.push_str(&format!("uninstalled_at {}\n", now()));
    body.push_str(&actor_lines(&Actor::observe()));
    let context = |error: std::io::Error| {
        AppError::Storage(format!(
            "failed to record the uninstall in the install receipt at `{}`: {error}",
            path.display()
        ))
    };
    write(&path, &body, context)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every receipt written before the tombstone existed: two lines, no
    /// state, no actor. It must keep reading as an install.
    #[test]
    fn the_legacy_two_line_receipt_reads_as_installed_with_no_actor() {
        let receipt = parse("version 2.4.2\ninstalled_at 2026-09-11T02:51:39Z\n");
        assert_eq!(
            receipt,
            Receipt::Installed {
                version: "2.4.2".into(),
                installed_at: "2026-09-11T02:51:39Z".into(),
                actor: None,
            }
        );
    }

    #[test]
    fn an_install_receipt_records_its_actor() {
        let receipt = parse(
            "state installed\nversion 3.0.4\ninstalled_at 2026-09-23T01:00:00Z\n\
             by /home/dev/.local/bin/story\nbuild installed\noverride no\n",
        );
        let Receipt::Installed {
            actor: Some(actor), ..
        } = receipt
        else {
            panic!("{receipt:?}");
        };
        assert_eq!(actor.exe.as_deref(), Some("/home/dev/.local/bin/story"));
        assert_eq!(actor.build, Some(Build::Installed));
        assert!(!actor.override_set);
    }

    #[test]
    fn a_tombstone_keeps_the_install_it_replaced_and_names_its_actor() {
        let receipt = parse(
            "state uninstalled\nversion 3.0.3\ninstalled_at 2026-09-21T14:34:17Z\n\
             uninstalled_at 2026-09-21T21:50:44Z\n\
             by /home/dev/repo/target/debug/deps/invoker_seam-1a2b\nbuild test\noverride no\n",
        );
        assert_eq!(
            receipt,
            Receipt::Uninstalled {
                version: Some("3.0.3".into()),
                installed_at: Some("2026-09-21T14:34:17Z".into()),
                uninstalled_at: "2026-09-21T21:50:44Z".into(),
                actor: Actor {
                    exe: Some("/home/dev/repo/target/debug/deps/invoker_seam-1a2b".into()),
                    build: Some(Build::TestBuild),
                    override_set: false,
                },
            }
        );
    }

    /// Deliberate: an installed binary, or anyone who set the override. Not
    /// deliberate: a test or checkout build with no override, and an actor
    /// whose build nobody recorded.
    #[test]
    fn deliberateness_is_the_installed_build_or_the_override() {
        let actor = |build: Option<Build>, override_set: bool| Actor {
            exe: Some("/x".into()),
            build,
            override_set,
        };
        assert!(actor(Some(Build::Installed), false).is_deliberate());
        assert!(actor(Some(Build::TestBuild), true).is_deliberate());
        assert!(actor(Some(Build::Checkout), true).is_deliberate());
        assert!(!actor(Some(Build::TestBuild), false).is_deliberate());
        assert!(!actor(Some(Build::Checkout), false).is_deliberate());
        assert!(!actor(None, false).is_deliberate());
    }

    /// A hand-edited or truncated body still parses to something the doctor
    /// can quote, and never to a vouched-for actor.
    #[test]
    fn a_malformed_body_parses_to_unrecorded_values() {
        let receipt = parse("state uninstalled\n");
        let Receipt::Uninstalled {
            uninstalled_at,
            actor,
            version,
            installed_at,
        } = receipt
        else {
            panic!("{receipt:?}");
        };
        assert_eq!(uninstalled_at, "an unrecorded time");
        assert_eq!(version, None);
        assert_eq!(installed_at, None);
        assert_eq!(actor.exe, None);
        assert_eq!(actor.build, None);
        assert!(!actor.is_deliberate());
        assert_eq!(
            actor.exe_display(),
            "an executable that could not name itself"
        );

        let receipt = parse("");
        assert!(
            matches!(receipt, Receipt::Installed { ref version, .. } if version == "an unrecorded release")
        );
    }

    /// `by` is a prefix of nothing else, but a key must match the whole
    /// word: `bypass x` is not `by pass x`.
    #[test]
    fn keys_match_whole_words() {
        let receipt = parse("state installed\nversion 1\ninstalled_at t\nbypass /nope\n");
        assert!(
            matches!(receipt, Receipt::Installed { actor: None, .. }),
            "{receipt:?}"
        );
    }

    #[test]
    fn this_process_observes_itself_as_a_test_build() {
        let actor = Actor::observe();
        assert_eq!(actor.build, Some(Build::TestBuild));
        assert!(actor.exe.is_some());
    }
}
