//! The launchd login agent: its plist, written **and** read.
//!
//! `story daemon install` registers a user agent that starts the daemon at
//! login, so the dashboard is there after a reboot without anybody running a
//! command. That plist is a durable, machine-wide artifact — it outlives the
//! shell that created it, every build it names, and most reboots — which is
//! what makes both of this module's halves necessary.
//!
//! # A login agent belongs to a store (SH-414)
//!
//! [`label`] and [`path`] are functions of the store, not machine-wide
//! constants: the default store keeps the bare [`LAUNCHD_LABEL`], every other
//! store gets that label plus its own key
//! ([`crate::env::StoreLocation::key`]) — the same digest that already names
//! the daemon state directory, the dashboard's per-store cookie, and the
//! keychain account. Before this, one label served every store, so installing
//! a login agent for a named store silently replaced whichever agent — the
//! default store's, or another named store's — happened to be at that one
//! label. [`serves_the_login_default`] is the predicate that decides
//! bare-vs-keyed, and it is deliberately **not**
//! [`crate::env::StoreLocation::is_default`] — see its own doc for why that
//! substitution would have left the defect half-open.
//!
//! # Why one module owns both directions
//!
//! SH-411 gave the plist a *reader* (`story daemon status` reports which binary
//! the agent runs, and whether that is still the one you use). A format with a
//! writer in one file and a hand-rolled reader in another is the SH-136 class
//! this project has already paid for four times: the two drift, agree with each
//! other about nothing, and no test is looking. They live together here, and
//! [`tests::the_reader_round_trips_the_writer`] is what keeps them honest.
//!
//! # Health answers two questions, not one
//!
//! [`judge`] decides [`Health::ServesAnotherStore`] — this label's plist
//! claims a different store than the one being asked about — **before**
//! [`Health::Missing`] — the registered binary no longer exists — which is in
//! turn decided **before** `$PATH` is consulted, and independently of it. Both
//! orderings are the point rather than an accident: a foreign agent's binary
//! is not this store's business to report on, and the machine SH-411 is about
//! is one where `$PATH` names no `story` at all, so a report that could only
//! compare against `$PATH` would have nothing to say on precisely the machine
//! that needs it.

use std::path::{Path, PathBuf};

use crate::env::{Environment, StoreLocation};
use crate::path_identity;

/// The launchd label for the user agent.
///
/// Reverse-DNS under the author's own domain, which is the convention every
/// other bundle identifier in this ecosystem follows. The **bare** form,
/// carried by the store [`serves_the_login_default`] reports true for — see
/// [`label`] for the store-keyed form every other store gets.
pub const LAUNCHD_LABEL: &str = "io.mikey.storyhook.daemon";

/// Whether a launchd-started `story daemon --serve` — no flags, none of this
/// process's environment — would open `env`'s store.
///
/// **Not** [`StoreLocation::is_default`]. That predicate is `self.path ==
/// self.default_path`, and `default_path` is computed from whatever
/// `$XDG_DATA_HOME` *this process* happens to have
/// (`StoreLocation::resolve`), so `XDG_DATA_HOME=/scratch story daemon
/// install`, run interactively, resolves a store at
/// `/scratch/storyhook/store.db` with `is_default()` **true** — and a launchd
/// child, which inherits none of this process's environment, would then open
/// `$HOME/.local/share/storyhook/store.db` instead: a different store than
/// the one just installed for. This is measured, not assumed — this project's
/// own CLAUDE.md doctrine — by [`tests::a_store_reached_via_xdg_data_home_
/// still_gets_its_own_label`], and it is a pre-existing gap in [`plist`]'s
/// `--store-path` arm this same predicate closes, not a new one this fix
/// introduces.
///
/// `StoreLocation::for_home` performs the identical resolution `resolve` does
/// with no XDG override — precisely what launchd hands the agent.
#[must_use]
fn serves_the_login_default(env: &Environment) -> bool {
    env.store_path() == StoreLocation::for_home(env.home()).path()
}

/// This store's launchd label: the bare [`LAUNCHD_LABEL`] for the store a
/// login agent would open with no flags, [`LAUNCHD_LABEL`] plus this store's
/// own key for any other — the same digest that already names the daemon
/// state directory, the dashboard's per-store cookie, and the keychain
/// account ([`StoreLocation::key`]).
///
/// No existing installation is touched: the login-time default's label is
/// unchanged, byte-for-byte, from every plist this project has ever written.
#[must_use]
pub fn label(env: &Environment) -> String {
    if serves_the_login_default(env) {
        LAUNCHD_LABEL.to_string()
    } else {
        format!("{LAUNCHD_LABEL}.{}", env.store().key())
    }
}

/// Where the launchd agent's plist goes.
pub fn path(env: &Environment) -> PathBuf {
    env.home()
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", label(env)))
}

/// The launchd agent definition.
///
/// `KeepAlive` is deliberately absent. The daemon is *supposed* to exit on a
/// version-skew restart, and an agent that resurrected it immediately would race
/// the client that just asked for a newer one. `RunAtLoad` is what this is for:
/// the dashboard is there after a reboot without anybody running a command.
pub fn plist(exe: &Path, env: &Environment) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
{store}        <string>daemon</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = label(env),
        exe = xml_escape(&exe.display().to_string()),
        // A login agent for the store a launchd child would open with no
        // flags needs no flag itself, and leaving it off keeps that store's
        // plist identical to the one every existing installation has. For any
        // other store the flag is required rather than tidy: the log path
        // below is already this store's, so an agent without it would run one
        // store's daemon while writing into another's directory. The same
        // predicate drives both this decision and `label`'s, so a plist can
        // never carry a bare label with a flag or a keyed label without one.
        store = if serves_the_login_default(env) {
            String::new()
        } else {
            format!(
                "        <string>--store-path</string>\n        <string>{}</string>\n",
                xml_escape(&env.store_path().display().to_string())
            )
        },
        log = xml_escape(&env.daemon_log().display().to_string()),
    )
}

/// Every `<string>` inside the `<array>` that follows
/// `<key>ProgramArguments</key>`, in document order, or `None` when this
/// parser does not recognise the document.
///
/// Deliberately narrow, same as the single-element reader it replaces: a
/// hand-edited plist, or one a future storyhook wrote in a shape this build
/// has never seen, reads as [`Health::Unreadable`] and is *reported* rather
/// than guessed at — the SH-312 rule that an unprovable outcome is reported as
/// unprovable, never as a confident wrong answer. Forgery is not the threat
/// model; anyone who can hand-write this file can also run `launchctl`
/// directly.
///
/// The one function both [`registered_exe`] and [`registered_store`] read
/// through, so the array is parsed once rather than twice — a second
/// hand-rolled scan of the same text is exactly the SH-136 class this module's
/// own doc already names.
#[must_use]
pub fn registered_args(text: &str) -> Option<Vec<String>> {
    let after_key = text.split_once("<key>ProgramArguments</key>")?.1;
    let array = after_key.split_once("<array>")?.1;
    // Bound the search to this array, so a malformed document cannot reach into
    // whatever element happens to follow it.
    let array = array.split_once("</array>")?.0;
    let mut args = Vec::new();
    let mut rest = array;
    while let Some((_, after_open)) = rest.split_once("<string>") {
        let Some((inner, after_close)) = after_open.split_once("</string>") else {
            break;
        };
        args.push(xml_unescape(inner));
        rest = after_close;
    }
    if args.is_empty() { None } else { Some(args) }
}

/// The program a plist's `ProgramArguments` names — the first argument — or
/// `None` when this parser does not recognise the document. See
/// [`registered_args`] for the narrowness this inherits.
#[must_use]
pub fn registered_exe(text: &str) -> Option<PathBuf> {
    registered_args(text)?.into_iter().next().map(PathBuf::from)
}

/// The store a plist's `ProgramArguments` names via `--store-path`, or `None`
/// when the array carries no such flag (the agent serves the login-time
/// default store) or this parser does not recognise the document. A flag with
/// no following value reads as `None` rather than panicking — a hand-edited or
/// truncated plist is exactly the kind of document this reader must survive.
#[must_use]
pub fn registered_store(text: &str) -> Option<PathBuf> {
    let args = registered_args(text)?;
    let index = args.iter().position(|arg| arg == "--store-path")?;
    args.get(index + 1).map(PathBuf::from)
}

/// What this machine's login agent is, and whether it still agrees with the
/// `story` its operator actually runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// No plist at all. `story daemon install` has never been run here, or
    /// `story daemon uninstall` has.
    NotInstalled,
    /// A plist this build cannot find a program in.
    Unreadable { plist: PathBuf },
    /// The plist at this store's own label actually serves a different
    /// store. Judged before every exe question, for the same reason
    /// [`Missing`](Health::Missing) is: a foreign agent's binary is not this
    /// store's business to report on. The state a machine that ran a
    /// pre-SH-414 `--store-path X daemon install` is left in — that install
    /// wrote `X`'s agent under the bare label, which now means "the
    /// login-time default's own agent".
    ServesAnotherStore {
        plist: PathBuf,
        exe: PathBuf,
        serves: PathBuf,
        wanted: PathBuf,
    },
    /// The agent names a binary that is no longer on disk. Judged before
    /// `$PATH` is consulted and independently of it — see the module doc.
    Missing { plist: PathBuf, exe: PathBuf },
    /// The agent names a binary that exists but is not the `story` `$PATH`
    /// runs. This is SH-411's own failure mode, seen from the reporting side.
    Disagrees {
        plist: PathBuf,
        exe: PathBuf,
        installed: PathBuf,
    },
    /// The agent names a binary that exists, and `$PATH` names no `story` to
    /// compare it against.
    Unconfirmable { plist: PathBuf, exe: PathBuf },
    /// The agent runs the `story` `$PATH` runs. Nothing to say.
    Agrees { plist: PathBuf, exe: PathBuf },
}

/// One read of the login agent's plist, with nothing judged yet.
///
/// A named struct rather than a positional argument list, deliberately: this
/// project has a standing rule against exactly that shape
/// (`tests/read_model_column_coverage.rs`'s own precedent, SH-365 — swap two
/// same-typed positional arguments and every call site still compiles, every
/// value still wrong). Named fields make a swap visible in the diff.
struct Reading {
    plist: PathBuf,
    found: bool,
    exe: Option<PathBuf>,
    exe_exists: bool,
    /// The store the plist's `ProgramArguments` claims to serve, canonicalized
    /// — the value after `--store-path`, or the login-time default when the
    /// array carries no such flag. Meaningless when `found` is false.
    serves: PathBuf,
    /// The store this call is actually asking about.
    wanted: PathBuf,
}

/// The store a plist's own `ProgramArguments` claims, canonicalized so two
/// spellings of one store read as one store. Absence of `--store-path`
/// resolves to the login-time default for `env`'s home, matching how a
/// flagless launchd child would resolve it — the same doctrine
/// [`serves_the_login_default`] states.
fn served_store(text: &str, env: &Environment) -> PathBuf {
    match registered_store(text) {
        Some(claimed) => crate::env::canonical_ish(&claimed).unwrap_or(claimed),
        None => StoreLocation::for_home(env.home()).path().to_path_buf(),
    }
}

/// Reads this machine's agent and judges it.
#[must_use]
pub fn health(env: &Environment) -> Health {
    let plist_path = path(env);
    let text = std::fs::read_to_string(&plist_path).ok();
    let exe = text.as_deref().and_then(registered_exe);
    let exists = exe.as_deref().is_some_and(Path::exists);
    let wanted = env.store_path().to_path_buf();
    let serves = text
        .as_deref()
        .map(|t| served_store(t, env))
        .unwrap_or_else(|| wanted.clone());
    let reading = Reading {
        plist: plist_path,
        found: text.is_some(),
        exe,
        exe_exists: exists,
        serves,
        wanted,
    };
    judge(reading, path_identity::installed_story())
}

/// The whole truth table, pure so it can be read as one.
///
/// `exe_canonical` is resolved by the caller rather than here, because this
/// function does no I/O — the same split [`crate::migration_guard::decide`]
/// draws, and for the same reason.
#[must_use]
fn judge(reading: Reading, installed: Option<path_identity::InstalledStory>) -> Health {
    let Reading {
        plist,
        found,
        exe,
        exe_exists,
        serves,
        wanted,
    } = reading;
    if !found {
        return Health::NotInstalled;
    }
    let Some(exe) = exe else {
        return Health::Unreadable { plist };
    };
    // Ahead of every exe question: if this label's plist serves a different
    // store, a binary disagreement on it is not this store's business to
    // report. Judged the same way Missing is judged ahead of the $PATH
    // comparison — the ordering is the point, not an accident.
    if serves != wanted {
        return Health::ServesAnotherStore {
            plist,
            exe,
            serves,
            wanted,
        };
    }
    if !exe_exists {
        return Health::Missing { plist, exe };
    }
    let Some(installed) = installed else {
        return Health::Unconfirmable { plist, exe };
    };
    let canonical = path_identity::canonicalize_or(exe.clone(), exe.clone());
    if canonical == installed.canonical {
        Health::Agrees { plist, exe }
    } else {
        Health::Disagrees {
            plist,
            exe,
            installed: installed.spelling,
        }
    }
}

/// The problem worth interrupting somebody about, or `None` when there is
/// none.
///
/// [`Health::Unconfirmable`] is deliberately silent, and that is the mirror of
/// [`crate::daemon::install_guard`]'s refusal on the same condition:
/// *creating* a durable artifact on an unconfirmable premise refuses, because
/// the artifact outlives the doubt; *reporting* on one that already exists says
/// nothing, because there is nothing definite to report.
#[must_use]
pub fn warning(health: &Health) -> Option<String> {
    match health {
        Health::NotInstalled | Health::Agrees { .. } | Health::Unconfirmable { .. } => None,
        Health::Unreadable { plist } => Some(format!(
            "the login agent's plist at {} does not name a program this storyhook \
             understands. Re-run `story daemon install` to rewrite it.",
            plist.display()
        )),
        Health::ServesAnotherStore { plist, serves, .. } => Some(format!(
            "the login agent at {} actually serves {}, not this store. Re-running \
             `story daemon install` here would silently replace it. If {} still needs \
             its own login agent, run `story --store-path {} daemon install` first, then \
             re-run `story daemon install` here.",
            plist.display(),
            serves.display(),
            serves.display(),
            serves.display()
        )),
        Health::Missing { exe, .. } => Some(format!(
            "the login agent runs {}, which no longer exists — the dashboard will not \
             come back after a reboot. Re-run `story daemon install`.",
            exe.display()
        )),
        Health::Disagrees { exe, installed, .. } => Some(format!(
            "the login agent runs {}, but your $PATH runs {}. At the next login launchd \
             will start the first of those. Re-run `story daemon install` to point the \
             agent at the one you use.",
            exe.display(),
            installed.display()
        )),
    }
}

/// Every state, rendered for `story daemon status` — including the ones
/// [`warning`] stays quiet about, because a reader who came to look is owed the
/// whole answer.
#[must_use]
pub fn describe(health: &Health) -> String {
    match health {
        Health::NotInstalled => {
            "login agent  not installed (`story daemon install` registers one)".to_string()
        }
        Health::Unreadable { plist } => format!(
            "login agent  {}\n             ! this storyhook cannot find a program in that plist \
             — re-run `story daemon install`",
            plist.display()
        ),
        Health::ServesAnotherStore {
            plist,
            exe,
            serves,
            ..
        } => format!(
            "login agent  {}\n             runs {}\n             ! serves {} instead of this \
             store — run `story --store-path {} daemon install` first if it still needs its \
             own agent, then re-run `story daemon install` here",
            plist.display(),
            exe.display(),
            serves.display(),
            serves.display()
        ),
        Health::Missing { plist, exe } => format!(
            "login agent  {}\n             runs {}\n             ! that binary no longer exists \
             — re-run `story daemon install`",
            plist.display(),
            exe.display()
        ),
        Health::Disagrees {
            plist,
            exe,
            installed,
        } => format!(
            "login agent  {}\n             runs {}\n             ! your $PATH runs {} — re-run \
             `story daemon install`",
            plist.display(),
            exe.display(),
            installed.display()
        ),
        Health::Unconfirmable { plist, exe } => format!(
            "login agent  {}\n             runs {}\n             ($PATH names no `story` to \
             compare it against)",
            plist.display(),
            exe.display()
        ),
        Health::Agrees { plist, exe } => format!(
            "login agent  {}\n             runs {}",
            plist.display(),
            exe.display()
        ),
    }
}

/// One other storyhook login agent found on this machine — not this store's
/// own.
struct OtherAgent {
    plist: PathBuf,
    serves: PathBuf,
    exe: Option<PathBuf>,
}

/// Every OTHER storyhook login agent on this machine: plists in
/// `~/Library/LaunchAgents` under this project's own label prefix, excluding
/// the one [`path`] would read for `env`'s own store.
///
/// Per-store labels remove the accidental self-limiting collision a shared
/// label used to provide — a second install no longer replaces the first, it
/// coexists — so this is the visibility that stops the accumulation from
/// being invisible. An entry whose plist this build cannot parse is still
/// named, with `exe: None`, rather than silently skipped: the SH-312 rule
/// that an unprovable outcome is reported as unprovable, never dropped.
#[must_use]
fn others(env: &Environment) -> Vec<OtherAgent> {
    let dir = env.home().join("Library/LaunchAgents");
    let own = path(env);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<OtherAgent> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            *candidate != own
                && candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(LAUNCHD_LABEL) && name.ends_with(".plist")
                    })
        })
        .map(|plist| {
            let text = std::fs::read_to_string(&plist).ok();
            let exe = text.as_deref().and_then(registered_exe);
            let serves = text
                .as_deref()
                .map(|t| served_store(t, env))
                .unwrap_or_else(|| plist.clone());
            OtherAgent { plist, serves, exe }
        })
        .collect();
    found.sort_by(|a, b| a.plist.cmp(&b.plist));
    found
}

/// The block naming every other storyhook login agent this machine has, or
/// `String::new()` when there are none. `pub` rather than folded only into
/// [`report`]: `install`'s and `uninstall`'s own success messages append it
/// too, so accumulation is named at the moment a machine grows past one
/// store, not only when someone happens to run `status`.
#[must_use]
pub fn describe_others(env: &Environment) -> String {
    let others = others(env);
    if others.is_empty() {
        return String::new();
    }
    let mut lines = vec!["other login agents on this machine".to_string()];
    for other in others {
        let ran = match &other.exe {
            Some(exe) => format!(", runs {}", exe.display()),
            None => ", this storyhook cannot find a program in that plist".to_string(),
        };
        lines.push(format!(
            "  {}\n    serves {}{ran}",
            other.plist.display(),
            other.serves.display()
        ));
    }
    lines.join("\n")
}

/// This store's own login-agent health, plus — when there are any — every
/// other storyhook login agent on this machine. One function rather than a
/// pair of calls at each of `status`'s three call sites (and `install`'s and
/// `uninstall`'s own messages), so the machine-wide facility cannot be
/// reported at one and silently dropped at another.
#[must_use]
pub fn report(env: &Environment) -> String {
    let mut said = describe(&health(env));
    let others = describe_others(env);
    if !others.is_empty() {
        said.push_str("\n\n");
        said.push_str(&others);
    }
    said
}

/// The three characters that cannot appear literally in XML element text.
///
/// `&` goes first, or it would escape the ampersands the other two just
/// introduced. [`xml_unescape`] reverses in the opposite order for the same
/// reason, so a literal `&amp;` in a path round-trips.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The inverse of [`xml_escape`], and the reason the escaper is not shipped
/// alone: a writer with no reader would reintroduce the same defect on the read
/// side one release later.
fn xml_unescape(s: &str) -> String {
    s.replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-daemon-agent-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// **`RunAtLoad` yes, `KeepAlive` no**, and both halves are decisions rather
    /// than defaults.
    ///
    /// It was worth re-taking once the daemon became mandatory (SH-114): an
    /// optional process that nothing restarts is a choice, and a *required* one
    /// that nothing restarts sounds like an oversight. It is not, and the
    /// reasons got stronger rather than weaker.
    ///
    /// - **Availability is already self-healing.** Every client calls
    ///   `lifecycle::ensure`, so a daemon that is not running is started by
    ///   whoever needs it next. A restarter would be a second mechanism for
    ///   something that has one.
    /// - **A restarter is now more dangerous than it was.** `spawn_locked`
    ///   stands the old daemon down and *then* claims the pidfile, so launchd
    ///   racing that window turns a deterministic failure into an intermittent
    ///   one — and since SH-114 that window is on every command's path, not on
    ///   the subset that chose the daemon.
    /// - **The version-upgrade race survives verbatim**: an agent that restarts
    ///   the daemon immediately races the client that just asked it to stop
    ///   because the binary moved.
    /// - **`KeepAlive{SuccessfulExit:false}`**, the surgical variant that looks
    ///   like it avoids all of the above, is the worst of them. It turns "the
    ///   store is damaged and the daemon cannot open it" — the exact scenario
    ///   the cannot-start diagnostic exists for — into a respawn loop on
    ///   launchd's ten-second throttle.
    #[test]
    fn the_agent_runs_the_daemon_at_login_and_never_resurrects_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>--serve</string>"));
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "the daemon is started at login, so a machine that has just booted \
             has a dashboard without anybody typing a command: {plist}"
        );
        assert!(
            !plist.contains("KeepAlive"),
            "and it is never restarted. A restarter races `spawn_locked`'s \
             stand-down window on every command's path, and the surgical \
             `SuccessfulExit: false` variant turns a damaged store into a \
             respawn loop on launchd's ten-second throttle: {plist}"
        );
    }

    /// The agent's log path is already this store's. An agent that ran the
    /// *default* store's daemon while writing into a named store's directory
    /// would be describing one process and starting another.
    #[test]
    fn the_agent_names_a_store_that_is_not_the_default_one() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        let env = Environment::at(dir.path());
        let default_plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(
            !default_plist.contains("--store-path"),
            "the default store's agent must stay byte-identical to the one every \
             existing installation has"
        );

        let env = env.with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir.path(),
            )
            .expect("resolving a named store"),
        );
        let plist = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>--store-path</string>"), "{plist}");
        assert!(
            plist.contains(&format!("<string>{}</string>", env.store_path().display())),
            "{plist}"
        );
    }

    #[test]
    fn the_agent_label_follows_the_bundle_convention() {
        assert_eq!(LAUNCHD_LABEL, "io.mikey.storyhook.daemon");
    }

    #[test]
    fn the_default_stores_label_is_the_bare_one() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert_eq!(label(&env), LAUNCHD_LABEL);
    }

    #[test]
    fn a_named_store_gets_its_own_label_and_its_own_plist_path() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        let env = Environment::at(dir.path()).with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir.path(),
            )
            .expect("resolving a named store"),
        );
        assert_eq!(
            label(&env),
            format!("{LAUNCHD_LABEL}.{}", env.store().key())
        );
        assert_ne!(path(&env), path(&Environment::at(dir.path())));
    }

    /// Two different non-default stores must never collide on one label — the
    /// exact defect a shared label produced.
    #[test]
    fn two_named_stores_get_different_labels() {
        let dir = scratch();
        let a = dir.path().join("a.db");
        let b = dir.path().join("b.db");
        let vars = crate::env::StoreVars::default();
        let env_a = Environment::at(dir.path()).with_store(
            crate::env::StoreLocation::resolve(Some(&a), &vars, dir.path())
                .expect("resolving store a"),
        );
        let env_b = Environment::at(dir.path()).with_store(
            crate::env::StoreLocation::resolve(Some(&b), &vars, dir.path())
                .expect("resolving store b"),
        );
        assert_ne!(label(&env_a), label(&env_b));
    }

    /// A plist's declared `Label` and its filename must always agree — `launchctl
    /// bootstrap` targets the file, `bootout`/`kickstart`/`print` target the
    /// label, and a disagreement makes an install "succeed" while its own
    /// uninstall silently fails to unload it.
    #[test]
    fn the_plist_names_the_label_its_filename_carries() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        for env in [
            Environment::at(dir.path()),
            Environment::at(dir.path()).with_store(
                crate::env::StoreLocation::resolve(
                    Some(&named),
                    &crate::env::StoreVars::default(),
                    dir.path(),
                )
                .expect("resolving a named store"),
            ),
        ] {
            let rendered = plist(Path::new("/usr/local/bin/story"), &env);
            let file_stem = path(&env)
                .file_stem()
                .expect("a plist filename")
                .to_string_lossy()
                .to_string();
            assert_eq!(file_stem, label(&env));
            assert!(
                rendered.contains(&format!("<string>{}</string>", label(&env))),
                "{rendered}"
            );
        }
    }

    /// The predicate a store reaches the login default through must not be
    /// `StoreLocation::is_default()` — see [`serves_the_login_default`]'s own
    /// doc. Measured here rather than only argued: a store named solely via
    /// `$XDG_DATA_HOME` reads `is_default() == true` (both sides of that
    /// comparison are computed from the same overridden variable), yet a
    /// launchd child — which inherits none of this process's environment —
    /// would open the *true* default instead. This store must therefore still
    /// get its own keyed label and its own `--store-path` flag, exactly as a
    /// `--store-path`-flagged store does.
    #[test]
    fn a_store_reached_via_xdg_data_home_still_gets_its_own_label() {
        let dir = scratch();
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(&xdg).expect("the xdg data home");
        let vars = crate::env::StoreVars {
            xdg_data_home: Some(xdg),
            ..Default::default()
        };
        let env = Environment::at(dir.path()).with_store(
            crate::env::StoreLocation::resolve(None, &vars, dir.path())
                .expect("resolving via XDG_DATA_HOME"),
        );
        assert!(
            env.store().is_default(),
            "is_default() is fooled by $XDG_DATA_HOME by construction — that is the whole point of this test"
        );
        assert_ne!(
            label(&env),
            LAUNCHD_LABEL,
            "a store reached only via $XDG_DATA_HOME is not what a launchd child would open with no flags"
        );
        let rendered = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(
            rendered.contains("<string>--store-path</string>"),
            "the agent must carry --store-path, or a launchd child would silently open the \
             true default instead: {rendered}"
        );
    }

    /// The pin that says the escaping fix is invisible to every installation
    /// that already exists: an ordinary path still produces exactly the bytes
    /// this plist has always had.
    #[test]
    fn an_ordinary_path_is_untouched_by_the_escaping() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let rendered = plist(Path::new("/usr/local/bin/story"), &env);
        assert!(
            rendered.contains("<string>/usr/local/bin/story</string>"),
            "{rendered}"
        );
        assert!(!rendered.contains("&amp;"), "{rendered}");
    }

    /// A path holding XML metacharacters used to write a malformed plist that
    /// launchd would refuse — silently, at the next login. Escaped now, and the
    /// reader gets the original path back.
    #[test]
    fn the_reader_round_trips_the_writer() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        for raw in [
            "/usr/local/bin/story",
            "/tmp/a & b/story",
            "/tmp/<odd>/story",
            "/tmp/already &amp; escaped/story",
        ] {
            let rendered = plist(Path::new(raw), &env);
            assert_eq!(
                registered_exe(&rendered),
                Some(PathBuf::from(raw)),
                "the reader must return exactly what the writer was given: {rendered}"
            );
        }
    }

    /// The `--store-path` shape puts a second `<string>` in the same array. The
    /// reader must still answer with the *program*, not the flag.
    #[test]
    fn the_reader_returns_the_program_not_the_store_flag() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        let env = Environment::at(dir.path()).with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir.path(),
            )
            .expect("resolving a named store"),
        );
        let rendered = plist(Path::new("/usr/local/bin/story"), &env);
        assert_eq!(
            registered_exe(&rendered),
            Some(PathBuf::from("/usr/local/bin/story"))
        );
    }

    #[test]
    fn a_plist_this_build_cannot_parse_reads_as_none() {
        assert_eq!(registered_exe(""), None);
        assert_eq!(registered_exe("<plist><dict></dict></plist>"), None);
        assert_eq!(
            registered_exe("<key>ProgramArguments</key><array></array>"),
            None,
            "an empty array names no program, and must not reach past its own </array>"
        );
        assert_eq!(
            registered_exe("<key>ProgramArguments</key><array></array><string>/nope</string>"),
            None,
            "the search is bounded by the array it started in"
        );
    }

    /// The store-path reader round-trips through the same escaping the writer
    /// uses, including a store path holding XML metacharacters — a live gap
    /// before this: the value was escaped on write and never unescaped on
    /// read.
    #[test]
    fn the_reader_returns_the_store_the_agent_serves() {
        let dir = scratch();
        for raw in ["/tmp/named.db", "/tmp/a & b/named.db", "/tmp/<odd>/named.db"] {
            let named = PathBuf::from(raw);
            let env = Environment::at(dir.path()).with_store(
                crate::env::StoreLocation::resolve(
                    Some(&named),
                    &crate::env::StoreVars::default(),
                    dir.path(),
                )
                .expect("resolving a named store"),
            );
            let rendered = plist(Path::new("/usr/local/bin/story"), &env);
            assert_eq!(
                registered_store(&rendered),
                Some(env.store_path().to_path_buf()),
                "the reader must return exactly the store path the writer embedded: {rendered}"
            );
        }
    }

    /// The default store's plist carries no `--store-path` flag at all, so the
    /// reader must answer `None` rather than misreading the next argument
    /// (`daemon`) as a store.
    #[test]
    fn a_plist_with_no_store_flag_reads_as_serving_no_named_store() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let rendered = plist(Path::new("/usr/local/bin/story"), &env);
        assert_eq!(registered_store(&rendered), None, "{rendered}");
    }

    /// A hand-edited or truncated plist can carry the flag with nothing after
    /// it. The reader must report `None`, never panic on an out-of-bounds
    /// index.
    #[test]
    fn registered_store_ignores_a_flag_with_no_value() {
        assert_eq!(
            registered_store(
                "<key>ProgramArguments</key><array><string>/usr/local/bin/story</string>\
                 <string>--store-path</string></array>"
            ),
            None
        );
    }

    // -- judge ---------------------------------------------------------------

    fn installed(spelling: &str, canonical: &str) -> path_identity::InstalledStory {
        path_identity::InstalledStory {
            spelling: PathBuf::from(spelling),
            canonical: PathBuf::from(canonical),
        }
    }

    fn plist_path() -> PathBuf {
        PathBuf::from("/home/dev/Library/LaunchAgents/io.mikey.storyhook.daemon.plist")
    }

    fn wanted_store() -> PathBuf {
        PathBuf::from("/home/dev/.local/share/storyhook/store.db")
    }

    /// `serves` defaults to `wanted`, so every existing row's fixture is
    /// unaffected by the store question — it never fires unless a test asks
    /// for it, via [`foreign_reading`].
    fn reading(found: bool, exe: Option<&str>, exe_exists: bool) -> Reading {
        Reading {
            plist: plist_path(),
            found,
            exe: exe.map(PathBuf::from),
            exe_exists,
            serves: wanted_store(),
            wanted: wanted_store(),
        }
    }

    fn foreign_reading(exe: Option<&str>, exe_exists: bool) -> Reading {
        Reading {
            serves: PathBuf::from("/scratch/other.db"),
            ..reading(true, exe, exe_exists)
        }
    }

    #[test]
    fn judge_reports_no_plist_as_not_installed() {
        assert_eq!(
            judge(reading(false, None, false), None),
            Health::NotInstalled
        );
    }

    #[test]
    fn judge_reports_an_unparseable_plist() {
        assert_eq!(
            judge(reading(true, None, false), None),
            Health::Unreadable {
                plist: plist_path()
            }
        );
    }

    /// The row that reaches the launchd-shaped machine: a deleted binary is
    /// judged *before* `$PATH` is consulted, so it is reported even where
    /// `$PATH` names no `story` at all.
    #[test]
    fn judge_reports_a_deleted_binary_without_consulting_path() {
        let exe = PathBuf::from("/gone/story");
        assert_eq!(
            judge(reading(true, Some("/gone/story"), false), None),
            Health::Missing {
                plist: plist_path(),
                exe
            }
        );
    }

    #[test]
    fn judge_reports_an_unconfirmable_agent_when_path_names_no_story() {
        let exe = PathBuf::from("/usr/local/bin/story");
        assert_eq!(
            judge(reading(true, Some("/usr/local/bin/story"), true), None),
            Health::Unconfirmable {
                plist: plist_path(),
                exe
            }
        );
    }

    #[test]
    fn judge_agrees_when_the_agent_runs_the_installed_story() {
        let exe = PathBuf::from("/usr/local/bin/story");
        assert_eq!(
            judge(
                reading(true, Some("/usr/local/bin/story"), true),
                Some(installed("/usr/local/bin/story", "/usr/local/bin/story"))
            ),
            Health::Agrees {
                plist: plist_path(),
                exe
            }
        );
    }

    /// SH-411's own failure mode, seen from the reporting side — and the
    /// installed binary is named by its `$PATH` *spelling*, which is what the
    /// operator would type.
    #[test]
    fn judge_reports_an_agent_that_runs_a_binary_path_does_not() {
        let exe = PathBuf::from("/repo/target/debug/story");
        assert_eq!(
            judge(
                reading(true, Some("/repo/target/debug/story"), true),
                Some(installed(
                    "/home/dev/.local/bin/story",
                    "/home/dev/.local/bin/story"
                ))
            ),
            Health::Disagrees {
                plist: plist_path(),
                exe,
                installed: PathBuf::from("/home/dev/.local/bin/story"),
            }
        );
    }

    /// The state a pre-SH-414 `--store-path X daemon install` leaves behind:
    /// the bare label's plist actually serves a different store.
    #[test]
    fn judge_reports_an_agent_that_serves_another_store() {
        let exe = PathBuf::from("/usr/local/bin/story");
        assert_eq!(
            judge(foreign_reading(Some("/usr/local/bin/story"), true), None),
            Health::ServesAnotherStore {
                plist: plist_path(),
                exe,
                serves: PathBuf::from("/scratch/other.db"),
                wanted: wanted_store(),
            }
        );
    }

    /// The dominance the module doc claims: a foreign agent whose exe is
    /// ALSO missing must still report `ServesAnotherStore`, not `Missing` —
    /// a foreign agent's binary is not this store's business to report on.
    #[test]
    fn the_store_question_is_asked_before_the_binary_questions() {
        let exe = PathBuf::from("/gone/story");
        assert_eq!(
            judge(foreign_reading(Some("/gone/story"), false), None),
            Health::ServesAnotherStore {
                plist: plist_path(),
                exe,
                serves: PathBuf::from("/scratch/other.db"),
                wanted: wanted_store(),
            }
        );
    }

    /// `warning` interrupts for a definite problem and nothing else. The
    /// `Unconfirmable` silence is the deliberate mirror of the install guard's
    /// refusal on the same condition — see [`warning`]'s own doc.
    #[test]
    fn warning_is_silent_unless_there_is_something_definite_to_say() {
        let exe = PathBuf::from("/usr/local/bin/story");
        for quiet in [
            Health::NotInstalled,
            Health::Agrees {
                plist: plist_path(),
                exe: exe.clone(),
            },
            Health::Unconfirmable {
                plist: plist_path(),
                exe: exe.clone(),
            },
        ] {
            assert_eq!(
                warning(&quiet),
                None,
                "{quiet:?} must not interrupt anybody"
            );
        }
        for loud in [
            Health::Unreadable {
                plist: plist_path(),
            },
            Health::Missing {
                plist: plist_path(),
                exe: exe.clone(),
            },
            Health::Disagrees {
                plist: plist_path(),
                exe: exe.clone(),
                installed: PathBuf::from("/home/dev/.local/bin/story"),
            },
            Health::ServesAnotherStore {
                plist: plist_path(),
                exe,
                serves: PathBuf::from("/scratch/other.db"),
                wanted: wanted_store(),
            },
        ] {
            let said = warning(&loud).unwrap_or_else(|| panic!("{loud:?} must be reported"));
            assert!(
                said.contains("story daemon install"),
                "a warning with no remedy is unactionable: {said}"
            );
        }
    }

    /// Every state renders, including the quiet ones — `status` is where a
    /// reader who came to look is owed the whole answer.
    #[test]
    fn describe_names_the_agent_in_every_state() {
        let exe = PathBuf::from("/usr/local/bin/story");
        for health in [
            Health::NotInstalled,
            Health::Unreadable {
                plist: plist_path(),
            },
            Health::Missing {
                plist: plist_path(),
                exe: exe.clone(),
            },
            Health::Unconfirmable {
                plist: plist_path(),
                exe: exe.clone(),
            },
            Health::Agrees {
                plist: plist_path(),
                exe: exe.clone(),
            },
            Health::Disagrees {
                plist: plist_path(),
                exe: exe.clone(),
                installed: PathBuf::from("/home/dev/.local/bin/story"),
            },
            Health::ServesAnotherStore {
                plist: plist_path(),
                exe,
                serves: PathBuf::from("/scratch/other.db"),
                wanted: wanted_store(),
            },
        ] {
            let said = describe(&health);
            assert!(
                said.starts_with("login agent  "),
                "{health:?} rendered as {said}"
            );
        }
    }

    // -- enumeration -----------------------------------------------------------

    fn write_raw_plist(env: &Environment, filename: &str, contents: &str) -> PathBuf {
        let dir = env.home().join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).expect("the LaunchAgents directory");
        let file = dir.join(filename);
        std::fs::write(&file, contents).expect("planting a plist");
        file
    }

    /// The negative control: with nothing else on the machine, enumeration
    /// says nothing.
    #[test]
    fn describe_others_is_empty_when_there_is_nothing_else() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert_eq!(describe_others(&env), String::new());
    }

    /// This store's own agent, freshly written, must never be named as
    /// "another" — the self-exclusion is by path equality, not by content.
    #[test]
    fn this_stores_own_agent_is_not_named_as_another() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        write_raw_plist(
            &env,
            &format!("{LAUNCHD_LABEL}.plist"),
            "<key>ProgramArguments</key><array><string>/usr/local/bin/story</string></array>",
        );
        assert!(others(&env).is_empty());
        assert_eq!(describe_others(&env), String::new());
    }

    /// A plist outside this project's label namespace — a launchd agent for
    /// an unrelated tool — must never be swept up.
    #[test]
    fn an_unrelated_plist_is_ignored() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        write_raw_plist(
            &env,
            "com.apple.something.plist",
            "<key>ProgramArguments</key><array><string>/usr/bin/true</string></array>",
        );
        assert!(others(&env).is_empty());
    }

    /// A sibling agent for another store is named, with the store it serves
    /// and the exe it runs.
    #[test]
    fn describe_others_names_a_sibling_agent() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let named = dir.path().join("named.db");
        write_raw_plist(
            &env,
            &format!("{LAUNCHD_LABEL}.deadbeefdeadbeef.plist"),
            &format!(
                "<key>ProgramArguments</key><array><string>/usr/local/bin/story</string>\
                 <string>--store-path</string><string>{}</string></array>",
                named.display()
            ),
        );
        let said = describe_others(&env);
        assert!(said.contains("other login agents on this machine"), "{said}");
        assert!(said.contains(&named.display().to_string()), "{said}");
        assert!(said.contains("/usr/local/bin/story"), "{said}");
    }

    /// An unparseable sibling is still named — SH-312's rule that an
    /// unprovable outcome is reported as unprovable, never silently skipped.
    #[test]
    fn an_agent_whose_store_cannot_be_read_is_named_anyway() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plist = write_raw_plist(
            &env,
            &format!("{LAUNCHD_LABEL}.deadbeefdeadbeef.plist"),
            "not a plist at all",
        );
        let found = others(&env);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].plist, plist);
        assert_eq!(found[0].exe, None);
        let said = describe_others(&env);
        assert!(
            said.contains("cannot find a program in that plist"),
            "{said}"
        );
    }
}
