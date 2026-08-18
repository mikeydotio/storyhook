//! Refuses to give a binary a permanent seat on this machine.
//!
//! `story daemon install` writes a launchd agent naming an executable and
//! loads it. What it creates is not a command's effect but a **declaration**:
//! launchd runs that exact path at every login, before any terminal exists,
//! until somebody runs the command again. Nothing checked what it was being
//! asked to enthrone.
//!
//! Two consequences compounded (SH-411). `./target/debug/story daemon install`,
//! run from a worktree, made that worktree's debug build the machine's
//! persistent login daemon — durable across reboots, with no notice. And a
//! launchd-started process inherits launchd's own minimal `PATH`
//! (`/usr/bin:/bin:/sbin:/usr/sbin`), which never contains `story`, so
//! [`crate::migration_guard`] — the guard that exists to catch "a binary that
//! is not the one you run is about to migrate your store" — takes its
//! fail-open branch for exactly this daemon at every launch and cannot see it.
//! SH-404's failure mode therefore survived SH-404's own fix, specifically for
//! any daemon launchd starts, in total silence.
//!
//! # Why this is not [`crate::migration_guard`] with different arguments
//!
//! The two guards ask the same question of the machine and reach **opposite**
//! conclusions from a missing answer, which is why they share
//! [`crate::path_identity`] and nothing else.
//!
//! `migration_guard` fails open when `$PATH` names no `story`, because in that
//! instant nothing is provably at risk: there is no installed binary whose
//! schema a migration could break. That reasoning does not transfer. This
//! command is not a claim about this run — it writes something that outlives
//! every binary on the machine. Absence here is not "nothing at risk", it is
//! *"I cannot confirm this binary is the one you run"*, and SH-372's doctrine
//! resolves absence against what the reader already believes rather than
//! promoting it to a negative answer. The belief is that a login agent names
//! the installed `story`. So [`Refusal::Unconfirmable`] refuses, with a way
//! through.
//!
//! Sharing `decide` across the two would have meant one function switching on
//! a purpose discriminator, and would have forced this caller to invent a
//! `store_path`, an `is_default`, a `from_version` and a `to_version` it has
//! no business knowing — lying to a pure function to get an answer out of it,
//! the SH-364 shape.
//!
//! # Scope: the machine, not the store
//!
//! `migration_guard` scopes itself to [`crate::env::StoreLocation::is_default`]
//! because the harm it prevents is *whose schema gets advanced*. This gate must
//! not, and the reason is structural: there is exactly one launchd label and
//! one plist path for the whole machine
//! ([`crate::daemon::agent::LAUNCHD_LABEL`]), so
//! `STORYHOOK_STORE_PATH=… story daemon install` overwrites *the* login agent.
//! A store-scoped gate would leave the entire hole open behind one environment
//! variable. This gate is about which binary gets a permanent seat, never about
//! which store it will serve.
//!
//! # Why the override is a flag
//!
//! `--this-binary`, not an environment variable. `migration_guard`'s override
//! had to be a variable because the process needing it is a daemon nobody types
//! at; `story daemon install` is typed by a human at a terminal, and a
//! misspelled flag is refused by name where a misspelled variable is silently
//! inert and an exported one authorizes every future install.

use std::path::PathBuf;

use crate::path_identity::InstalledStory;

/// The facts this gate decides on, already resolved.
///
/// Every field is a plain value rather than something [`decide`] would have to
/// go compute, so the decision does no I/O and the whole truth table is a table
/// of literals in a test — the split [`crate::migration_guard`] draws, for the
/// reason stated there.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// This process's effective user id. Read from
    /// [`crate::daemon::commands::user_id`] rather than a second `getuid`, so
    /// the guard and the launchd domain the install targets are two readings of
    /// one fact rather than two facts (SH-136).
    pub uid: u32,
    /// This process's own executable, in both spellings.
    pub running: InstalledStory,
    /// The `story` `$PATH` resolves, or `None` when it names none.
    pub installed_story: Option<InstalledStory>,
    /// Whether `--this-binary` was written.
    pub this_binary: bool,
}

/// What the gate permits, and the spelling to enthrone.
///
/// A bare `Ok(())` would not have been enough: *which* path goes into the plist
/// is part of the decision. See [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The path to write into `ProgramArguments`.
    pub enthrone: PathBuf,
}

/// Why the gate refused.
///
/// An enum rather than a `String`, because the refusals make **different
/// claims** — one says "that is not the binary you run", the other says "I
/// cannot tell what binary you run" — and a test asserting on prose could not
/// tell them apart, so a later edit could collapse them into one message with
/// nothing failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Running as root. A launchd *user* agent has no meaning here.
    Root,
    /// The running binary is not the `story` `$PATH` runs.
    Disagrees {
        running: PathBuf,
        installed: PathBuf,
    },
    /// `$PATH` names no `story`, so nothing can confirm this binary is the one
    /// the operator runs.
    Unconfirmable { running: PathBuf },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root => write!(
                f,
                "refusing to register a login agent as root: `story daemon install` writes a \
                 launchd *user* agent, which has no meaning for root — it would be bootstrapped \
                 into `gui/0`, and its plist, its store and its log path would all be resolved \
                 against root's home rather than yours.\n\n\
                 Nothing was written; no agent was loaded or replaced.\n\n\
                 Run it again without `sudo`, as the user whose login it should start at."
            ),
            Self::Disagrees { running, installed } => write!(
                f,
                "refusing to register `{running}` as your login agent: your $PATH runs \
                 `{installed}`.\n\n\
                 launchd runs the path in this plist verbatim, at every login, until this \
                 command is run again — and the process it starts has no $PATH of its own to \
                 check against, which makes it the one process the migration guard structurally \
                 cannot see. Registering a binary you do not otherwise run makes that build the \
                 machine's daemon.\n\n\
                 Nothing was written; no agent was loaded or replaced.\n\n\
                 If you meant to install this build, do that first (`make install`, or however \
                 this tree normally installs), then run `story daemon install` again — the two \
                 will agree. If these are two names for the same file, or you meant to register \
                 this exact binary on purpose, run `story daemon install --this-binary`.",
                running = running.display(),
                installed = installed.display(),
            ),
            Self::Unconfirmable { running } => write!(
                f,
                "refusing to register `{running}` as your login agent: $PATH names no `story` at \
                 all, so nothing here can confirm this binary is the one you actually run.\n\n\
                 This is what a first install looks like when the install directory is not on \
                 $PATH yet — `~/.local/bin` by default. Add it to $PATH, or open a new shell if \
                 you have just added it, and run `story daemon install` again; the check will \
                 then pass on its own.\n\n\
                 If this binary is deliberately the one to register — a build in a fixed \
                 location, on a machine with no `story` on $PATH by design — run \
                 `story daemon install --this-binary`.\n\n\
                 Nothing was written; no agent was loaded or replaced.",
                running = running.display(),
            ),
        }
    }
}

/// Decides whether this binary may be given a permanent seat, and under which
/// spelling.
///
/// The rows are a conjunction read in order, not a heuristic score.
///
/// Neither refusal names `story update`: an unreleased build has no release to
/// update *to*, and pointing at a command that reports up to date and changes
/// nothing is the dead end SH-405 records — repeating it here would ship it
/// twice.
///
/// # Errors
///
/// [`Refusal`], carrying the whole message.
pub fn decide(inputs: &Inputs) -> Result<Verdict, Refusal> {
    // Ahead of everything, and deliberately not overridable: under `sudo`,
    // `env_reset` means the `$PATH` below belongs to root rather than to the
    // operator, so the comparison would answer about the wrong user with the
    // confident shape of a right answer. `--this-binary` answers *which
    // binary*, never *which user*.
    if inputs.uid == 0 {
        return Err(Refusal::Root);
    }
    // The operator naming this exact binary is the whole request, so the plist
    // records the spelling they invoked rather than one resolved for them.
    if inputs.this_binary {
        return Ok(Verdict {
            enthrone: inputs.running.spelling.clone(),
        });
    }
    let Some(installed) = &inputs.installed_story else {
        return Err(Refusal::Unconfirmable {
            running: inputs.running.spelling.clone(),
        });
    };
    if installed.canonical != inputs.running.canonical {
        return Err(Refusal::Disagrees {
            running: inputs.running.spelling.clone(),
            installed: installed.spelling.clone(),
        });
    }
    // Permitted by agreement, so the two paths are the same file and the plist
    // may have either. It takes the `$PATH` spelling, because that is the one
    // proven stable across an upgrade: `~/.local/bin/story` survives where
    // `~/.local/share/storyhook/versions/2.1.1/story` — which an operator can
    // legitimately have invoked directly to get here — does not. A plist
    // outlives the build it names (SH-239).
    Ok(Verdict {
        enthrone: installed.spelling.clone(),
    })
}

/// Resolves the real inputs [`decide`] needs, for the one production caller.
///
/// The only impure step; everything past it is a pure function of these values.
#[must_use]
pub fn gather(uid: u32, this_binary: bool, running: InstalledStory) -> Inputs {
    Inputs {
        uid,
        running,
        installed_story: crate::path_identity::installed_story(),
        this_binary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(spelling: &str, canonical: &str) -> InstalledStory {
        InstalledStory {
            spelling: PathBuf::from(spelling),
            canonical: PathBuf::from(canonical),
        }
    }

    fn base_inputs() -> Inputs {
        Inputs {
            uid: 501,
            running: story(
                "/home/dev/repo/target/debug/story",
                "/home/dev/repo/target/debug/story",
            ),
            installed_story: Some(story(
                "/home/dev/.local/bin/story",
                "/home/dev/.local/bin/story",
            )),
            this_binary: false,
        }
    }

    /// SH-411's own shape: a worktree's debug binary asking for a permanent
    /// seat while `$PATH` runs something else.
    #[test]
    fn refuses_a_binary_that_is_not_the_story_on_path() {
        let refusal = decide(&base_inputs()).expect_err("must refuse");
        assert_eq!(
            refusal,
            Refusal::Disagrees {
                running: PathBuf::from("/home/dev/repo/target/debug/story"),
                installed: PathBuf::from("/home/dev/.local/bin/story"),
            }
        );
        let said = refusal.to_string();
        assert!(said.contains("/home/dev/repo/target/debug/story"), "{said}");
        assert!(said.contains("/home/dev/.local/bin/story"), "{said}");
        assert!(said.contains("--this-binary"), "{said}");
        assert!(
            said.contains("Nothing was written"),
            "a refusal must say the machine was left alone: {said}"
        );
        assert!(
            !said.contains("story update"),
            "must not repeat SH-405's dead end: {said}"
        );
    }

    /// The control. Without it every refusal test above passes equally well for
    /// a gate that refuses everything.
    #[test]
    fn permits_the_installed_story_registering_its_own_agent() {
        let mut inputs = base_inputs();
        inputs.running = story("/home/dev/.local/bin/story", "/home/dev/.local/bin/story");
        assert_eq!(
            decide(&inputs),
            Ok(Verdict {
                enthrone: PathBuf::from("/home/dev/.local/bin/story")
            })
        );
    }

    /// The deliberate asymmetry with [`crate::migration_guard`], which
    /// *permits* on this same condition. Stated as its own test so a later
    /// edit that "makes the two consistent" has to delete an assertion that
    /// says why they are not.
    #[test]
    fn refuses_when_path_names_no_story_at_all() {
        let mut inputs = base_inputs();
        inputs.installed_story = None;
        let refusal = decide(&inputs).expect_err("must refuse");
        assert_eq!(
            refusal,
            Refusal::Unconfirmable {
                running: PathBuf::from("/home/dev/repo/target/debug/story"),
            }
        );
        let said = refusal.to_string();
        assert!(
            said.contains("~/.local/bin"),
            "the message must name the first-install case it will most often meet: {said}"
        );
        assert!(said.contains("--this-binary"), "{said}");
        assert!(said.contains("Nothing was written"), "{said}");
    }

    /// The two refusals make different claims, so they must not be one
    /// message. This is what the enum buys over a `String`.
    #[test]
    fn the_two_refusals_are_distinguishable() {
        let mut unconfirmable = base_inputs();
        unconfirmable.installed_story = None;
        let a = decide(&base_inputs()).expect_err("disagrees");
        let b = decide(&unconfirmable).expect_err("unconfirmable");
        assert_ne!(a, b);
        assert_ne!(a.to_string(), b.to_string());
    }

    #[test]
    fn the_flag_permits_a_disagreement_and_enthrones_this_binary() {
        let mut inputs = base_inputs();
        inputs.this_binary = true;
        assert_eq!(
            decide(&inputs),
            Ok(Verdict {
                enthrone: PathBuf::from("/home/dev/repo/target/debug/story")
            }),
            "naming this exact binary is the whole request, so the plist records what was \
             invoked rather than a path resolved on the operator's behalf"
        );
    }

    #[test]
    fn the_flag_permits_an_unconfirmable_path() {
        let mut inputs = base_inputs();
        inputs.installed_story = None;
        inputs.this_binary = true;
        assert!(decide(&inputs).is_ok());
    }

    /// A symlinked `~/.local/bin/story` pointing at the build being run is
    /// *agreement*: the comparison canonicalizes, so the two names for one file
    /// are equal. What goes into the plist is the `$PATH` spelling, which is
    /// the one that survives the next upgrade (SH-239) — the version-pinned
    /// real path does not.
    #[test]
    fn agreement_enthrones_the_path_spelling_not_the_version_pinned_realpath() {
        let inputs = Inputs {
            uid: 501,
            running: story(
                "/home/dev/.local/share/storyhook/versions/2.1.1/story",
                "/home/dev/.local/share/storyhook/versions/2.1.1/story",
            ),
            installed_story: Some(story(
                "/home/dev/.local/bin/story",
                "/home/dev/.local/share/storyhook/versions/2.1.1/story",
            )),
            this_binary: false,
        };
        assert_eq!(
            decide(&inputs),
            Ok(Verdict {
                enthrone: PathBuf::from("/home/dev/.local/bin/story")
            }),
            "a plist outlives the build it names, so it must record the durable spelling"
        );
    }

    /// Root is refused ahead of the comparison, and `--this-binary` does not
    /// reach it: the flag answers which binary, never which user.
    #[test]
    fn refuses_root_ahead_of_everything_and_the_flag_does_not_override_it() {
        let mut inputs = base_inputs();
        inputs.uid = 0;
        inputs.running = story("/home/dev/.local/bin/story", "/home/dev/.local/bin/story");
        assert_eq!(decide(&inputs), Err(Refusal::Root));

        inputs.this_binary = true;
        assert_eq!(
            decide(&inputs),
            Err(Refusal::Root),
            "`--this-binary` answers which binary, never which user"
        );
        let said = Refusal::Root.to_string();
        assert!(said.contains("sudo"), "{said}");
        assert!(said.contains("Nothing was written"), "{said}");
    }
}
