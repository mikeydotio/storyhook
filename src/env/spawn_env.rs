//! The allowlists for the two spawn families storyhook itself trusts: `story.sh`
//! (dispatch) and provider CLIs (plugin management) — SH-193, the sibling of
//! [`super::git_env`] this module's header pointed at.
//!
//! # Why these two, and not the third
//!
//! SH-193 named three children a daemon hands its whole inherited environment
//! to with no `env_clear`: a user's own event hook (`sh -c`,
//! [`crate::event_hooks`]), `story.sh`'s dispatch child
//! ([`crate::api::dispatch::run_child`]), and provider plugin subcommands
//! ([`crate::plugin`]). Only the last two are addressed here.
//!
//! An event hook's command is arbitrary, user-authored shell — storyhook does
//! not know what it needs, the same reasoning [`super::git_env`]'s own header
//! already gives for why the *process*-layer scrub can only be a denylist.
//! Clearing it risks breaking a hook that legitimately reads an ambient
//! variable, for a security benefit that is small (a hook author already has
//! arbitrary shell execution — narrowing its environment does not narrow what
//! it can do). `event_hooks.rs`'s own doc comment names this explicitly
//! instead: a hook contract is "you inherit the daemon's environment, which
//! may be stale and from a different project than the one invoking you."
//!
//! `story.sh` and provider plugin CLIs are different: storyhook chooses every argument on
//! their command lines. They are exactly the case [`super::git_env`]'s header
//! calls "a command can be, and therefore is, an allowlist" — cleared, then
//! handed back exactly what is measured to be needed, so a variable nobody
//! has heard of yet (an exported `AWS_SECRET_ACCESS_KEY`, an `OPENAI_API_KEY`)
//! is excluded by construction rather than by enumeration.
//!
//! # Two lists, not one — and a third that is one of them plus GitHub
//!
//! `story.sh` reads roughly fifty environment variables — the great majority
//! under its own `STORY_`/`STORYHOOK_` prefix, the tuning knobs
//! (`STORY_READY_ATTEMPTS`, `STORY_CONFIRM_DELAY`, `STORY_PROMPT`, …) that are
//! its whole designed configuration surface — plus `TMPDIR` for scratch
//! work. A daemon's inherited `TMUX` and `TMUX_PANE` belong to whichever
//! terminal started it, not to a later dashboard dispatch. Excluding them
//! makes daemon-owned helper work use the default server. Direct interactive
//! helper invocation does not cross this Rust boundary and retains its
//! caller's context; cleanup leases carry their exact socket separately.
//! Provider marketplace/install/
//! uninstall commands ([`crate::plugin`]) need none of that: they are a
//! narrow, local plugin-management call with no `STORY_` surface and no tmux
//! session of its own. Handing it `story.sh`'s list would be unjustified
//! over-permissiveness for a spawn that never asked for it — the same
//! objection [`super::git_env`]'s council raised against a single shared list
//! for every git invocation.
//!
//! The centralized verifier spawns two more children, and they sit on
//! opposite sides of that line. `scripts/verify-pr.sh` is orchestration with
//! GitHub credentials and nothing of `story.sh`'s surface
//! ([`apply_verification_allowlist`]). The submission helper
//! (`story.sh submit`, SH-647) IS `story.sh` — it runs `story` against the
//! daemon's own store, so it needs everything the dispatch list carries, the
//! `TEST_ENVIRONMENT` isolation parameters included — and it also pushes the
//! leased branch and opens the pull request, so it needs the GitHub
//! credentials too. [`apply_submission_allowlist`] is therefore the dispatch
//! list plus [`GITHUB_CREDENTIAL_MAY_SEE`], never the verification list plus
//! a hand-picked subset of `STORYHOOK_*` names: the fence that every
//! isolation parameter survives is derived over the table, and a helper child
//! that could not isolate itself is SH-633's second daemon all over again.
//! The helper runs no repository tests, so the `merge-watch.sh` scrub that
//! strips the tokens before a gate runs has no analogue here.
//!
//! # Two names deliberately left off, and why
//!
//! Decided by SH-193's council (unanimous, three seats) after
//! its first-round proposals were rejected on this exact point: both had
//! copied [`super::git_env`]'s `GIT_MAY_SEE` verbatim, which omits
//! `SSH_AUTH_SOCK` — safely, for `git_env`, only because *its* call sites
//! (`src/service/git.rs`'s `rev-parse`/`log`) are local-only and need no
//! remote auth at all. `story.sh` is not local-only: it runs
//! `git fetch --quiet origin` (`plugins/story/bin/story.sh`) and
//! `git ls-remote --heads origin` (`plugins/story/lib/session.sh`)
//! directly, against a real remote.
//!
//! Both call sites were read before this list was written. Both are already
//! best-effort: `git fetch`'s exit code is captured and a failure falls back
//! to a cached `origin/<default>` ref rather than blocking dispatch (the
//! `base_fresh`/`base_note` handling in `story.sh`), and `freshen_base_ref`'s
//! own doc comment already states its contract as "any fetch failure is
//! swallowed." So a dispatch whose remote needs `SSH_AUTH_SOCK` to fetch does
//! not fail — it silently bases the new worktree on a **stale** cached ref
//! instead of a fresh one. That is a real, user-visible behavior change from
//! today, and it is accepted rather than fixed by forwarding the socket,
//! because an SSH agent socket is not a tuning knob — it is a live credential
//! handle, functionally equivalent to the exported secrets this story exists
//! to stop leaking, and putting it back on the allowlist for convenience would
//! undercut the fix. **Known limitation, not an oversight**: if this proves to
//! break real SSH-remote dispatch workflows, the redesign trigger is a report
//! of exactly that, and the fix is a fresher-base warning surfaced to the
//! dispatch caller, not a wider allowlist.
//!
//! The second omission is any `claude`-auth variable (`ANTHROPIC_API_KEY` and
//! siblings). `story.sh`'s `LAUNCH_TPL` starts the real coding-agent session
//! this dispatch exists to create, inside a **new** tmux session whose
//! environment is captured from this allowlist at creation time — so a
//! `claude` install authenticated only via such a variable, never via
//! `claude login`'s persisted `~/.claude` credentials, would fail to
//! authenticate on first dispatch. `HOME` is on the allowlist, which is what
//! makes persisted login work unaffected; an env-var-only auth is not. This is
//! deliberately not accommodated for the same reason `SSH_AUTH_SOCK` is not: a
//! model-provider API key is exactly the shape of secret named in SH-193's own
//! filing (`OPENAI_API_KEY`, alongside it), and forwarding it back in in a
//! module written to stop that would be the fix rejecting its own premise. A
//! **pre-existing** tmux session for a project is entirely unaffected — tmux
//! captured its environment before this fix ever ran — so this narrows, but
//! does not close, the coding agent's real ambient exposure; that gap is
//! logged here rather than implied away. Known limitation; the redesign
//! trigger is the same as above, a report of real breakage.

use std::process::Command;

/// Names both trusted spawns below may see unconditionally: how to find and
/// run the child ([`PATH`](Self)), where its own configuration and credential
/// store live (`HOME`, and the XDG base directories that refine it), where to
/// stage scratch files (`TMPDIR`), and the locale/terminal identity that
/// changes only *how* it behaves, never which data it can reach (`USER`,
/// `SHELL`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM`).
///
/// The three `XDG_*` names travel with `HOME` because they are not separable
/// from it (SH-633): a child handed `HOME` but not `XDG_STATE_HOME` resolves a
/// *different* state home than its parent did, and a `story` run inside a
/// dispatch child then looks for the store's daemon under the developer's
/// real `~/.local/state/storyhook`, finds nothing, and starts a second one
/// there. None of the three is a credential — they say where a program's
/// files live, which is exactly what `HOME` already says — so admitting them
/// narrows nothing this allowlist exists to protect.
const COMMON_MAY_SEE: [&str; 12] = [
    "PATH",
    "HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "TMPDIR",
    "USER",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
];

/// Prefixes of `story.sh`'s own designed configuration surface — every
/// `STORY_*`/`STORYHOOK_*` tuning knob it reads, passed through by prefix
/// rather than enumerated by name so a new one story.sh grows does not
/// silently stop working. Storyhook's own naming convention is exactly what
/// makes a prefix allowlist safe here where it would not be for a third
/// party's variables: nothing outside storyhook is permitted to define a name
/// under either prefix, and both prefixes are unique to it.
const DISPATCH_MAY_SEE_PREFIXES: [&str; 2] = ["STORY_", "STORYHOOK_"];

/// The names through which `gh` and an HTTPS `git push` find the operator's
/// GitHub credentials, stated once so every spawn that is permitted to reach
/// GitHub admits exactly the same three (SH-136: a second hand-copied list is a
/// list that drifts). `GH_CONFIG_DIR` relocates `gh`'s own config and token
/// store; `GH_TOKEN`/`GITHUB_TOKEN` are the token itself.
const GITHUB_CREDENTIAL_MAY_SEE: [&str; 3] = ["GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN"];

/// Configuration the centralized verifier needs in addition to the common
/// executable/user environment and the GitHub credentials. The tokens stop at
/// its orchestration process; the shell boundary removes them before the
/// repository test runs.
const VERIFICATION_EXTRA_MAY_SEE: [&str; 1] = ["STORYHOOK_LOCK_DIR"];

/// True if `name` is one `story.sh`'s dispatch child is allowed to see.
fn dispatch_permits(name: &str) -> bool {
    COMMON_MAY_SEE.contains(&name)
        || DISPATCH_MAY_SEE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// True if `name` is one `claude`'s plugin-management child is allowed to see.
///
/// Narrower than [`dispatch_permits`] on purpose — see this module's header.
fn plugin_cli_permits(name: &str) -> bool {
    COMMON_MAY_SEE.contains(&name)
}

/// True if `name` is one the verifier's submission helper may see: the whole
/// dispatch surface (it is `story.sh`, and it runs `story`) plus the GitHub
/// credentials it pushes and opens the pull request with.
fn submission_permits(name: &str) -> bool {
    dispatch_permits(name) || GITHUB_CREDENTIAL_MAY_SEE.contains(&name)
}

/// True if `name` is needed by the centralized GitHub/gate subprocess.
fn verification_permits(name: &str) -> bool {
    COMMON_MAY_SEE.contains(&name)
        || GITHUB_CREDENTIAL_MAY_SEE.contains(&name)
        || VERIFICATION_EXTRA_MAY_SEE.contains(&name)
}

/// Clears `command`'s environment, then restores every currently-set variable
/// for which `permits` returns true.
///
/// Cleared and rebuilt rather than filtered from a denylist, so a variable
/// this list has never heard of is excluded by default — the same argument
/// [`super::git_env::command`]'s doc comment makes for `git`.
fn apply_allowlist(command: &mut Command, permits: impl Fn(&str) -> bool) {
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if permits(name) {
            command.env(name, value);
        }
    }
}

/// Clears `command`'s environment and restores exactly what `story.sh`'s
/// dispatch child may see. Call before any of `run_child`'s own explicit
/// `.env(...)` calls (`STORY_BIN`, `STORYHOOK_STORE_PATH`, …) — `env_clear`
/// removes anything set before it runs, explicit values included.
/// Ambient terminal handles are deliberately excluded: daemon-owned work
/// targets the default tmux server, or an explicit cleanup-lease socket.
pub fn apply_dispatch_allowlist(command: &mut Command) {
    apply_allowlist(command, dispatch_permits);
}

/// Clears `command`'s environment and restores exactly what a provider
/// plugin-management child may see.
pub fn apply_plugin_cli_allowlist(command: &mut Command) {
    apply_allowlist(command, plugin_cli_permits);
}

/// Clears `command`'s environment and restores only execution, GitHub auth,
/// and machine-lock configuration needed by centralized verification.
pub fn apply_verification_allowlist(command: &mut Command) {
    apply_allowlist(command, verification_permits);
}

/// Clears `command`'s environment and restores what the verifier's leased
/// submission helper may see: everything [`apply_dispatch_allowlist`] admits,
/// plus the GitHub credentials. Call before the explicit `.env(...)` calls
/// (`STORY_BIN`, the lease, `GH_PROMPT_DISABLED`), which `env_clear` would
/// otherwise remove.
pub fn apply_submission_allowlist(command: &mut Command) {
    apply_allowlist(command, submission_permits);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The allowlist is the child's whole environment** — asserted against a
    /// real child, not against the `Command` that describes it, for the same
    /// reason [`super::super::git_env`]'s own test of this shape gives:
    /// `Command::get_envs` reports only what a caller explicitly set and says
    /// nothing about what would otherwise be inherited, so a test written
    /// against it cannot tell `env_clear` from no `env_clear`.
    fn assert_allowlist_is_the_childs_whole_environment(
        apply: impl Fn(&mut Command),
        permits: impl Fn(&str) -> bool,
    ) {
        // A name on neither list, and a real credential shape — if it
        // survives into the child, the environment was inherited rather than
        // rebuilt, and no denylist would ever have caught it.
        let smuggled = "OPENAI_API_KEY";
        assert!(
            !permits(smuggled),
            "the probe's own marker must not be on the allowlist"
        );

        let mut probe = Command::new("/usr/bin/env");
        apply(&mut probe);
        // Set *after* the allowlist is applied, exactly as a poisoned
        // parent's environment would already hold it, so this proves
        // `env_clear` rather than call ordering.
        probe.env(smuggled, "smuggled-through");

        let seen = probe.output().expect("running /usr/bin/env");
        assert!(seen.status.success(), "the probe child must run");
        let names: Vec<String> = String::from_utf8_lossy(&seen.stdout)
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
            .collect();

        for name in &names {
            assert!(
                permits(name) || name == smuggled,
                "`{name}` reached the child but is not on the allowlist; the environment was \
                 inherited rather than rebuilt"
            );
        }
        assert!(
            names.iter().any(|name| name == smuggled),
            "the probe is broken: its own marker never arrived, so this test proves nothing"
        );
    }

    #[test]
    fn the_dispatch_allowlist_is_the_childs_whole_environment() {
        assert_allowlist_is_the_childs_whole_environment(
            apply_dispatch_allowlist,
            dispatch_permits,
        );
    }

    #[test]
    fn the_plugin_cli_allowlist_is_the_childs_whole_environment() {
        assert_allowlist_is_the_childs_whole_environment(
            apply_plugin_cli_allowlist,
            plugin_cli_permits,
        );
    }

    #[test]
    fn the_verification_allowlist_is_the_childs_whole_environment() {
        assert_allowlist_is_the_childs_whole_environment(
            apply_verification_allowlist,
            verification_permits,
        );
        assert!(verification_permits("GH_TOKEN"));
        assert!(verification_permits("STORYHOOK_LOCK_DIR"));
        assert!(!verification_permits("STORYHOOK_STORE_PATH"));
        assert!(!verification_permits("OPENAI_API_KEY"));
    }

    /// The submission helper is `story.sh` with GitHub credentials: it must
    /// see exactly what dispatch sees plus the three credential names, and the
    /// credential list must be the one the verification list shares rather
    /// than a second spelling of it.
    #[test]
    fn the_submission_allowlist_is_the_childs_whole_environment() {
        assert_allowlist_is_the_childs_whole_environment(
            apply_submission_allowlist,
            submission_permits,
        );
        for name in GITHUB_CREDENTIAL_MAY_SEE {
            assert!(submission_permits(name), "submission dropped {name}");
            assert!(verification_permits(name), "verification dropped {name}");
        }
        assert!(submission_permits("STORYHOOK_STORE_PATH"));
        assert!(submission_permits("STORY_READY_ATTEMPTS"));
        assert!(!submission_permits("OPENAI_API_KEY"));
        assert!(!submission_permits("SSH_AUTH_SOCK"));
        assert!(
            !dispatch_permits("GH_TOKEN"),
            "dispatch must not carry GitHub credentials; only the submission helper does"
        );
    }

    /// `story.sh`'s own tuning surface must survive by prefix, not just by
    /// the fixed names above — this is the property the whole prefix design
    /// exists for, so it is pinned directly rather than left to be implied by
    /// the probe test's use of a single unrelated marker.
    #[test]
    fn a_story_prefixed_variable_survives_the_dispatch_allowlist() {
        assert!(dispatch_permits("STORY_READY_ATTEMPTS"));
        assert!(dispatch_permits("STORYHOOK_PROJECT"));
        assert!(!dispatch_permits("STORYWRITER_ANYTHING"));
    }

    #[test]
    fn daemon_owned_helpers_do_not_inherit_terminal_identity() {
        for name in ["TMUX", "TMUX_PANE"] {
            assert!(!dispatch_permits(name), "dispatch inherited {name}");
            assert!(!submission_permits(name), "submission inherited {name}");
            assert!(!verification_permits(name), "verification inherited {name}");
            assert!(
                !plugin_cli_permits(name),
                "plugin management inherited {name}"
            );
        }
    }

    /// Every isolation parameter reaches a dispatch child (SH-633).
    ///
    /// `story.sh` runs `story`, so a dispatch child resolves its own
    /// [`crate::env::Environment`] from the environment this allowlist hands
    /// it. `TEST_ENVIRONMENT` is the one statement of which variables decide
    /// *what that resolution reaches* — the store, the daemon, credentials —
    /// so any parameter the allowlist drops is a fact the child gets from the
    /// developer's real environment instead of its parent's. That is how a
    /// child handed `STORYHOOK_STORE_PATH` but not `XDG_STATE_HOME` found no
    /// daemon under the real state home and started a second one for a
    /// fixture store, 1,199 times on one machine. Derived from the table, so
    /// a parameter added there and forgotten here fails the build; behavioural
    /// completeness (the allowlist IS the child's whole environment) is the
    /// probe test above.
    #[test]
    fn every_test_environment_parameter_survives_the_dispatch_allowlist() {
        let dropped: Vec<&str> = crate::env::test_environment::TEST_ENVIRONMENT
            .iter()
            .map(|parameter| parameter.name)
            .filter(|name| !dispatch_permits(name))
            .collect();
        assert!(
            dropped.is_empty(),
            "the dispatch allowlist drops {dropped:?}; a `story` run inside the child \
             resolves those from the developer's real environment rather than its parent's"
        );
    }

    /// The submission helper runs `story` too (SH-647), so the same derived
    /// fence applies to its list: a parameter the table gains and this list
    /// drops would be resolved from the developer's real environment.
    #[test]
    fn every_test_environment_parameter_survives_the_submission_allowlist() {
        let dropped: Vec<&str> = crate::env::test_environment::TEST_ENVIRONMENT
            .iter()
            .map(|parameter| parameter.name)
            .filter(|name| !submission_permits(name))
            .collect();
        assert!(
            dropped.is_empty(),
            "the submission allowlist drops {dropped:?}; a `story` run inside the helper \
             resolves those from the developer's real environment rather than its parent's"
        );
    }

    /// The narrower plugin-CLI list must reject what the dispatch list allows,
    /// proving the two are genuinely separate rather than one list reused.
    #[test]
    fn the_plugin_cli_allowlist_rejects_dispatch_only_names() {
        assert!(!plugin_cli_permits("STORY_READY_ATTEMPTS"));
        assert!(!plugin_cli_permits("STORYHOOK_PROJECT"));
    }
}
