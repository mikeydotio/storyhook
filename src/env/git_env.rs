//! The git environment storyhook refuses to inherit, and the one place a `git`
//! it runs is constructed (SH-160).
//!
//! # The defect this exists to end
//!
//! Since SH-114 the daemon is the process that runs every git command storyhook
//! runs, against a working directory carried in the request envelope. It also
//! **outlives** the client that started it and inherits that client's whole
//! environment for its whole life. So one `GIT_DIR` exported in one shell — by
//! whoever happened to run the first `story` command after a restart — decides
//! what every later command on the machine sees, in every repository, until
//! somebody restarts the daemon.
//!
//! Measured, with the real binary against an isolated store:
//!
//! | `commit-sync` run from | clean daemon | daemon started once with `GIT_DIR` |
//! |---|---|---|
//! | a repository | links its own `HEAD` | links a sha that does not exist in it, and moves the story |
//! | a directory that is **not a repository** | `error: not a git repository`, exit 2 | syncs the foreign repository's commits |
//!
//! The guard and the payload fail in the same direction, which is what made it
//! silent: every layer is internally consistent and reports success. The second
//! row is the sharper one — a project in a plain directory with no `.git`
//! anywhere above it acquired a commit link and a state transition from a
//! repository it has no relationship with, because `git rev-parse --git-dir`
//! answered for somewhere else and the refusal never fired.
//!
//! # Deny at the process, allow at the command — and the split is the design
//!
//! The two layers get opposite rules, because they protect different things.
//!
//! **The process** ([`scrub_this_process`]) can only be a denylist. Storyhook's
//! children are not all `git`: [`crate::event_hooks`] spawns a user's shell
//! command, and the daemon can also spawn a dispatch child and a `claude`
//! process. Those legitimately inherit the environment, so clearing it is not
//! available — the most that can be said is which names must not survive.
//!
//! **A `git` command** ([`command`]) can be, and therefore is, an allowlist:
//! cleared, then handed back exactly what it needs. That closes the class by
//! construction rather than by enumeration, which matters because git's
//! environment surface grows every release. The council that chose this had the
//! proof in front of it: the nine-name denylist inherited from SH-151 was caught
//! missing `GIT_SHALLOW_FILE`, `GIT_GRAFT_FILE`, `GIT_CONFIG_COUNT` and
//! `GIT_CONFIG_PARAMETERS` *during the vote*, and the amended twelve-name list
//! offered as its replacement still missed one of them. Two of three seats voted
//! against their own denylist proposals on that evidence.
//! `.council/sh160-git-env-scrub-home/DECISION.md` is the record.
//!
//! # What was measured about `GIT_CONFIG_*`, and why three names survive
//!
//! On git 2.50.1, against a repository whose own `remote.origin.url` is set:
//!
//! | environment | what `git config --get remote.origin.url` answered |
//! |---|---|
//! | `GIT_CONFIG_KEY_0` + `GIT_CONFIG_VALUE_0`, no `COUNT` | the repository's own — **inert** |
//! | the same, `GIT_CONFIG_COUNT=0` | the repository's own — **inert** |
//! | the same, `GIT_CONFIG_COUNT=1` | the injected value |
//! | `GIT_CONFIG_PARAMETERS` alone | the injected value |
//! | `GIT_CONFIG_GLOBAL` naming a file that sets it | the repository's own |
//!
//! Two consequences are written into the lists below. **`GIT_CONFIG_COUNT` is
//! the single gate git itself uses**, so removing that one name makes every
//! `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` pair inert however many were
//! exported — enumerating indices would be dead coverage chasing a bound that
//! does not exist, and it is the first thing a reader will otherwise think was
//! forgotten. And **`GIT_CONFIG_PARAMETERS` is a separate channel**, live with
//! no `COUNT` at all, so it is named in its own right.
//!
//! `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` are kept,
//! individually named and never as a `GIT_CONFIG*` wildcard: they are
//! file-based, lowest-precedence, and measured unable to override a value the
//! repository itself sets. Keeping them is a **named residual risk** rather than
//! an inert choice — they still supply anything the repository leaves unset,
//! such as `core.hooksPath` or `include.path`.
//!
//! The reason that used to sit beside this carve-out — that the test suite
//! isolates git config through `GIT_CONFIG_*` — was false, and is deleted rather
//! than moved. Fixture isolation comes from `HOME` and `XDG_CONFIG_HOME` in
//! `storyhook_test_support`'s `ISOLATED_VARS`; the whole tree contains
//! `GIT_CONFIG_*` in exactly one test file, set on a `git` that test spawns
//! itself and that storyhook's scrub could never reach. A carve-out justified by
//! a false reason is what the next contributor cites when widening it back.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Names removed from **this process's** environment at startup, because each
/// one redirects which repository git answers about.
///
/// Three families, and the grouping is the argument for membership:
///
/// 1. **Where the repository is** — the nine SH-151 established, which decide
///    which directory, object store or namespace git resolves against.
/// 2. **What its history is** — `git log`'s answer is commit-sync's idempotency
///    key *and* its state-transition trigger, so rewriting it forges both.
///    Measured: `GIT_SHALLOW_FILE` and `GIT_GRAFT_FILE` each cut a two-commit
///    log to one.
/// 3. **What its configuration is** — the highest-precedence config sources
///    git has, able to replace `remote.origin.url` outright.
///
/// Deliberately **not** a general-purpose list of git's environment variables.
/// `GIT_EXEC_PATH`, `GIT_SSH_COMMAND`, `GIT_EXTERNAL_DIFF`, `GIT_TRACE*` and the
/// rest change how git *behaves* rather than which repository it answers about;
/// the allowlist in [`command`] excludes every one of them by construction,
/// without anybody having to keep them written down here. This list exists only
/// for the layer where an allowlist is unavailable.
const REDIRECTS_GIT: [&str; 16] = [
    // 1. where the repository is
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_NAMESPACE",
    // 2. what its history is
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
    "GIT_REPLACE_REF_BASE",
    "GIT_NO_REPLACE_OBJECTS",
    // 3. what its configuration is. `GIT_CONFIG_COUNT` carries the whole
    //    indexed family with it — see this module's header for the measurement.
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

/// Everything a `git` storyhook runs is allowed to see.
///
/// Short because the job is narrow: storyhook asks git to read a repository it
/// names by working directory. `PATH` is how the child is found at all; `HOME`
/// and `XDG_CONFIG_HOME` are where the user's own git config lives, and are what
/// the test harness overrides to isolate a fixture; `TMPDIR`, `TZ`, `LANG` and
/// `LC_ALL` keep the user's locale and scratch directory rather than silently
/// imposing the daemon's.
///
/// The three `GIT_CONFIG_*` names are the deliberate, measured carve-out this
/// module's header explains.
const GIT_MAY_SEE: [&str; 10] = [
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "TMPDIR",
    "TZ",
    "LANG",
    "LC_ALL",
];

/// Never inherited, always set: a `git` that reached a real remote would
/// otherwise block on a credential prompt for as long as the daemon lives.
const NO_TERMINAL_PROMPT: (&str, &str) = ("GIT_TERMINAL_PROMPT", "0");

/// Removes every name in [`REDIRECTS_GIT`] from this process's environment.
///
/// **Called as the first statement of `main`, unconditionally** — for every
/// invocation rather than only the daemon's. Three things follow from that
/// placement, and all three are the point:
///
/// 1. **It covers every way a daemon comes to exist.** Auto-spawn and
///    `story daemon start` both exec a `--serve` process that runs this; so does
///    the launchd agent, which inherits the launchd user session rather than any
///    shell; so does a human or a service manager running `story daemon --serve`
///    by hand, which is what `story daemon install` recommends off macOS.
///    Scrubbing at the spawn instead would have covered only the first of those.
/// 2. **It cleans what the daemon hands on.** A daemon holding `GIT_DIR` passes
///    it to every user event hook it fires and to every other child it starts.
///    No funnel inside `src/` can reach a user's hook script; only this can, and
///    that is the property `tests/daemon_git_env.rs`'s hook row exists to pin.
/// 3. **It cleans the client too**, which still matters while `story tui` opens
///    the store in-process rather than through the daemon.
///
/// # Safety
///
/// [`std::env::remove_var`] is unsound in the presence of another thread reading
/// the environment. This is called as the first statement of `main`, before this
/// program has started any thread — the same argument, at the same point, that
/// `main`'s existing `publish_store_path` already relies on. Moving the call
/// behind a thread, or into a library path a caller reaches after spawning one,
/// makes it unsound with no compiler diagnostic and no test failure. It is
/// deliberately not re-exported above [`crate::env`].
pub fn scrub_this_process() {
    for name in REDIRECTS_GIT {
        // SAFETY: single-threaded, as the doc comment above establishes.
        unsafe { std::env::remove_var(name) };
    }
}

/// A `git` to run in `cwd`, carrying only [`GIT_MAY_SEE`].
///
/// **The only place in `src/` that constructs a `git` command.** Pinned by
/// `tests/spawn_inventory.rs`, which enumerates every process spawn in the tree
/// by the file holding it, so a second one anywhere else fails that test with
/// the file named. (That test greps for the constructor's literal token, so
/// this sentence deliberately does not spell it — see its own header.)
///
/// Cleared and rebuilt rather than filtered, so a variable nobody has heard of
/// yet is excluded by default. That is the difference the council turned on: a
/// denylist is only as complete as the last person to read git's release notes,
/// and this repository watched one go stale twice in a single afternoon.
pub fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    apply_allowlist(&mut command);
    command
}

/// Clears `command`'s environment and gives back exactly [`GIT_MAY_SEE`].
///
/// Split out of [`command`] for one reason, and it is a testing reason worth
/// stating rather than hiding: **`Command::get_envs` reports only what was
/// explicitly set, never what is inherited**, so a test that inspects it cannot
/// tell `env_clear` from no `env_clear` and would pass either way. The first
/// draft of this module shipped exactly that test, and deleting the
/// `env_clear()` line left it green — the gap was found by disarming the fix and
/// watching nothing fail. A test can only pin this by reading a real child's
/// real environment, which is what `the_allowlist_is_the_childs_whole_environment`
/// does through this function.
fn apply_allowlist(command: &mut Command) {
    command.env_clear();
    for name in GIT_MAY_SEE {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env(NO_TERMINAL_PROMPT.0, NO_TERMINAL_PROMPT.1);
}

/// The names a `git` may carry, for a test that asserts the set exactly rather
/// than writing it down a second time.
///
/// Exposed so the pin is **complete by construction**: a test comparing against
/// this cannot drift from what [`command`] does, because both read one array.
#[must_use]
pub fn allowlist() -> Vec<OsString> {
    GIT_MAY_SEE
        .iter()
        .map(OsString::from)
        .chain(std::iter::once(OsString::from(NO_TERMINAL_PROMPT.0)))
        .collect()
}

/// The names [`scrub_this_process`] removes, for the same reason.
#[must_use]
pub fn scrubbed() -> Vec<OsString> {
    REDIRECTS_GIT.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The allowlist is the child's whole environment** — asserted against a
    /// real child, not against the `Command` that describes it.
    ///
    /// The distinction is the whole test. `Command::get_envs` reports the
    /// *modifications* a caller has made and says nothing about what is
    /// inherited, so a test written against it passes identically whether or not
    /// `env_clear` was ever called. This module shipped that test first; deleting
    /// the `env_clear()` line left it green, which is how the gap was found — by
    /// disarming the fix and watching nothing fail.
    ///
    /// `env` rather than `git` because git has no way to print its environment,
    /// and the property under test is about the environment rather than about
    /// git. It is built through [`apply_allowlist`], the same function
    /// [`command`] uses, so the two cannot drift.
    #[test]
    fn the_allowlist_is_the_childs_whole_environment() {
        // A name in none of the lists, and one git genuinely reads. If it
        // survives into the child, the environment was inherited rather than
        // rebuilt — and no denylist would ever have caught it, which is the
        // argument for an allowlist in one variable.
        let smuggled = "GIT_AUTHOR_NAME";
        assert!(!GIT_MAY_SEE.contains(&smuggled) && !REDIRECTS_GIT.contains(&smuggled));

        let mut probe = Command::new("/usr/bin/env");
        apply_allowlist(&mut probe);
        // Set *after* the allowlist, exactly as a poisoned parent's environment
        // would already hold it, so this proves `env_clear` rather than ordering.
        probe.env(smuggled, "smuggled-through");

        let seen = probe.output().expect("running /usr/bin/env");
        assert!(seen.status.success(), "the probe child must run");
        let names: Vec<String> = String::from_utf8_lossy(&seen.stdout)
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
            .collect();

        let permitted = allowlist();
        for name in &names {
            assert!(
                permitted.iter().any(|allowed| allowed == name.as_str()) || name == smuggled,
                "`{name}` reached the child but is not on the allowlist; the environment was \
                 inherited rather than rebuilt"
            );
        }
        assert!(
            names.iter().any(|name| name == smuggled),
            "the probe is broken: its own marker never arrived, so this test proves nothing"
        );
    }

    /// The credential-prompt guard is set on every git, not merely permitted.
    ///
    /// A prompt in a daemon blocks for as long as the daemon lives, with no
    /// terminal for anybody to answer it at — the worst failure a background
    /// process can have.
    #[test]
    fn every_git_is_told_not_to_prompt() {
        let built = command(Path::new("."));
        let set = built
            .get_envs()
            .find(|(name, _)| *name == NO_TERMINAL_PROMPT.0)
            .and_then(|(_, value)| value);
        assert_eq!(
            set.map(|v| v.to_string_lossy().to_string()).as_deref(),
            Some(NO_TERMINAL_PROMPT.1),
            "GIT_TERMINAL_PROMPT must always be set to 0"
        );
    }

    /// Nothing is both scrubbed from the process and handed to git.
    ///
    /// A name on both lists would be removed at startup and then looked up again
    /// out of a process that no longer has it — harmless today only because the
    /// sets are disjoint, and quietly self-contradictory the moment they are not.
    #[test]
    fn nothing_is_both_scrubbed_and_allowed() {
        for allowed in GIT_MAY_SEE {
            assert!(
                !REDIRECTS_GIT.contains(&allowed),
                "`{allowed}` is on both lists, which cannot be what anyone meant"
            );
        }
    }

    /// The `GIT_CONFIG_*` boundary, as measured rather than as remembered.
    ///
    /// `GIT_CONFIG_COUNT` covers `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`
    /// without naming them: git reads a pair only for an index below `COUNT`, so
    /// with `COUNT` gone every pair is inert however many were exported. That is
    /// why no prefix sweep appears in either list, and it is the question a
    /// future reader will otherwise answer by adding one.
    #[test]
    fn the_git_config_carve_out_is_exactly_three_names() {
        for kept in [
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
        ] {
            assert!(
                GIT_MAY_SEE.contains(&kept),
                "{kept} is file-based and lowest-precedence, and is deliberately kept"
            );
        }
        for scrubbed in ["GIT_CONFIG", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS"] {
            assert!(
                REDIRECTS_GIT.contains(&scrubbed),
                "{scrubbed} is a config-injection channel and must be scrubbed"
            );
        }
    }

    /// Every name the story was filed about is still covered.
    ///
    /// The allowlist makes the process list the only place a name can be
    /// forgotten, so the three families are named here explicitly rather than
    /// left to the reader to check by eye.
    #[test]
    fn the_process_list_covers_location_history_and_config() {
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_NAMESPACE",
            "GIT_SHALLOW_FILE",
            "GIT_GRAFT_FILE",
            "GIT_REPLACE_REF_BASE",
            "GIT_NO_REPLACE_OBJECTS",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
        ] {
            assert!(
                REDIRECTS_GIT.contains(&name),
                "`{name}` redirects which repository git answers about"
            );
        }
    }

    /// git runs, and answers about `cwd`, under the trimmed environment.
    ///
    /// The council accepted the allowlist only on this condition. An allowlist's
    /// characteristic failure is that git cannot exec at all — `PATH` is the
    /// obvious way to get that wrong, and on macOS `/usr/bin/git` is a shim that
    /// may consult variables of its own — and "two people checked it by hand
    /// once" is an anecdote, not a property. This makes it a permanent pin.
    #[test]
    fn git_still_execs_and_answers_about_the_directory_it_was_given() {
        // `scratch_dir` rather than `tempfile::tempdir`: `$TMPDIR` is
        // Spotlight-indexed on macOS, which `clippy.toml` disallows outright.
        let dir = storyhook_test_support::scratch_dir();
        let init = command(dir.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .expect("git must exec under the allowlist");
        assert!(
            init.status.success(),
            "git init failed under the trimmed environment: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        let top = command(dir.path())
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .expect("git must exec under the allowlist");
        assert!(
            top.status.success(),
            "git rev-parse failed under the trimmed environment: {}",
            String::from_utf8_lossy(&top.stderr)
        );
        let answered = String::from_utf8_lossy(&top.stdout).trim().to_string();
        let answered = Path::new(&answered)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&answered));
        let expected = dir
            .path()
            .canonicalize()
            .unwrap_or_else(|_| dir.path().to_path_buf());
        assert_eq!(answered, expected, "git answered about the wrong directory");
    }
}
