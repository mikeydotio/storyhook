//! The launchd login agent: its plist, written **and** read.
//!
//! `story daemon install` registers a user agent that starts the daemon at
//! login, so the dashboard is there after a reboot without anybody running a
//! command. That plist is a durable, machine-wide artifact — it outlives the
//! shell that created it, every build it names, and most reboots — which is
//! what makes both of this module's halves necessary.
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
//! # Health is not just a `$PATH` comparison
//!
//! [`judge`] decides [`Health::Missing`] — the registered binary no longer
//! exists — **before** `$PATH` is consulted, and independently of it. That
//! ordering is the point rather than an accident: the machine this whole story
//! is about is one where `$PATH` names no `story` at all, so a report that
//! could only compare against `$PATH` would have nothing to say on precisely
//! the machine that needs it.

use std::path::{Path, PathBuf};

use crate::env::Environment;
use crate::path_identity;

/// The launchd label for the user agent.
///
/// Reverse-DNS under the author's own domain, which is the convention every
/// other bundle identifier in this ecosystem follows.
pub const LAUNCHD_LABEL: &str = "io.mikey.storyhook.daemon";

/// Where the launchd agent's plist goes.
pub fn path(env: &Environment) -> PathBuf {
    env.home()
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
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
    <string>{LAUNCHD_LABEL}</string>
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
        exe = xml_escape(&exe.display().to_string()),
        // A login agent for the default store needs no flag, and leaving it off
        // keeps the plist identical to the one every existing installation has.
        // For any other store the flag is required rather than tidy: the log
        // path below is already this store's, so an agent without it would run
        // one store's daemon while writing into another's directory.
        store = if env.store().is_default() {
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

/// The program a plist's `ProgramArguments` names, or `None` when this parser
/// does not recognise the document.
///
/// Deliberately narrow: the first `<string>` inside the `<array>` that follows
/// `<key>ProgramArguments</key>`. A hand-edited plist, or one a future
/// storyhook wrote in a shape this build has never seen, reads as
/// [`Health::Unreadable`] and is *reported* rather than guessed at — the SH-312
/// rule that an unprovable outcome is reported as unprovable, never as a
/// confident wrong answer. Forgery is not the threat model; anyone who can
/// hand-write this file can also run `launchctl` directly.
#[must_use]
pub fn registered_exe(text: &str) -> Option<PathBuf> {
    let after_key = text.split_once("<key>ProgramArguments</key>")?.1;
    let array = after_key.split_once("<array>")?.1;
    // Bound the search to this array, so a malformed document cannot reach into
    // whatever element happens to follow it.
    let array = array.split_once("</array>")?.0;
    let inner = array.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(PathBuf::from(xml_unescape(inner)))
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

/// Reads this machine's agent and judges it.
#[must_use]
pub fn health(env: &Environment) -> Health {
    let plist_path = path(env);
    let text = std::fs::read_to_string(&plist_path).ok();
    let exe = text.as_deref().and_then(registered_exe);
    let exists = exe.as_deref().is_some_and(Path::exists);
    judge(
        plist_path,
        text.is_some(),
        exe,
        exists,
        path_identity::installed_story(),
    )
}

/// The whole truth table, pure so it can be read as one.
///
/// `exe_canonical` is resolved by the caller rather than here, because this
/// function does no I/O — the same split [`crate::migration_guard::decide`]
/// draws, and for the same reason.
#[must_use]
fn judge(
    plist: PathBuf,
    plist_exists: bool,
    exe: Option<PathBuf>,
    exe_exists: bool,
    installed: Option<path_identity::InstalledStory>,
) -> Health {
    if !plist_exists {
        return Health::NotInstalled;
    }
    let Some(exe) = exe else {
        return Health::Unreadable { plist };
    };
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

    #[test]
    fn judge_reports_no_plist_as_not_installed() {
        assert_eq!(
            judge(plist_path(), false, None, false, None),
            Health::NotInstalled
        );
    }

    #[test]
    fn judge_reports_an_unparseable_plist() {
        assert_eq!(
            judge(plist_path(), true, None, false, None),
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
            judge(plist_path(), true, Some(exe.clone()), false, None),
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
            judge(plist_path(), true, Some(exe.clone()), true, None),
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
                plist_path(),
                true,
                Some(exe.clone()),
                true,
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
                plist_path(),
                true,
                Some(exe.clone()),
                true,
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
                exe,
                installed: PathBuf::from("/home/dev/.local/bin/story"),
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
                exe,
                installed: PathBuf::from("/home/dev/.local/bin/story"),
            },
        ] {
            let said = describe(&health);
            assert!(
                said.starts_with("login agent  "),
                "{health:?} rendered as {said}"
            );
        }
    }
}
