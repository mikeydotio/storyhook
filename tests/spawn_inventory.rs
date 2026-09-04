//! Every place storyhook starts a process, classified by how a descendant of
//! it could hang the caller.
//!
//! # The class this pins, and why a grep for the *shape* cannot pin it
//!
//! SH-94 was one instance of a general defect: **a lifetime mismatch between a
//! descriptor's holder and its owner.** Any process storyhook causes to exist
//! can outlive the command that caused it, and it holds every descriptor it
//! inherited for its whole life — so a caller reading a pipe waits for an
//! end-of-file that a stranger is now responsible for delivering.
//!
//! The members of that class share no syntactic shape. The captured instance
//! was a *detached* spawn. `src/event_hooks.rs` is a *waited* spawn whose
//! grandchild is the leaker, so a rule keyed on "detached" would miss it
//! entirely. A rule keyed on `Command::new` would catch every site here, most
//! of which are fine, and a lint with that false-positive rate is allowed away
//! within two commits.
//!
//! So this test states no rule about how a child may be spawned. It pins the
//! **set** of sites and their classifications, in the idiom
//! `storyhook_test_support::env`'s `isolation_covers_every_variable_…` already
//! uses: by name, never by count. Adding a way to start a process cannot happen
//! without somebody writing down which of the three kinds it is — and that
//! classification is the step that was skipped when the daemon spawn was
//! written.
//!
//! # The three kinds
//!
//! The distinction is **whether the caller reads a pipe**, because that is what
//! decides whether a descendant can block it, and it is visible in the code
//! rather than a judgement about the program being run.
//!
//! * [`Kind::Waited`] — the caller runs the child to completion without reading
//!   a **pipe** from it (`.status()`, or `spawn` then `wait`). A descendant that
//!   inherits a descriptor cannot block the caller, because the caller is not
//!   waiting on one. The word *pipe* is load-bearing: a caller that reads a
//!   regular file the child wrote is `Waited`, because a file has no
//!   end-of-file rendezvous and no descendant can make the read wait.
//! * [`Kind::Reads`] — the caller reads the child's output to end-of-file
//!   (`.output()`, or an explicit read of a piped stream). A descendant that
//!   inherits that pipe and outlives the child holds the caller for as long as
//!   it lives. `src/daemon/tailnet.rs` is the hardened example: its child gets
//!   its own process group and the whole group is killed on timeout.
//! * [`Kind::Detached`] — the child is meant to outlive the caller. It must
//!   inherit nothing beyond the stdio it is given, which
//!   `daemon::lifecycle::spawn_child` enforces and `tests/daemon_fd_hygiene.rs`
//!   proves.
//!
//! A `Reads` site is not a defect. It is a site where the question has to be
//! asked, and `src/event_hooks.rs` was the one where the answer was "nothing
//! stops it" — filed here rather than fixed, because it had no captured
//! reproduction and this repository does not fix what it has not reproduced.
//!
//! It has one now, and SH-141 closed it, which is why that row reads `Waited`.
//! What the reproduction found is worth carrying: the wedge did **not** need a
//! descendant. `fire_hook` drove three concurrent pipes sequentially — write
//! stdin, then wait, then read stderr — so a merely *verbose* hook deadlocked
//! against storyhook with nothing inherited by anyone. The lifetime mismatch
//! this file is named for was one instance of that, not the whole of it.
//!
//! The remedy generalizes, and it is why the failure message below names two:
//! a process group bounds *who you can kill*, but giving the child a file
//! instead of a pipe removes the wait entirely, and only the second works when
//! what the child leaves behind is not yours to kill.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How a descendant of this site could hang its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Run to completion, no pipe read. A descendant cannot block the caller.
    Waited,
    /// Output read to end-of-file. A descendant holding that pipe blocks the
    /// caller for as long as it lives.
    Reads,
    /// Meant to outlive the caller, so it must inherit nothing.
    Detached,
}

/// Every `Command::new(…)` in `src/`, by the file it is in and the expression
/// naming the program, with its classification.
///
/// Keyed on the pair rather than on a line number, because line numbers churn
/// on every edit and would make this a nuisance rather than a gate. A second
/// `Command::new("git")` in a file that already has one does not trip it — the
/// classification for "git, run from this module" has not changed. A new
/// program, or a new file, does.
const INVENTORY: &[(&str, &str, Kind)] = &[
    ("src/daemon/commands.rs", "\"launchctl\"", Kind::Reads),
    ("src/daemon/lifecycle.rs", "exe", Kind::Detached),
    ("src/daemon/tailnet.rs", "\"tailscale\"", Kind::Reads),
    // Centralized verification waits for bash orchestration commands and
    // reads their JSON results. The main gate is now bounded and file-backed;
    // notify/reap still use output pipes until SH-547's adopted sibling fix.
    ("src/daemon/verification.rs", "\"bash\"", Kind::Reads),
    // `env::git_env::command` — the one place in `src/` that constructs a
    // `git`. Classified with the reads it replaced: every caller uses
    // `.output()`, which reads the child's stdout to EOF. `git` reads files,
    // spawns nothing of its own and touches no network, so the
    // descendant-holds-the-pipe hazard this column exists for has nothing to
    // attach to.
    ("src/env/git_env.rs", "\"git\"", Kind::Reads),
    // `event_hooks::fire_hook` — a user's shell command. `Waited` since SH-141:
    // it spawns, waits, and reads a *file*. The hook is handed unlinked
    // temporary files rather than pipes, so there is no pipe for a descendant to
    // hold and nothing for the caller to wait on.
    ("src/event_hooks.rs", "\"sh\"", Kind::Waited),
    // `service::engine` owns both process doors behind the Dispatcher seam:
    // the bash helper and bounded tmux probes/kills. Both are `Waited`:
    // stdout/stderr are unlinked files rather than pipes, `wait_timeout`
    // bounds the child, and a timeout kills the process group before reading.
    // A descendant therefore has no EOF rendezvous with the caller and no
    // unbounded process lifetime to inherit.
    ("src/service/engine.rs", "\"bash\"", Kind::Waited),
    ("src/service/engine.rs", "&self.tmux_program", Kind::Waited),
    // `install_status::installed_binary` — the `story` on this machine's own
    // `$PATH`, asked for its version so the report can say whether the build
    // answering you is the build this machine runs (SH-530). `Reads`, because
    // `.output()` takes both streams to EOF.
    //
    // The descendant-holds-the-pipe hazard this column exists for has nothing
    // to attach to: `--version` is `Invocation::Version`, which
    // `invoke::needs_no_store` answers locally, so it starts no daemon, opens
    // no store and spawns nothing of its own. That is a property of the verb
    // rather than of this call site, which is why it is written down here
    // rather than assumed — a future `--version` that consulted the daemon
    // would move this row to `Detached`'s problem, not leave it unchanged.
    ("src/install_status.rs", "&spelling", Kind::Reads),
    // `plugin::run_provider` — the selected provider CLI (`claude` or `codex`).
    // Classified as `Reads` because install/uninstall captures both streams to
    // report the provider's exact failure. The availability probe sharing this
    // constructor uses null streams and `.status()`, so the more restrictive
    // classification covers every call made through it.
    ("src/plugin.rs", "target.executable(", Kind::Reads),
    // `plugin::run_helper` — the Codex stable-launcher bridge. The helper
    // inherits the caller's streams and is waited to completion with
    // `.status()`, so there is no output pipe whose EOF a descendant could
    // withhold.
    ("src/plugin.rs", "\"bash\"", Kind::Waited),
    ("src/tui/app.rs", "&editor_cmd", Kind::Waited),
    ("src/update.rs", "\"tar\"", Kind::Waited),
    ("src/update.rs", "staged", Kind::Waited),
    // `clipboard::pipe_to_command` — `pbcopy`/`xclip`/`wl-copy`, or whatever
    // `$STORYHOOK_CLIPBOARD_CMD` names. `Waited`: stdout and stderr are both
    // `Stdio::null()`, so there is no pipe for a descendant to hold, and the
    // only pipe at all is *stdin*, which this end closes. Lived at
    // `src/web.rs` until SH-250 moved it beside its second caller.
    ("src/clipboard.rs", "&argv[0]", Kind::Waited),
    // `claim_comment::tmux_window` — `tmux display-message -p`, asked once
    // per `story claim` so the default comment can name the caller's own
    // window (SH-476). `Reads`: stdout is a pipe. Hardened on
    // `daemon/tailnet.rs`'s model rather than `env/git_env.rs`'s, because
    // unlike `git` the program on the other end is a *client of a running
    // server*, and a wedged tmux server is exactly the case where an
    // unbounded read would hold a mutating command open forever. Own process
    // group, bounded wait, whole group killed on timeout, and the timeout is
    // reported rather than swallowed.
    ("src/claim_comment.rs", "\"tmux\"", Kind::Reads),
    // `web::open_in_browser` — `open`/`xdg-open`, or `$BROWSER`.
    // `Waited`: `.status()` with both output streams `Stdio::null()`, so no
    // pipe exists for a descendant to hold. A browser it launches routinely
    // outlives the command, which is why the null streams matter rather than
    // being tidiness.
    //
    // Until SH-250 this file's clipboard spawn shared this entry's key
    // (same path, same `&argv[0]` expression), so one row was standing for two
    // unrelated spawns. Moving the clipboard out separated them.
    ("src/web.rs", "&argv[0]", Kind::Waited),
];

/// Every `.rs` file under `src/`, as (path relative to the crate root, source).
///
/// Each file is truncated at its `#[cfg(test)]` module, because a unit test may
/// legitimately spawn anything and is not a way the *program* starts a process.
/// `daemon::lifecycle`'s own `disinheriting_still_reports_an_exec_that_failed`
/// spawns a deliberately missing binary, and it belongs in neither this
/// inventory nor a reviewer's attention.
fn production_sources(root: &Path, crate_root: &Path, into: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(root).expect("reading src/") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            production_sources(&path, crate_root, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path).expect("reading a source file");
            let production = match text.find("\n#[cfg(test)]\nmod tests {") {
                Some(at) => text[..at].to_string(),
                None => text,
            };
            let relative = path
                .strip_prefix(crate_root)
                .expect("a path under the crate root")
                .to_string_lossy()
                .replace('\\', "/");
            into.push((relative, production));
        }
    }
}

/// The expression each `Command::new(` in `source` is handed, verbatim.
fn programs(source: &str) -> Vec<String> {
    const OPENING: &str = "Command::new(";
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(OPENING) {
        rest = &rest[at + OPENING.len()..];
        // Every argument in this tree is a literal or a simple binding, so the
        // first `)` closes the call. A future argument containing one would
        // show up here as a truncated entry and fail the comparison, which is
        // the right outcome: it is a site nobody has classified.
        //
        // **This scans raw text, comments included**, so a doc comment that
        // spells the constructor's own token is read as a spawn site and fails
        // this test with a paragraph of prose where a program name should be.
        // SH-160 hit it while documenting `env::git_env`; the fix is to describe
        // the constructor without spelling it, which is cheaper than teaching
        // this function to parse Rust. The failure is loud and self-explaining,
        // which is why it stays a documented gotcha rather than a parser.
        if let Some(close) = rest.find(')') {
            found.push(rest[..close].trim().to_string());
        }
    }
    found
}

/// Adding a way to start a process must be accompanied by saying what kind it
/// is.
///
/// The failure message is the whole value of this test, so it names the site
/// and asks the question rather than printing two sets and leaving the reader
/// to diff them.
#[test]
fn every_way_storyhook_starts_a_process_is_classified() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    production_sources(&crate_root.join("src"), &crate_root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {} files — the walk is broken, and a broken \
         walk would report an empty inventory as a clean one",
        files.len()
    );

    let found: BTreeSet<(String, String)> = files
        .iter()
        .flat_map(|(path, source)| {
            programs(source)
                .into_iter()
                .map(move |program| (path.clone(), program))
        })
        .collect();
    let pinned: BTreeSet<(String, String)> = INVENTORY
        .iter()
        .map(|(path, program, _)| ((*path).to_string(), (*program).to_string()))
        .collect();

    let unclassified: Vec<_> = found.difference(&pinned).collect();
    assert!(
        unclassified.is_empty(),
        "a new way to start a process is not in tests/spawn_inventory.rs: {unclassified:?}\n\
         \n\
         Decide which kind it is and add it to INVENTORY.\n\
           Waited   — run to completion, no pipe read; a descendant cannot block you.\n\
           Reads    — you read its output to EOF; a descendant that inherits that pipe\n\
                      and outlives the child holds you for as long as it lives. Two\n\
                      remedies, and they are not interchangeable: give the child its own\n\
                      process group and kill the group (src/daemon/tailnet.rs), or give\n\
                      it files instead of pipes so there is no end-of-file to wait for\n\
                      (src/event_hooks.rs). Only the second works when what the child\n\
                      leaves behind is not yours to kill.\n\
           Detached — it outlives you, so it must inherit nothing: see\n\
                      daemon::lifecycle::spawn_child and tests/daemon_fd_hygiene.rs.\n\
         \n\
         This is not a rule about how to spawn. It is the classification step that\n\
         was skipped when the daemon spawn was written, and SH-94 is what that cost."
    );

    let gone: Vec<_> = pinned.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "tests/spawn_inventory.rs pins sites that no longer exist: {gone:?}\n\
         Delete them from INVENTORY — a pin nobody can reach reads as coverage."
    );
}

/// Exactly one site is detached, and it is the daemon.
///
/// Separate from the inventory comparison because it is a different claim: that
/// one is "nothing is unclassified", and this one is "the thing
/// `tests/daemon_fd_hygiene.rs` guards is still the only thing that needs
/// guarding". A second `Detached` entry means that file's single test no longer
/// covers the whole promise.
#[test]
fn only_the_daemon_outlives_the_command_that_starts_it() {
    let detached: Vec<_> = INVENTORY
        .iter()
        .filter(|(_, _, kind)| *kind == Kind::Detached)
        .map(|(path, program, _)| (*path, *program))
        .collect();
    assert_eq!(
        detached,
        vec![("src/daemon/lifecycle.rs", "exe")],
        "tests/daemon_fd_hygiene.rs proves one detached spawn inherits nothing. \
         A second one needs its own proof, or it ships unguarded."
    );
}
