//! What a storyhook **test environment** is, stated once.
//!
//! Story data lives in one SQLite store per machine, served by one daemon per
//! store. "Which store am I talking to" is therefore global state, and every
//! test, harness script and hand-typed command on a machine is a potential
//! writer to a tracker somebody is actually using. Isolating a run means
//! setting a specific list of environment variables to specific things — and
//! before this module that list existed in seven hand-copied places which had
//! already drifted apart from each other.
//!
//! This is that list. It lives in the library rather than in
//! `storyhook-test-support` for the same reason `story help priority-rubric`
//! ships in the binary: a suite driving `story` from another repository needs
//! it, and it has to be readable from the tool rather than from storyhook's own
//! documentation. `story help test-environment` renders it.
//!
//! # The two dispositions that are easy to get wrong
//!
//! **`STORYHOOK_STORE_PATH` is given a value, never merely unset.** It outranks
//! `STORYHOOK_DATA_DIR`, so a developer with one exported — exactly the person
//! debugging a second store — would otherwise send an entire suite into it
//! while every guard inspected the variable that lost. Setting it makes what
//! the child sees asserted rather than assumed.
//!
//! **`HOME` may not be redirected by a wrapper around `cargo` or `npm`.** See
//! [`Scope`]. This is the distinction that a flat list could not express, and
//! it is why the shell harnesses looked "incomplete" for as long as they have.
//!
//! # What is deliberately not here
//!
//! A parameter earns a place by answering one question: *what of the
//! developer's own does a storyhook process reach when this is left alone?* Two
//! things a harness also sets are not answers to it, and folding them in would
//! make this table mean two things at once:
//!
//! - `INSTA_UPDATE`, a snapshot-tool setting, which says nothing about which
//!   store a process reaches.
//! - `STORYHOOK_GATE_PROGRESS` and `STORYHOOK_GATE_PROGRESS_PATH`, which a
//!   harness legitimately *sets* for itself and strips per-child with `env -u`.
//!   A table of things every harness must neutralize cannot also contain a
//!   thing a harness must set, and `tests/store_isolation.rs` already fences
//!   those two on their own terms.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What an isolating harness must do with one variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Point it at this path beneath the environment root.
    ///
    /// The tail is relative and always spelled with `/`, because it is rendered
    /// into shell as well as joined in Rust. An empty tail names the root
    /// itself.
    Root(&'static str),
    /// Pin it to a literal value.
    Literal(&'static str),
    /// The pid of the process doing the isolating.
    ///
    /// Not a constant, and not the harness's choice: the daemon polls this pid
    /// and exits when it dies, so it has to name a process whose death should
    /// take the daemon with it.
    OwnPid,
    /// Remove it. There is no harmless value for a credential or for a
    /// selection somebody else made.
    Clear,
}

/// Where a parameter may be applied.
///
/// The distinction is **who else reads this variable**, and it is load-bearing
/// rather than fussy. `CARGO_HOME` and `RUSTUP_HOME` are unset on an ordinary
/// machine, so a harness that exports a fake `HOME` around `cargo test` costs
/// cargo its registry and its build cache, and one that does it around the
/// browser suite costs playwright its downloaded browsers — both silently, both
/// as a large slowdown rather than an error.
///
/// `storyhook-test-support`'s `TestEnv` can redirect `HOME` regardless, because
/// it applies isolation to each `story` child rather than to its own process. A
/// shell wrapper around `cargo` cannot, and a harness that runs nothing but
/// `story` and `git` (the plugin suite) can. So the answer is a property of the
/// parameter *and* of the caller, which is why it is a field here and an
/// argument at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Safe to export across a whole harness process tree.
    Anywhere,
    /// Only ever applied to a storyhook process itself.
    StoryhookProcessOnly,
}

/// One parameter of a storyhook test environment.
#[derive(Debug, Clone, Copy)]
pub struct Parameter {
    /// The environment variable's name.
    pub name: &'static str,
    /// What an isolating harness does with it.
    pub disposition: Disposition,
    /// Where that may be done.
    pub scope: Scope,
    /// What of the developer's own a run reaches when this is left alone.
    ///
    /// Rendered verbatim into `story help test-environment`, so it is written
    /// for a stranger: it describes storyhook's own model and names no story
    /// ids, the same boundary `priority-rubric` keeps.
    pub reason: &'static str,
}

/// One parameter resolved against a concrete root.
///
/// `value: None` means the variable is **removed**, which is a different thing
/// from being set to the empty string — `crate::env::env_path` already treats an
/// empty value as absent for paths, but a credential set to `""` is still a
/// credential the child was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// The environment variable's name.
    pub name: &'static str,
    /// The value to set, or `None` to remove it.
    pub value: Option<OsString>,
}

/// Everything that must be true of a process before it is safe to point at a
/// throwaway store.
///
/// Ordered store-first, then daemon, then the removals, because that is the
/// order the reasons build on each other and `story help test-environment`
/// renders it in this order.
pub const TEST_ENVIRONMENT: &[Parameter] = &[
    Parameter {
        name: "HOME",
        disposition: Disposition::Root("home"),
        scope: Scope::StoryhookProcessOnly,
        reason: "the fallback beneath every path below, and the root of the \
                 login agents, crash reports and git configuration storyhook \
                 reads directly",
    },
    Parameter {
        name: "XDG_DATA_HOME",
        disposition: Disposition::Root("home/.local/share"),
        scope: Scope::Anywhere,
        reason: "the default store's parent directory",
    },
    Parameter {
        name: "XDG_CONFIG_HOME",
        disposition: Disposition::Root("home/.config"),
        scope: Scope::Anywhere,
        reason: "configuration a fixture would otherwise read from, and write \
                 to, the real one",
    },
    Parameter {
        name: "XDG_STATE_HOME",
        disposition: Disposition::Root("home/.local/state"),
        scope: Scope::Anywhere,
        reason: "every daemon's portfile, pidfile, log and backups; not covered \
                 by STORYHOOK_DATA_DIR, which names only the store",
    },
    Parameter {
        name: "STORYHOOK_DATA_DIR",
        disposition: Disposition::Root("home/.local/share/storyhook"),
        scope: Scope::Anywhere,
        reason: "the directory the store file sits in",
    },
    Parameter {
        name: "STORYHOOK_STORE_PATH",
        disposition: Disposition::Root("home/.local/share/storyhook/store.db"),
        scope: Scope::Anywhere,
        reason: "the store file itself, and it OUTRANKS STORYHOOK_DATA_DIR — an \
                 exported one sends the whole run into that store while a guard \
                 inspecting the data directory sees nothing wrong",
    },
    Parameter {
        name: "STORYHOOK_DAEMON_ADDR",
        disposition: Disposition::Literal("127.0.0.1:0"),
        scope: Scope::Anywhere,
        reason: "a kernel-assigned port instead of 3456, which is where a \
                 developer's own dashboard lives",
    },
    Parameter {
        name: "STORYHOOK_PARENT_PID",
        disposition: Disposition::OwnPid,
        scope: Scope::Anywhere,
        reason: "the pid a spawned daemon dies with; without it a daemon \
                 outlives the run that made it and poisons every later one",
    },
    Parameter {
        name: "STORYHOOK_GITHUB_TOKEN",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "a real credential, which a fixture could spend against the \
                 real GitHub API; there is no harmless value, so it is removed",
    },
    Parameter {
        name: "STORYHOOK_PROJECT",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "a project selection made outside the run, which would steer \
                 every command at a project the run never named",
    },
    Parameter {
        name: "STORYHOOK_ACTOR",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "the identity writes are attributed to; inherited, a fixture's \
                 writes are recorded as somebody's",
    },
    Parameter {
        name: "STORYHOOK_ALLOW_TEMP_PROJECT",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "an override that disarms the throwaway-project guard, so a \
                 run that inherits it cannot observe the refusal it may be \
                 testing",
    },
    Parameter {
        name: "STORYHOOK_ALLOW_PROJECT_BURST",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "the same, for the burst guard",
    },
    Parameter {
        name: "STORYHOOK_ALLOW_UNINSTALLED_MIGRATION",
        disposition: Disposition::Clear,
        scope: Scope::Anywhere,
        reason: "the same, for the guard that stops an uninstalled build \
                 advancing the default store's schema",
    },
    Parameter {
        name: "STORYHOOK_VERIFIER_MIRROR",
        disposition: Disposition::Literal("0"),
        scope: Scope::Anywhere,
        reason: "the developer's own tmux server, on which the centralized \
                 verifier's live-observability mirror otherwise creates a \
                 fixed session and window and leaves a follower process \
                 running",
    },
];

impl Parameter {
    /// This parameter's value in an environment rooted at `root`, for a run
    /// owned by `pid`. `None` means the variable is removed.
    #[must_use]
    pub fn value(&self, root: &Path, pid: u32) -> Option<OsString> {
        match self.disposition {
            Disposition::Root(tail) => {
                let mut path = PathBuf::from(root);
                for segment in tail.split('/').filter(|s| !s.is_empty()) {
                    path.push(segment);
                }
                Some(path.into_os_string())
            }
            Disposition::Literal(value) => Some(OsString::from(value)),
            Disposition::OwnPid => Some(OsString::from(pid.to_string())),
            Disposition::Clear => None,
        }
    }

    /// Whether a harness of `scope` may apply this parameter.
    ///
    /// [`Scope::Anywhere`] parameters are applied by everyone; a
    /// [`Scope::StoryhookProcessOnly`] one only by a caller that says it is
    /// isolating a storyhook process rather than a whole process tree.
    #[must_use]
    pub const fn applies_in(&self, scope: Scope) -> bool {
        match self.scope {
            Scope::Anywhere => true,
            Scope::StoryhookProcessOnly => matches!(scope, Scope::StoryhookProcessOnly),
        }
    }
}

/// The whole parameter set, resolved against `root` for a run owned by `pid`.
///
/// `scope` says what the caller is isolating: [`Scope::Anywhere`] for a wrapper
/// around other tools, [`Scope::StoryhookProcessOnly`] for a caller that
/// applies these to a `story` process itself and may therefore also redirect
/// `HOME`.
///
/// The order is [`TEST_ENVIRONMENT`]'s own, and callers depend on it: the
/// equality test between this and the shell rendering compares sequences, so a
/// reordering here is a change both sides have to make.
#[must_use]
pub fn resolve(root: &Path, pid: u32, scope: Scope) -> Vec<Setting> {
    TEST_ENVIRONMENT
        .iter()
        .filter(|parameter| parameter.applies_in(scope))
        .map(|parameter| Setting {
            name: parameter.name,
            value: parameter.value(root, pid),
        })
        .collect()
}

/// The directories an environment rooted at `root` needs to exist before a
/// `story` process is pointed at it.
///
/// Derived from the [`Disposition::Root`] parameters rather than listed, so a
/// parameter added above cannot leave its directory uncreated. The store file
/// itself is excluded — a *file*'s parent is what has to exist, and the daemon
/// creates the file.
#[must_use]
pub fn directories(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = TEST_ENVIRONMENT
        .iter()
        .filter_map(|parameter| match parameter.disposition {
            Disposition::Root(tail) => {
                let value = parameter.value(root, 0)?;
                let path = PathBuf::from(value);
                // A parameter naming a file contributes its parent; every other
                // one names a directory. `store.db` is the only file today, and
                // the test below is what keeps that true rather than this
                // guess: the tail's last segment carrying an extension is the
                // signal, because a directory in this layout never has one.
                if Path::new(tail).extension().is_some() {
                    path.parent().map(Path::to_path_buf)
                } else {
                    Some(path)
                }
            }
            _ => None,
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// `story help test-environment`, rendered from [`TEST_ENVIRONMENT`].
///
/// Rendered rather than written out, because a hand-written copy of a table is
/// a second table: it agrees on the day it is written and drifts thereafter.
/// The prose around it is fixed; every parameter, disposition and reason comes
/// from the one definition above.
///
/// A [`std::sync::LazyLock<String>`] is what lets a computed topic sit in a map
/// of `&'static str`: the lock itself is `'static`, so `as_str()` borrows for
/// that long.
pub static HELP_TOPIC: std::sync::LazyLock<String> = std::sync::LazyLock::new(render_help_topic);

/// Wraps `text` into lines no wider than 78 columns once `indent` spaces are
/// prepended, and returns them already indented.
///
/// The bound is on the *finished* line, not on the text before indenting: a
/// wrap width that ignores its own indent produces a table nobody can read in
/// an 80-column terminal, which is what the first draft of this did.
fn wrap(text: &str, indent: usize) -> String {
    const TERMINAL_WIDTH: usize = 78;
    let width = TERMINAL_WIDTH.saturating_sub(indent).max(20);
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push_str(&pad);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&pad);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn render_help_topic() -> String {
    let mut body = String::from(
        r#"story help test-environment

How to run storyhook without touching the storyhook somebody is using.

Story data lives in one SQLite store per machine, and one daemon serves
one store. So "which store am I talking to" is machine-wide state: a
test suite, a script or a stray command that never names a store writes
into the real one, permanently, with no error and nothing to notice. One
run of one unisolated suite once put 394 junk projects into a real
tracker.

Isolating a run means giving a process the parameters below. Point them
all at one throwaway root directory and nothing that process does can
reach anything of yours.

== The parameters ==

"#,
    );

    for parameter in TEST_ENVIRONMENT {
        let disposition = match parameter.disposition {
            Disposition::Root("") => "<root>".to_string(),
            Disposition::Root(tail) => format!("<root>/{tail}"),
            Disposition::Literal(value) => value.to_string(),
            Disposition::OwnPid => "the isolating process's own pid".to_string(),
            Disposition::Clear => "REMOVE IT".to_string(),
        };
        body.push_str(&format!("  {} = {disposition}\n", parameter.name));
        body.push_str(&wrap(parameter.reason, 6));
        body.push('\n');
    }

    body.push_str(
        r#"== One of them is not like the others ==

HOME may only be redirected on a storyhook process itself, never
exported around a whole test run. Other tools read it too: a wrapper
that sets a fake HOME around a compiler, a package manager or a browser
harness costs each of them its caches, silently, as a slowdown rather
than an error. Set it on the command you are running storyhook with.
Every other parameter is safe to export for a whole run.

STORYHOOK_STORE_PATH outranks STORYHOOK_DATA_DIR. Give it a value rather
than unsetting it: an exported one in the shell you inherited sends the
whole run into that store, and a guard inspecting the data directory
sees nothing wrong.

== A shell that does it ==

  root=$(mktemp -d)                  # anywhere disposable
  export XDG_DATA_HOME="$root/home/.local/share"
  export XDG_CONFIG_HOME="$root/home/.config"
  export XDG_STATE_HOME="$root/home/.local/state"
  export STORYHOOK_DATA_DIR="$root/home/.local/share/storyhook"
  export STORYHOOK_STORE_PATH="$STORYHOOK_DATA_DIR/store.db"
  export STORYHOOK_DAEMON_ADDR=127.0.0.1:0
  export STORYHOOK_PARENT_PID=$$
  unset STORYHOOK_GITHUB_TOKEN STORYHOOK_PROJECT STORYHOOK_ACTOR
  unset STORYHOOK_ALLOW_TEMP_PROJECT STORYHOOK_ALLOW_PROJECT_BURST
  unset STORYHOOK_ALLOW_UNINSTALLED_MIGRATION
  mkdir -p "$STORYHOOK_DATA_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"

  story project new --prefix TST     # in the throwaway store, not yours

Then delete $root. Nothing outside it was written.

== What storyhook does on its own ==

These are backstops, not a substitute for the parameters above. Each
catches one shape of mistake, and none of them catches "pointed at the
wrong store on purpose".

  - A store that is not the machine default gets a kernel-assigned
    daemon port instead of 3456, so a second store can never take the
    port a dashboard is on.
  - Creating a project at a throwaway path is refused when the store is
    not itself throwaway.
  - Five or more projects created in a real store inside ten minutes is
    refused, on the grounds that the rate is a suite's and not a
    person's.
  - A build produced by `cargo test` refuses to guess a store at all.

Related:
  story help storage    — Where the store lives, and how it is named
  story store new       — Create an empty store to point a suite at
  story store backup    — Snapshot a store before something risky
"#,
    );
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parameter_is_named_once() {
        let mut names: Vec<&str> = TEST_ENVIRONMENT.iter().map(|p| p.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            before,
            "a parameter listed twice would be resolved twice, and the two \
             copies could disagree"
        );
    }

    /// Every reason is written for a stranger reading `story help
    /// test-environment`, so none of them may carry this project's own story
    /// ids — the boundary `priority-rubric` keeps for the same reason.
    #[test]
    fn no_reason_carries_a_story_id() {
        for parameter in TEST_ENVIRONMENT {
            assert!(
                !parameter.reason.contains("SH-"),
                "{}'s reason names a story id; `story scaffold` points strangers \
                 at this text and their trackers have no SH-numbers",
                parameter.name
            );
            assert!(
                !parameter.reason.is_empty(),
                "{} must say what it protects",
                parameter.name
            );
        }
    }

    /// The store-naming parameter that outranks the others must be **set**, not
    /// merely removable.
    ///
    /// Pinned by name because it is the one an author would most plausibly
    /// "simplify" into a `Clear`: unsetting it looks equivalent and is not —
    /// setting it is what makes the value a child sees assertable.
    #[test]
    fn the_store_path_is_given_a_value_rather_than_removed() {
        let parameter = TEST_ENVIRONMENT
            .iter()
            .find(|p| p.name == "STORYHOOK_STORE_PATH")
            .expect("STORYHOOK_STORE_PATH is a parameter");
        assert!(
            matches!(parameter.disposition, Disposition::Root(_)),
            "STORYHOOK_STORE_PATH outranks STORYHOOK_DATA_DIR, so it is given a \
             value inside the environment rather than unset"
        );
    }

    /// A `Clear` parameter that a harness could be tempted to redirect instead.
    #[test]
    fn a_credential_is_removed_and_never_redirected() {
        let parameter = TEST_ENVIRONMENT
            .iter()
            .find(|p| p.name == "STORYHOOK_GITHUB_TOKEN")
            .expect("STORYHOOK_GITHUB_TOKEN is a parameter");
        assert_eq!(parameter.disposition, Disposition::Clear);
    }

    #[test]
    fn home_is_the_only_parameter_a_wrapper_may_not_set() {
        let restricted: Vec<&str> = TEST_ENVIRONMENT
            .iter()
            .filter(|p| p.scope == Scope::StoryhookProcessOnly)
            .map(|p| p.name)
            .collect();
        assert_eq!(
            restricted,
            ["HOME"],
            "the restriction exists because other tools read the variable too. \
             Adding a name here means a harness stops setting it — say why in \
             the parameter's own reason, and update the shell rendering."
        );
    }

    #[test]
    fn resolving_gives_paths_under_the_root_and_removes_the_rest() {
        let root = Path::new("/private/tmp/fixture");
        let settings = resolve(root, 4321, Scope::StoryhookProcessOnly);

        for setting in &settings {
            let parameter = TEST_ENVIRONMENT
                .iter()
                .find(|p| p.name == setting.name)
                .expect("every setting names a parameter");
            match parameter.disposition {
                Disposition::Root(_) => {
                    let value = setting.value.as_ref().expect("a path");
                    assert!(
                        Path::new(value).starts_with(root),
                        "{} resolved to {value:?}, which is outside the environment",
                        setting.name
                    );
                }
                Disposition::Literal(expected) => {
                    assert_eq!(
                        setting.value.as_deref(),
                        Some(OsString::from(expected).as_os_str())
                    );
                }
                Disposition::OwnPid => {
                    assert_eq!(
                        setting.value.as_deref(),
                        Some(OsString::from("4321").as_os_str())
                    );
                }
                Disposition::Clear => assert_eq!(setting.value, None),
            }
        }
    }

    /// The narrower scope really is narrower, and by exactly one parameter.
    #[test]
    fn a_wrapper_resolves_every_parameter_except_home() {
        let root = Path::new("/private/tmp/fixture");
        let wrapper: Vec<&str> = resolve(root, 1, Scope::Anywhere)
            .into_iter()
            .map(|s| s.name)
            .collect();
        let full: Vec<&str> = resolve(root, 1, Scope::StoryhookProcessOnly)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(full.len(), TEST_ENVIRONMENT.len());
        assert!(!wrapper.contains(&"HOME"));
        assert_eq!(wrapper.len() + 1, full.len());
    }

    /// The store's parent is created; the store file is not.
    #[test]
    fn the_directories_cover_every_path_parameter_and_no_file() {
        let root = Path::new("/private/tmp/fixture");
        let dirs = directories(root);
        assert!(dirs.contains(&root.join("home/.local/share/storyhook")));
        assert!(dirs.contains(&root.join("home/.config")));
        assert!(
            !dirs.iter().any(|d| d.ends_with("store.db")),
            "the store is a file the daemon creates, not a directory to mkdir"
        );
        for dir in &dirs {
            assert!(dir.starts_with(root), "{dir:?} is outside the environment");
        }
    }

    /// Two roots never share a resolved path, which is what makes a per-test
    /// environment actually private.
    #[test]
    fn two_roots_share_nothing() {
        let a = resolve(Path::new("/private/tmp/a"), 1, Scope::StoryhookProcessOnly);
        let b = resolve(Path::new("/private/tmp/b"), 1, Scope::StoryhookProcessOnly);
        for (left, right) in a.iter().zip(b.iter()) {
            assert_eq!(left.name, right.name);
            match (&left.value, &right.value) {
                (Some(l), Some(r)) if l == r => {
                    // Only a literal or the pid may match across roots.
                    let parameter = TEST_ENVIRONMENT
                        .iter()
                        .find(|p| p.name == left.name)
                        .expect("a parameter");
                    assert!(
                        !matches!(parameter.disposition, Disposition::Root(_)),
                        "{} resolved identically under two different roots",
                        left.name
                    );
                }
                _ => {}
            }
        }
    }
}
