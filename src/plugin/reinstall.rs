//! `story plugin reinstall`: re-point every provider that has the storyhook
//! plugin *registered* at the marketplace projection this binary carries
//! (SH-667).
//!
//! # Why this exists
//!
//! The plugin travels inside the binary and is projected per version
//! (`materialize_release_marketplace`), so replacing the binary — `make
//! install`, `story update`, `install.sh` — leaves every provider registered
//! at the previous release's projection until someone re-runs `story plugin
//! install <provider>` by hand. `story doctor install` names that state
//! `STALE RELEASE`; SH-584's RCA saw 2.4.0 plugins under a 2.4.2 CLI. Every
//! binary-replacement path now runs this verb, so the plugins never need a
//! separate update.
//!
//! # What "installed" means here
//!
//! A provider is reinstalled when its own configuration registers the
//! storyhook marketplace ([`registration::snapshot`] answers
//! [`Previous::Registered`]). The registration is the provider's statement of
//! intent, and the parser is the one `story doctor install` already reads with
//! — never a second copy (SH-136).
//!
//! Installed copies **without** a registration are not reinstalled. That is the
//! DEREGISTERED state SH-640 taught the doctor to name, and it is answered the
//! same way here: a warning naming `story plugin install <provider>`, because
//! absence of a registration is never promoted to intent (SH-372). A
//! configuration this parser cannot read is likewise a warning, never a block
//! on the other provider (the SH-404/SH-405 trap).
//!
//! # Two halves, one loop
//!
//! [`plan`] reads the machine; [`execute`] runs a plan through whatever
//! installs — in-process [`super::install`] for the verb, a spawned copy of the
//! freshly installed executable for `story update`, which must not install
//! from its own, now-stale, embedded payload. The loop, the aggregation and the
//! wording live once, here, and are unit-tested without a provider.

use std::path::{Path, PathBuf};

use super::registration::{Previous, snapshot};
use super::{PluginTarget, install_residue};
use crate::error::AppError;

/// Every provider this binary can install, in the order they are reported.
pub(crate) const TARGETS: [PluginTarget; 2] = [PluginTarget::ClaudeCode, PluginTarget::Codex];

/// The retry the messages name whenever something was left undone.
const RETRY: &str = "run `story plugin reinstall` to retry";

/// One provider's state on this machine, as read before anything runs.
pub(crate) struct Reading {
    pub(crate) target: PluginTarget,
    pub(crate) previous: Previous,
    /// Installed copies present on disk (`install_residue`), consulted only
    /// when nothing is registered.
    pub(crate) residue: Vec<PathBuf>,
}

/// What a reinstall will do: the providers to reinstall, and the findings it
/// will not act on but must not stay silent about.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) targets: Vec<PluginTarget>,
    pub(crate) warnings: Vec<String>,
}

/// What a completed reinstall reports: one paragraph per provider, and the
/// plan's warnings carried through unchanged.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub message: String,
    pub warnings: Vec<String>,
}

/// Reads both providers' registrations and residue.
pub(crate) fn plan() -> Plan {
    let readings: Vec<Reading> = TARGETS
        .into_iter()
        .map(|target| Reading {
            target,
            previous: snapshot(target),
            // A residue lookup fails only when the home directory cannot be
            // resolved, in which case the snapshot is already `Unreadable`
            // with that exact reason; nothing is lost by reading it as empty.
            residue: install_residue(target).unwrap_or_default(),
        })
        .collect();
    plan_from(&readings)
}

/// Pure over the readings: what to reinstall, and what to warn about.
pub(crate) fn plan_from(readings: &[Reading]) -> Plan {
    let mut plan = Plan::default();
    for reading in readings {
        let name = reading.target.display_name();
        let token = reading.target.install_token();
        match &reading.previous {
            Previous::Registered(_) => plan.targets.push(reading.target),
            Previous::Unregistered if reading.residue.is_empty() => {}
            Previous::Unregistered => {
                let copies = reading
                    .residue
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                plan.warnings.push(format!(
                    "{name}: the storyhook marketplace is not registered, but installed \
                     copies remain at {copies} — not reinstalled; run `story plugin install \
                     {token}` to register it again"
                ));
            }
            Previous::Unreadable(reason) => plan.warnings.push(format!(
                "{name}: its registration could not be read ({reason}) — not reinstalled; \
                 run `story plugin install {token}` if it was installed here"
            )),
        }
    }
    plan
}

/// Runs every target in `plan` through `install`, never stopping at the first
/// failure: a provider that can be refreshed is refreshed even when its
/// sibling cannot, and the result names every outcome.
///
/// `Err` when any target failed. Its text carries the successes too, and the
/// plan's warnings, because an error is the only thing the caller prints.
pub(crate) fn execute(
    plan: &Plan,
    mut install: impl FnMut(PluginTarget) -> Result<String, AppError>,
) -> Result<Report, AppError> {
    if plan.targets.is_empty() {
        return Ok(Report {
            message: "no provider has the storyhook plugin registered; nothing to reinstall"
                .to_string(),
            warnings: plan.warnings.clone(),
        });
    }
    let mut paragraphs = Vec::new();
    let mut failed = 0usize;
    for target in &plan.targets {
        let name = target.display_name();
        match install(*target) {
            Ok(message) => {
                paragraphs.push(format!(
                    "reinstalled the {name} plugin:\n{}",
                    message.trim_end()
                ));
            }
            Err(error) => {
                failed += 1;
                paragraphs.push(format!("failed to reinstall the {name} plugin: {error}"));
            }
        }
    }
    let message = paragraphs.join("\n\n");
    if failed == 0 {
        return Ok(Report {
            message,
            warnings: plan.warnings.clone(),
        });
    }
    let mut text = message;
    for warning in &plan.warnings {
        text.push_str("\n\nwarning: ");
        text.push_str(warning);
    }
    text.push_str(&format!(
        "\n\n{failed} of {} provider plugin(s) could not be reinstalled; {RETRY}",
        plan.targets.len()
    ));
    Err(AppError::Storage(text))
}

/// The verb: reinstall every registered provider through this binary's own
/// installer.
pub fn run(project_root: &Path) -> Result<Report, AppError> {
    execute(&plan(), |target| {
        super::install(target.install_token(), project_root)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(target: PluginTarget, previous: Previous, residue: &[&str]) -> Reading {
        Reading {
            target,
            previous,
            residue: residue.iter().map(PathBuf::from).collect(),
        }
    }

    fn registered(target: PluginTarget) -> Reading {
        reading(target, Previous::Registered("/release/1.0.0".into()), &[])
    }

    #[test]
    fn every_registered_provider_is_reinstalled_and_nothing_else_is() {
        let plan = plan_from(&[
            registered(PluginTarget::ClaudeCode),
            registered(PluginTarget::Codex),
        ]);
        assert_eq!(
            plan.targets,
            vec![PluginTarget::ClaudeCode, PluginTarget::Codex]
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        let plan = plan_from(&[
            reading(PluginTarget::ClaudeCode, Previous::Unregistered, &[]),
            registered(PluginTarget::Codex),
        ]);
        assert_eq!(plan.targets, vec![PluginTarget::Codex]);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn a_machine_with_nothing_registered_and_nothing_left_behind_plans_nothing() {
        let plan = plan_from(&[
            reading(PluginTarget::ClaudeCode, Previous::Unregistered, &[]),
            reading(PluginTarget::Codex, Previous::Unregistered, &[]),
        ]);
        assert_eq!(plan, Plan::default());
    }

    /// The DEREGISTERED state (SH-640): copies on disk, no registration. Not
    /// reinstalled — absence is never promoted to intent (SH-372) — but named,
    /// with the same remedy `story doctor install` prescribes.
    #[test]
    fn residue_without_a_registration_is_a_warning_not_a_reinstall() {
        let plan = plan_from(&[
            reading(
                PluginTarget::ClaudeCode,
                Previous::Unregistered,
                &["/home/u/.claude/plugins/cache/storyhook"],
            ),
            registered(PluginTarget::Codex),
        ]);
        assert_eq!(plan.targets, vec![PluginTarget::Codex]);
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        let warning = &plan.warnings[0];
        assert!(warning.starts_with("Claude Code:"), "{warning}");
        assert!(
            warning.contains("/home/u/.claude/plugins/cache/storyhook"),
            "{warning}"
        );
        assert!(warning.contains("not reinstalled"), "{warning}");
        assert!(
            warning.contains("`story plugin install claude`"),
            "{warning}"
        );
    }

    /// An unreadable configuration never blocks the other provider (the
    /// SH-404/SH-405 trap) and is never read as "nothing there" (SH-372).
    #[test]
    fn an_unreadable_registration_is_a_warning_naming_the_reason() {
        let plan = plan_from(&[
            registered(PluginTarget::ClaudeCode),
            reading(
                PluginTarget::Codex,
                Previous::Unreadable("its configuration is invalid TOML: eof".into()),
                &["/home/u/.codex/plugins/cache/storyhook"],
            ),
        ]);
        assert_eq!(plan.targets, vec![PluginTarget::ClaudeCode]);
        assert_eq!(plan.warnings.len(), 1, "{:?}", plan.warnings);
        let warning = &plan.warnings[0];
        assert!(warning.starts_with("Codex:"), "{warning}");
        assert!(
            warning.contains("its configuration is invalid TOML: eof"),
            "{warning}"
        );
        assert!(
            warning.contains("`story plugin install codex`"),
            "{warning}"
        );
    }

    #[test]
    fn an_empty_plan_reports_nothing_to_do_and_keeps_its_warnings() {
        let plan = Plan {
            targets: Vec::new(),
            warnings: vec!["Codex: left behind".into()],
        };
        let mut calls = 0;
        let report = execute(&plan, |_| {
            calls += 1;
            Ok(String::new())
        })
        .expect("nothing to do is a success");
        assert_eq!(calls, 0, "nothing may be installed for an empty plan");
        assert!(
            report.message.contains("nothing to reinstall"),
            "{report:?}"
        );
        assert_eq!(report.warnings, vec!["Codex: left behind".to_string()]);
    }

    #[test]
    fn every_target_runs_in_order_and_the_report_names_each() {
        let plan = Plan {
            targets: vec![PluginTarget::ClaudeCode, PluginTarget::Codex],
            warnings: Vec::new(),
        };
        let mut trail = Vec::new();
        let report = execute(&plan, |target| {
            trail.push(target);
            Ok(format!(
                "registered {} at /release\n",
                target.install_token()
            ))
        })
        .expect("both installs succeeded");
        assert_eq!(trail, vec![PluginTarget::ClaudeCode, PluginTarget::Codex]);
        assert_eq!(
            report.message,
            "reinstalled the Claude Code plugin:\nregistered claude at /release\n\n\
             reinstalled the Codex plugin:\nregistered codex at /release"
        );
        assert!(report.warnings.is_empty());
    }

    /// A failure never stops the sibling, and the error names every outcome,
    /// the warnings that would otherwise be lost, and the retry.
    #[test]
    fn a_failed_target_does_not_stop_the_next_and_the_error_names_everything() {
        let plan = Plan {
            targets: vec![PluginTarget::ClaudeCode, PluginTarget::Codex],
            warnings: vec!["a warning that must survive".into()],
        };
        let mut trail = Vec::new();
        let error = execute(&plan, |target| {
            trail.push(target);
            match target {
                PluginTarget::ClaudeCode => {
                    Err(AppError::Storage("claude plugin install exploded".into()))
                }
                PluginTarget::Codex => Ok("registered codex".into()),
            }
        })
        .expect_err("one failure fails the whole reinstall");
        assert_eq!(trail, vec![PluginTarget::ClaudeCode, PluginTarget::Codex]);
        let text = error.to_string();
        assert!(
            text.contains(
                "failed to reinstall the Claude Code plugin: claude plugin install exploded"
            ),
            "{text}"
        );
        assert!(
            text.contains("reinstalled the Codex plugin:\nregistered codex"),
            "{text}"
        );
        assert!(
            text.contains("warning: a warning that must survive"),
            "{text}"
        );
        assert!(
            text.contains("1 of 2 provider plugin(s) could not be reinstalled"),
            "{text}"
        );
        assert!(text.ends_with(RETRY), "{text}");
    }
}
