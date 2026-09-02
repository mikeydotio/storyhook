//! The test-environment parameter set, and everything that has to agree with it.
//!
//! `storyhook::env::test_environment::TEST_ENVIRONMENT` is the one definition of
//! what isolating a storyhook run means. Before it existed the same list sat in
//! seven hand-copied places that had already drifted; these tests are what stop
//! an eighth appearing, and what stop the copies that remain from disagreeing.
//!
//! Every scan here is **derived** — from the table, or from `git ls-files` — for
//! the reason a hand-kept list is exactly the failure this story exists to fix.

use storyhook::env::test_environment::{Disposition, TEST_ENVIRONMENT};
use storyhook::help_topics::get_help_topic;

/// The topic's own key. Named once here rather than typed at each assertion.
const TOPIC: &str = "test-environment";

fn topic_body() -> &'static str {
    get_help_topic(TOPIC).unwrap_or_else(|| {
        panic!("`story help {TOPIC}` must exist — it is how a suite in another repository learns to isolate itself")
    })
}

/// Every parameter reaches the shipped text.
///
/// Trivially true today, because the topic is rendered from the table. That is
/// the point: this test is what fails if somebody replaces the rendering with a
/// hand-written copy, which is the change that would look like an
/// improvement and would silently freeze the documentation at today's list.
#[test]
fn the_help_topic_names_every_parameter() {
    let body = topic_body();
    let missing: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .filter(|name| !body.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "`story help {TOPIC}` does not mention {missing:?}. A suite reading that \
         topic would isolate itself incompletely and never be told."
    );
}

/// …and the topic names no storyhook variable the table does not.
///
/// The other direction, and the one that catches stale prose: a paragraph that
/// still names a variable the parameter set has dropped tells a reader to set
/// something storyhook no longer reads, which is worse than saying nothing.
#[test]
fn the_help_topic_names_no_variable_the_parameter_set_omits() {
    let body = topic_body();
    let known: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .collect();

    let mut strays: Vec<String> = Vec::new();
    for word in body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if !word.starts_with("STORYHOOK_") {
            continue;
        }
        if !known.contains(&word) && !strays.iter().any(|s| s == word) {
            strays.push(word.to_string());
        }
    }
    assert!(
        strays.is_empty(),
        "`story help {TOPIC}` names {strays:?}, which is not in TEST_ENVIRONMENT. \
         Either add the parameter or stop naming it in the topic — a variable \
         documented as part of the contract and absent from the code is one a \
         reader will set for no effect."
    );
}

/// The scan above can only prove anything if it actually finds variable names.
///
/// A positive control in the SH-364 shape: a parser that stopped recognising
/// `STORYHOOK_*` words would report a clean tree forever. The fixture is
/// assembled at run time so this file's own source carries no stray name for
/// the scan to trip over.
#[test]
fn the_stray_variable_scan_can_see_a_stray() {
    let prefix = "STORYHOOK";
    let planted = format!("{prefix}_A_VARIABLE_NOBODY_DEFINED");
    let body = format!("some prose mentioning ${planted} in passing");

    let known: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .collect();
    let found: Vec<&str> = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with("STORYHOOK_"))
        .filter(|word| !known.contains(word))
        .collect();
    assert_eq!(
        found,
        [planted.as_str()],
        "the scan in the test above cannot see a planted stray, so its silence \
         proves nothing"
    );
}

/// The topic's first line has to parse as an invocation.
///
/// `tests/help_topic_usage.rs` reads the leading block of every topic as usage
/// lines, exactly as it already does for `priority-rubric` and `scope-rubric`.
/// Asserted here too, next to the topic it is about, because the failure over
/// there names a parsing rule rather than this topic.
#[test]
fn the_help_topic_opens_with_its_own_invocation() {
    let first = topic_body().lines().next().expect("a non-empty topic");
    assert_eq!(first, format!("story help {TOPIC}"));
}

/// The shipped text is written for a stranger's repository.
///
/// `story scaffold` points other projects at storyhook's help, so a topic that
/// cites this tracker's own story ids is telling a reader to look up something
/// they cannot see. The same boundary `tests/priority_rubric.rs` keeps.
#[test]
fn the_help_topic_cites_no_story_of_this_projects_own() {
    let body = topic_body();
    let cited: Vec<&str> = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| {
            word.strip_prefix("SH-")
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    assert!(
        cited.is_empty(),
        "`story help {TOPIC}` cites {cited:?}. This text ships to repositories \
         whose trackers have no such ids; put the case study in CLAUDE.md and \
         keep the rule here."
    );
}

/// A parameter that names a *file* is the one shape `directories()` has to
/// treat differently, and there is exactly one of them.
///
/// Pinned so that adding a second file-valued parameter is a decision somebody
/// makes on purpose: `directories()` infers "this tail is a file" from it
/// carrying an extension, which is true of this layout and would stop being
/// true silently.
#[test]
fn only_the_store_names_a_file() {
    let with_extension: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .filter(|parameter| match parameter.disposition {
            Disposition::Root(tail) => std::path::Path::new(tail).extension().is_some(),
            _ => false,
        })
        .map(|parameter| parameter.name)
        .collect();
    assert_eq!(
        with_extension,
        ["STORYHOOK_STORE_PATH"],
        "`directories()` reads a trailing extension as \"this parameter names a \
         file, so create its parent\". A second one is fine, but say so here."
    );
}

// ---------------------------------------------------------------------------
// The shell rendering, and the harnesses that use it
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use storyhook::env::test_environment::{Scope, resolve};
use storyhook_test_support::scratch_dir;

/// This checkout's root.
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The environment a `bash` that has sourced `scripts/test-env.sh` and called
/// `storyhook_isolate` actually ends up with, plus that shell's own pid.
///
/// **Every parameter is poisoned in the parent first.** The failure this test
/// exists to catch is a parameter the shell forgets, and a forgotten parameter
/// is invisible when the parent did not have one either: the child shows
/// nothing, the table says nothing should be there, and a missing `export` and
/// a correct `unset` look identical. Handing the shell a wrong value for every
/// single parameter is what makes "it left this one alone" observable.
fn isolate_in_bash(root: &Path, extra: &[&str]) -> (BTreeMap<String, String>, u32) {
    let script = format!(
        ". \"{}/scripts/test-env.sh\"\nstoryhook_isolate {} \"{}\"\necho \"__SHELL_PID__=$$\"\nexec /usr/bin/env\n",
        repo_root().display(),
        extra.join(" "),
        root.display(),
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .envs(poison())
        .output()
        .expect("running bash");
    assert!(
        out.status.success(),
        "the shell rendering refused a root it should have accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();

    let mut pid = None;
    let mut seen = BTreeMap::new();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name == "__SHELL_PID__" {
            pid = Some(value.parse::<u32>().expect("a pid"));
            continue;
        }
        seen.insert(name.to_string(), value.to_string());
    }
    (seen, pid.expect("the shell printed its own pid"))
}

/// A wrong value for every parameter, for the parent to hand the shell.
fn poison() -> Vec<(String, String)> {
    TEST_ENVIRONMENT
        .iter()
        .map(|parameter| {
            (
                parameter.name.to_string(),
                format!("/decoy/{}", parameter.name),
            )
        })
        .collect()
}

/// The shell rendering and the library agree, in both directions, for both
/// scopes.
///
/// Behavioural, never structural (the SH-357 doctrine): the question asked is
/// what the two actually put in a process's environment, not whether their
/// source looks alike. A scan comparing shapes passes while the two mean
/// different things, which is the whole failure mode a second copy of a list
/// has.
#[test]
fn the_shell_rendering_and_the_library_isolate_identically() {
    for (extra, scope) in [
        (&[][..], Scope::Anywhere),
        (&["--home"][..], Scope::StoryhookProcessOnly),
    ] {
        let fixture = scratch_dir();
        let root = fixture.path();
        let (seen, shell_pid) = isolate_in_bash(root, extra);
        let expected = resolve(root, shell_pid, scope);

        for setting in &expected {
            match &setting.value {
                Some(value) => {
                    let want = value.to_str().expect("a UTF-8 fixture path");
                    assert_eq!(
                        seen.get(setting.name).map(String::as_str),
                        Some(want),
                        "with {extra:?}, the shell set {} to {:?} and the library \
                         says {want:?}. One of the two is what a harness will \
                         actually run under.",
                        setting.name,
                        seen.get(setting.name)
                    );
                }
                None => assert!(
                    !seen.contains_key(setting.name),
                    "with {extra:?}, the shell left {} in the child's environment \
                     as {:?}; the library removes it. A parameter with no \
                     harmless value has none in shell either.",
                    setting.name,
                    seen.get(setting.name)
                ),
            }
        }

        // The other direction: nothing the shell touched is outside the table.
        // Compared against a control child of the same poisoned parent, so the
        // shell's own bookkeeping is not mistaken for the function's work.
        let control = std::process::Command::new("bash")
            .arg("-c")
            .arg("exec /usr/bin/env")
            .envs(poison())
            .output()
            .expect("running the control shell");
        let control: BTreeMap<String, String> = String::from_utf8_lossy(&control.stdout)
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();

        let known: Vec<&str> = expected.iter().map(|setting| setting.name).collect();
        let mut strays: Vec<String> = Vec::new();
        for (name, value) in &seen {
            if known.contains(&name.as_str()) || SHELL_BOOKKEEPING.contains(&name.as_str()) {
                continue;
            }
            if control.get(name) != Some(value) {
                strays.push(format!("{name}={value}"));
            }
        }
        for name in control.keys() {
            if known.contains(&name.as_str()) || SHELL_BOOKKEEPING.contains(&name.as_str()) {
                continue;
            }
            if !seen.contains_key(name) {
                strays.push(format!("{name} (removed)"));
            }
        }
        assert!(
            strays.is_empty(),
            "with {extra:?}, `storyhook_isolate` changed {strays:?}, which \
             `story help test-environment` does not mention. A harness that \
             does more than the published contract is a harness whose users \
             cannot reproduce it."
        );
    }
}

/// Names `bash` itself maintains, which are not the isolating function's doing.
///
/// A deliberately tiny list of *shell* internals rather than of storyhook
/// variables — nothing here is a parameter, or could become one.
const SHELL_BOOKKEEPING: [&str; 4] = ["_", "SHLVL", "PWD", "OLDPWD"];

/// The shell rendering creates the directories the library says it needs.
#[test]
fn the_shell_rendering_creates_every_directory_the_library_names() {
    let fixture = scratch_dir();
    let root = fixture.path();
    let (_, _) = isolate_in_bash(root, &["--home"]);
    for dir in storyhook::env::test_environment::directories(root) {
        assert!(
            dir.is_dir(),
            "{} was not created; a `story` pointed at this environment would \
             have to guess",
            dir.display()
        );
    }
}

/// A root that is not disposable is refused rather than isolated.
///
/// The one refusal the shared function owns, and the reason three of the six
/// harnesses used to carry a copy of it and three did not.
#[test]
fn the_shell_rendering_refuses_a_root_that_is_not_disposable() {
    let script = format!(
        ". \"{}/scripts/test-env.sh\"\nstoryhook_isolate \"$HOME/not-disposable\"\necho REACHED\n",
        repo_root().display(),
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("running bash");
    assert!(!out.status.success(), "a real home must be refused");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("REACHED"),
        "the refusal must END the caller, not return a status it can ignore: a \
         caller that carried on would carry on UNISOLATED, which is the exact \
         outcome the refusal exists to prevent. Saw:\n{combined}"
    );
    assert!(
        combined.contains("refusing to isolate"),
        "the refusal must say what it refused; saw:\n{combined}"
    );
}

/// An option that lands nowhere is refused, not dropped.
///
/// The SH-357 doctrine in shell: a misspelled `--home` that silently isolated
/// less than the caller asked for would be invisible in every observable
/// except the damage.
#[test]
fn the_shell_rendering_refuses_an_unknown_option() {
    let script = format!(
        ". \"{}/scripts/test-env.sh\"\nstoryhook_isolate --hoem /private/tmp/x\necho REACHED\n",
        repo_root().display(),
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("running bash");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--hoem"),
        "the refusal must name the offending word; saw:\n{stderr}"
    );
}

/// `storyhook_isolate_print` prints what `storyhook_isolate` does.
///
/// The printer exists for `scripts/scratch-env.sh --print`, and a printer that
/// claimed one thing while the function did another would be a third rendering
/// of the same table. Proven by *evaluating* what it prints and comparing the
/// result to the function's own, rather than by reading it.
#[test]
fn the_printed_environment_is_the_one_the_function_applies() {
    let fixture = scratch_dir();
    let root = fixture.path();

    let script = format!(
        ". \"{}/scripts/test-env.sh\"\neval \"$(storyhook_isolate_print --home --parent-pid 4242 \"{}\")\"\nexec /usr/bin/env\n",
        repo_root().display(),
        root.display(),
    );
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .envs(poison())
        .output()
        .expect("running bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen: BTreeMap<String, String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();

    for setting in resolve(root, 4242, Scope::StoryhookProcessOnly) {
        match setting.value {
            Some(value) => assert_eq!(
                seen.get(setting.name).map(String::as_str),
                Some(value.to_str().expect("a UTF-8 fixture path")),
                "the printed form disagrees with the library about {}",
                setting.name
            ),
            None => assert!(
                !seen.contains_key(setting.name),
                "the printed form left {} behind",
                setting.name
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Nothing builds a storyhook environment by hand
// ---------------------------------------------------------------------------

/// A tracked shell script, with its text.
fn tracked_shell_scripts() -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(["ls-files", "-z", "--", "*.sh"])
        .output()
        .expect("listing this repository's tracked shell scripts");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(repo_root().join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// The parameters a script sets or removes by hand, by name.
fn parameters_set_by_hand(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        for parameter in TEST_ENVIRONMENT {
            let name = parameter.name;
            let sets = trimmed.starts_with(&format!("export {name}="));
            let removes = trimmed
                .strip_prefix("unset ")
                .is_some_and(|rest| rest.split_whitespace().any(|word| word == name));
            if (sets || removes) && !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// Whether a script *calls* the shared isolation (as opposed to defining it).
fn calls_the_shared_isolation(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        ["storyhook_isolate ", "storyhook_isolate_print "]
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
    })
}

/// Whether a script *is* the shared isolation.
///
/// Derived from the shape of a shell function definition rather than from the
/// file's name, so moving or renaming the implementation needs no edit here.
fn defines_the_shared_isolation(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("storyhook_isolate()") || line.starts_with("storyhook_isolate_print()")
    })
}

/// A script that constructs a storyhook environment uses the shared one.
///
/// **Two or more parameters, not one.** Setting a single parameter is
/// *pointing* a run at something — `scripts/merge-watch.sh` names a store for a
/// real, deliberate, non-isolated run, and should not be dragged into this.
/// Setting two or more is building an environment, and building one by hand is
/// exactly what produced six copies that had already drifted: three carried a
/// path guard and three did not, one used a sentinel pid, and only the Rust
/// harness cleared the developer's real credential.
#[test]
fn every_shell_script_that_builds_an_environment_uses_the_shared_one() {
    let scripts = tracked_shell_scripts();
    let offenders: Vec<String> = scripts
        .iter()
        .filter(|(_, text)| !defines_the_shared_isolation(text))
        .filter(|(_, text)| !calls_the_shared_isolation(text))
        .filter_map(|(relative, text)| {
            let by_hand = parameters_set_by_hand(text);
            (by_hand.len() >= 2).then(|| format!("{relative} sets {by_hand:?}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} build a storyhook environment by hand. Source \
         `scripts/test-env.sh` and call `storyhook_isolate` instead — it is one \
         line, it carries the disposable-root refusal, and it cannot fall \
         behind `story help test-environment` the way a seventh copy would."
    );
}

/// …and a script that uses the shared one does not also set parameters itself.
///
/// The other half, and the one that catches a partial migration: a harness that
/// calls the function and then "helpfully" re-exports one variable has quietly
/// reintroduced a private opinion about the contract, in the one place a reader
/// would not look for it.
#[test]
fn no_script_that_uses_the_shared_isolation_also_sets_a_parameter() {
    let scripts = tracked_shell_scripts();
    let offenders: Vec<String> = scripts
        .iter()
        .filter(|(_, text)| calls_the_shared_isolation(text))
        .filter_map(|(relative, text)| {
            let by_hand = parameters_set_by_hand(text);
            (!by_hand.is_empty()).then(|| format!("{relative} also sets {by_hand:?}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?}. A harness on the shared isolation states no opinion of \
         its own about these; if the shared one is wrong, change it there and \
         let `tests/test_environment.rs` prove the library agrees."
    );
}

/// The two scans above are only worth anything if they find the harnesses.
///
/// A control in the SH-131 shape: a pattern that matched nothing would pass
/// both of them forever, and a clean tree and a broken scan would be
/// indistinguishable.
#[test]
fn the_harness_scan_finds_the_harnesses() {
    let scripts = tracked_shell_scripts();
    let users: Vec<&str> = scripts
        .iter()
        .filter(|(_, text)| calls_the_shared_isolation(text))
        .map(|(relative, _)| relative.as_str())
        .collect();
    assert!(
        users.len() >= 5,
        "this scan is supposed to find every shell harness that isolates a run, \
         and it found {}: {users:?}. The pattern is broken, not the harnesses.",
        users.len()
    );

    let definers: Vec<&str> = scripts
        .iter()
        .filter(|(_, text)| defines_the_shared_isolation(text))
        .map(|(relative, _)| relative.as_str())
        .collect();
    assert_eq!(
        definers.len(),
        1,
        "there must be exactly one implementation of the shared isolation, and \
         the scan must be able to see it; found {definers:?}"
    );

    // The by-hand detector, provoked rather than assumed: a parser that stopped
    // recognising an `export` would report every harness compliant.
    let planted = format!(
        "export {}=/somewhere\nunset {}\n",
        TEST_ENVIRONMENT[0].name, TEST_ENVIRONMENT[1].name
    );
    assert_eq!(
        parameters_set_by_hand(&planted),
        [TEST_ENVIRONMENT[0].name, TEST_ENVIRONMENT[1].name],
        "the by-hand detector cannot see a hand-set parameter, so its silence \
         proves nothing"
    );
}

/// Unused-import guard for the one type only the scans above need.
#[allow(dead_code)]
fn _pathbuf_is_used(_: PathBuf) {}
