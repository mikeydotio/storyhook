//! What a provider has registered as the storyhook marketplace, read from the
//! provider's own configuration rather than by invoking it — and put back
//! when an install destroys it and then fails (SH-641).
//!
//! `story doctor install` needs the registered source on a machine where the
//! provider CLI is not installed and must not pay a subprocess for it; the
//! installer needs it *before* it removes the registration, so that a later
//! failure can put it back. One parser serves both (SH-136): a second copy
//! would drift the first time a provider changed its config layout.
//!
//! # The transaction
//!
//! Both installers are remove-then-add: neither provider promises that adding
//! an existing marketplace name changes its source, so the plugin and the
//! marketplace are removed before the release projection is registered. Until
//! this module, nothing stood between the removes and a later failure — the
//! provider was left with no storyhook marketplace and no plugin, which since
//! SH-640 `story doctor install` names as DEREGISTERED but which still cost
//! the operator a session with no `/story`. The two siblings in `plugin.rs`
//! already did this right (`materialize_release_marketplace` renames the
//! previous projection aside and restores it; `install_codex_sandbox_integration`
//! snapshots and restores its files); the registration was the odd one out.
//!
//! [`snapshot`] records what is registered before the removes; everything
//! after them runs as one closure, and [`undo`] answers its failure: remove
//! whatever the failed run added, re-register the previous source, and say
//! which happened — both errors when the restore fails too, never the first
//! alone (SH-578: a diagnosis downstream of an unchecked failure names the
//! wrong layer).
//!
//! # What "put back" means, exactly
//!
//! The previous *source* is re-registered and the plugin re-added from it.
//! That is the registration, not the bytes: a git or checkout source may serve
//! a different version now than the provider's cache held before, so the note
//! says "re-registered", never "restored". `verify_codex_install` is not
//! re-run over the put-back registration either — verification is a claim
//! about *this* release's payload, and the previous registration was never
//! verified against this binary in the first place. When the previous source
//! is the very release this run failed to install, the note says so and says
//! the result is unverified, because the same commands that just failed are
//! what put it back.
//!
//! # Two limits, stated rather than glossed
//!
//! **A signal is not a failure this module sees.** SIGKILL between the removes
//! and the add is unrecoverable in-process, and SIGINT/SIGTERM are not
//! deferred across the window: doing so needs the children reset to default
//! dispositions so a hung provider stays interruptible, and the only test of
//! it is a process-group signal race that is load-sensitive (SH-347, SH-394).
//! The window is sub-second against a local directory source; SH-640's
//! detector names the state it leaves, and the next `story plugin install`
//! *is* the restore.
//!
//! **An unreadable config never blocks the install.** A parser narrower than
//! the provider's own format must not make `story plugin install` a dead end
//! (the SH-404/SH-405 trap); [`Previous::Unreadable`] proceeds with nothing to
//! put back and says why in any failure message (SH-372: absence states
//! nothing, and is never promoted to "there was nothing").

use std::path::{Path, PathBuf};

use super::{MARKETPLACE_NAME, PluginTarget, home_dir};
use crate::error::AppError;

/// The provider configuration file that records marketplace registrations.
pub(crate) fn config_path(home: &Path, target: PluginTarget) -> PathBuf {
    match target {
        PluginTarget::ClaudeCode => home.join(".claude/plugins/known_marketplaces.json"),
        PluginTarget::Codex => home.join(".codex/config.toml"),
    }
}

/// The storyhook marketplace source a provider configuration names.
///
/// `Ok(None)` is a configuration with no storyhook marketplace in it; `Err`
/// is a configuration this parser could not read, with the reason, which the
/// caller reports rather than resolving (SH-372: an unreadable record states
/// nothing and is never promoted to "there was nothing").
pub(crate) fn configured_source(
    body: &str,
    target: PluginTarget,
) -> Result<Option<String>, String> {
    match target {
        PluginTarget::ClaudeCode => {
            let value: serde_json::Value = serde_json::from_str(body)
                .map_err(|error| format!("its configuration is invalid JSON: {error}"))?;
            let Some(marketplace) = value.get(MARKETPLACE_NAME) else {
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
        PluginTarget::Codex => {
            let value: toml::Value = toml::from_str(body)
                .map_err(|error| format!("its configuration is invalid TOML: {error}"))?;
            let Some(marketplace) = value
                .get("marketplaces")
                .and_then(|value| value.get(MARKETPLACE_NAME))
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

/// What a provider had registered as the storyhook marketplace before an
/// install started removing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Previous {
    /// A storyhook marketplace registered at this source.
    Registered(String),
    /// No config file, or no storyhook marketplace in it: a fresh install.
    Unregistered,
    /// A config this parser could not read, and the reason. Nothing can be
    /// put back, and the install proceeds regardless (see the module doc).
    Unreadable(String),
}

/// Reads the provider's registration from disk. Never invokes the provider
/// and never fails: the answer is one of three named states.
pub(crate) fn snapshot(target: PluginTarget) -> Previous {
    let Ok(home) = home_dir() else {
        return Previous::Unreadable("could not determine home directory".to_string());
    };
    let path = config_path(&home, target);
    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Previous::Unregistered;
        }
        Err(error) => {
            return Previous::Unreadable(format!(
                "its configuration at `{}` could not be read: {error}",
                path.display()
            ));
        }
    };
    match configured_source(&body, target) {
        Ok(Some(source)) => Previous::Registered(source),
        Ok(None) => Previous::Unregistered,
        Err(reason) => Previous::Unreadable(reason),
    }
}

/// The provider verbs a restore replays, in the installer's own order.
///
/// A struct of functions rather than a trait, because the two providers'
/// helpers already exist as free functions with these exact shapes and the
/// restore has no state of its own to carry between calls.
pub(crate) struct Verbs {
    /// Removes the plugin; tolerates its absence.
    pub(crate) remove_plugin: fn() -> Result<(), AppError>,
    /// Removes the marketplace; tolerates its absence.
    pub(crate) remove_marketplace: fn() -> Result<(), AppError>,
    /// Registers a marketplace at `source`.
    pub(crate) add_marketplace: fn(&str) -> Result<(), AppError>,
    /// Installs the plugin from the registered marketplace.
    pub(crate) add_plugin: fn() -> Result<(), AppError>,
}

/// What a restore did, for the note appended to the install's own error.
enum Outcome {
    /// The previous source is registered again, with the plugin re-added.
    ReRegistered,
    /// There was nothing to put back; what the failed run added is gone.
    RemovedPartial,
    /// The previous registration could not be read, so only the removal ran.
    NothingToRestore(String),
    /// The restore itself failed after the removal.
    Failed(AppError),
}

/// Replays the removes and, when there was one, re-adds the previous
/// registration. Pure over `verbs`, which is what the unit tests substitute.
fn restore(verbs: &Verbs, previous: &Previous) -> Outcome {
    let removal = (verbs.remove_plugin)().and_then(|()| (verbs.remove_marketplace)());
    if let Err(error) = removal {
        return Outcome::Failed(AppError::Storage(format!(
            "while restoring the previous registration: {error}"
        )));
    }
    match previous {
        Previous::Registered(source) => {
            match (verbs.add_marketplace)(source).and_then(|()| (verbs.add_plugin)()) {
                Ok(()) => Outcome::ReRegistered,
                Err(error) => Outcome::Failed(error),
            }
        }
        Previous::Unregistered => Outcome::RemovedPartial,
        Previous::Unreadable(reason) => Outcome::NothingToRestore(reason.clone()),
    }
}

/// The sentence the install's error ends with. Pure, so every phrasing is
/// unit-tested without a provider.
fn note(target: PluginTarget, previous: &Previous, attempted: &str, outcome: &Outcome) -> String {
    match (outcome, previous) {
        (Outcome::ReRegistered, Previous::Registered(source)) if source == attempted => format!(
            "\n\nre-registered the same release source {source} it was registered at before; \
             that registration is not verified — fix the problem above and run \
             `story plugin install {}` again.",
            target.install_token()
        ),
        (Outcome::ReRegistered, Previous::Registered(source)) => {
            format!("\n\nre-registered the previous marketplace at {source}.")
        }
        (Outcome::ReRegistered, _) => {
            unreachable!("a restore re-registers only a Registered previous")
        }
        (Outcome::RemovedPartial, _) => {
            "\n\nremoved the partial registration this install had added; nothing was \
             registered before it ran."
                .to_string()
        }
        (Outcome::NothingToRestore(reason), _) => format!(
            "\n\nremoved the partial registration this install had added, and nothing \
             restored: {reason}. Register the marketplace by hand or fix the provider's \
             configuration, then run `story plugin install {}` again.",
            target.install_token()
        ),
        (Outcome::Failed(error), Previous::Registered(source)) => {
            format!("\n\nAND failed to re-register the previous marketplace at {source}: {error}")
        }
        (Outcome::Failed(error), _) => format!(
            "\n\nAND failed to remove the partial registration this install had added: {error}"
        ),
    }
}

/// Answers a failure after the removes: puts the previous registration back
/// through `verbs` and returns the failure with the outcome appended.
///
/// `attempted` is the source this run was installing, so the note can say
/// when the put-back is the same thing that just failed.
pub(crate) fn undo(
    target: PluginTarget,
    verbs: &Verbs,
    previous: &Previous,
    attempted: &str,
    failure: AppError,
) -> AppError {
    let outcome = restore(verbs, previous);
    AppError::Storage(format!(
        "{failure}{}",
        note(target, previous, attempted, &outcome)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static TRAIL: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static ADD_MARKETPLACE_FAILS: RefCell<bool> = const { RefCell::new(false) };
        static REMOVE_FAILS: RefCell<bool> = const { RefCell::new(false) };
    }

    fn record(step: &'static str) {
        TRAIL.with(|t| t.borrow_mut().push(step));
    }

    fn verbs() -> Verbs {
        TRAIL.with(|t| t.borrow_mut().clear());
        Verbs {
            remove_plugin: || {
                record("remove-plugin");
                Ok(())
            },
            remove_marketplace: || {
                record("remove-marketplace");
                if REMOVE_FAILS.with(|f| *f.borrow()) {
                    return Err(AppError::Storage("marketplace stuck".to_string()));
                }
                Ok(())
            },
            add_marketplace: |_| {
                record("add-marketplace");
                if ADD_MARKETPLACE_FAILS.with(|f| *f.borrow()) {
                    return Err(AppError::Storage("marketplace exploded".to_string()));
                }
                Ok(())
            },
            add_plugin: || {
                record("add-plugin");
                Ok(())
            },
        }
    }

    fn trail() -> Vec<&'static str> {
        TRAIL.with(|t| t.borrow().clone())
    }

    fn failure() -> AppError {
        AppError::Storage("failed to install `story@storyhook`:\nplugin exploded".to_string())
    }

    #[test]
    fn a_previous_registration_is_removed_then_re_added_from_its_source() {
        let previous = Previous::Registered("/previous/1.0.0".to_string());
        let error = undo(
            PluginTarget::ClaudeCode,
            &verbs(),
            &previous,
            "/release/2.0.0",
            failure(),
        );
        assert_eq!(
            trail(),
            [
                "remove-plugin",
                "remove-marketplace",
                "add-marketplace",
                "add-plugin"
            ]
        );
        let text = error.to_string();
        assert!(
            text.starts_with("failed to install `story@storyhook`"),
            "{text}"
        );
        assert!(
            text.ends_with("re-registered the previous marketplace at /previous/1.0.0."),
            "{text}"
        );
        assert!(!text.contains("not verified"), "{text}");
    }

    #[test]
    fn re_registering_the_source_that_just_failed_is_called_unverified() {
        let previous = Previous::Registered("/release/2.0.0".to_string());
        let text = undo(
            PluginTarget::Codex,
            &verbs(),
            &previous,
            "/release/2.0.0",
            failure(),
        )
        .to_string();
        assert!(
            text.contains("re-registered the same release source /release/2.0.0"),
            "{text}"
        );
        assert!(text.contains("not verified"), "{text}");
        assert!(text.contains("`story plugin install codex`"), "{text}");
    }

    #[test]
    fn a_fresh_install_only_removes_what_it_added() {
        let text = undo(
            PluginTarget::ClaudeCode,
            &verbs(),
            &Previous::Unregistered,
            "/release/2.0.0",
            failure(),
        )
        .to_string();
        assert_eq!(trail(), ["remove-plugin", "remove-marketplace"]);
        assert!(text.contains("removed the partial registration"), "{text}");
        assert!(!text.contains("re-registered"), "{text}");
    }

    #[test]
    fn an_unreadable_previous_is_named_and_nothing_is_re_added() {
        let previous = Previous::Unreadable("its configuration is invalid JSON: eof".to_string());
        let text = undo(
            PluginTarget::ClaudeCode,
            &verbs(),
            &previous,
            "/release/2.0.0",
            failure(),
        )
        .to_string();
        assert_eq!(trail(), ["remove-plugin", "remove-marketplace"]);
        assert!(
            text.contains("nothing restored: its configuration is invalid JSON: eof"),
            "{text}"
        );
        assert!(text.contains("`story plugin install claude`"), "{text}");
    }

    #[test]
    fn a_failed_re_add_reports_both_errors_and_the_previous_source() {
        ADD_MARKETPLACE_FAILS.with(|f| *f.borrow_mut() = true);
        let previous = Previous::Registered("/previous/1.0.0".to_string());
        let text = undo(
            PluginTarget::ClaudeCode,
            &verbs(),
            &previous,
            "/release/2.0.0",
            failure(),
        )
        .to_string();
        ADD_MARKETPLACE_FAILS.with(|f| *f.borrow_mut() = false);
        assert_eq!(
            trail(),
            ["remove-plugin", "remove-marketplace", "add-marketplace"]
        );
        assert!(text.contains("plugin exploded"), "{text}");
        assert!(
            text.contains(
                "AND failed to re-register the previous marketplace at /previous/1.0.0: \
                 marketplace exploded"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_failed_removal_during_restore_says_which_phase_it_was_in() {
        REMOVE_FAILS.with(|f| *f.borrow_mut() = true);
        let previous = Previous::Registered("/previous/1.0.0".to_string());
        let text = undo(
            PluginTarget::ClaudeCode,
            &verbs(),
            &previous,
            "/release/2.0.0",
            failure(),
        )
        .to_string();
        REMOVE_FAILS.with(|f| *f.borrow_mut() = false);
        assert_eq!(trail(), ["remove-plugin", "remove-marketplace"]);
        assert!(
            text.contains(
                "AND failed to re-register the previous marketplace at /previous/1.0.0: \
                 while restoring the previous registration: marketplace stuck"
            ),
            "{text}"
        );
    }

    #[test]
    fn configured_source_distinguishes_absent_unregistered_and_unreadable() {
        for (target, registered, unregistered, unreadable) in [
            (
                PluginTarget::ClaudeCode,
                r#"{"storyhook":{"source":{"source":"directory","path":"/p"}}}"#,
                r#"{"other":{}}"#,
                "{ nope",
            ),
            (
                PluginTarget::Codex,
                "[marketplaces.storyhook]\nsource_type = \"local\"\nsource = \"/p\"\n",
                "[marketplaces.other]\nsource = \"/q\"\n",
                "[marketplaces.storyhook\nsource = 1",
            ),
        ] {
            assert_eq!(
                configured_source(registered, target),
                Ok(Some("/p".to_string())),
                "{target:?}"
            );
            assert_eq!(
                configured_source(unregistered, target),
                Ok(None),
                "{target:?}"
            );
            assert!(configured_source(unreadable, target).is_err(), "{target:?}");
        }
    }
}
