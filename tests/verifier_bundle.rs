//! The verifier script family ships inside the binary and is complete (SH-654).
//!
//! `src/daemon/verification.rs` used to spawn `scripts/verify-pr.sh` relative
//! to the registered project's checkout, so only a storyhook checkout could be
//! verified. `build.rs` now embeds the family (`VERIFIER_SCRIPTS`) and
//! `storyhook::daemon::verifier_bundle` projects it under the daemon's own
//! state directory. Three things have to stay true for that to be a fix rather
//! than a relocation of the same defect, and each is derived from the scripts
//! themselves rather than from a second list (the SH-136 rule; SH-198, SH-258,
//! SH-260/276 and SH-364 are the precedents for why a hand-kept list here
//! would be the defect's next home):
//!
//! 1. **The bundle is the tracked scripts.** Every embedded file is
//!    byte-identical to `scripts/<name>` with the same executable bit.
//! 2. **The bundle is closed.** Every sibling a bundled script reaches
//!    through its own directory — `$script_dir/NAME`,
//!    `"$(dirname "${BASH_SOURCE[0]}")/NAME"`, a Python `from NAME import` —
//!    is itself bundled. A name missing from `VERIFIER_SCRIPTS` fails here
//!    by name, with a positive control proving the reference scanner sees
//!    the shapes it claims to.
//! 3. **No bundled script reaches a sibling through the checkout.** The
//!    leaks this story closed were `$root/scripts/…` and bare `scripts/…`
//!    invocations; the fence reads the scripts with comments stripped
//!    (`tests/dashboard_focus_coverage.rs`'s lesson — prose above a rule
//!    routinely spells the forbidden shape while explaining it).
//!
//! The materializer half proves the projection against a fixture
//! [`Environment`]: the leaf lives under `daemon_state_dir()/verifier/`,
//! is named by the payload digest, carries the executable bits, is reused
//! untouched on a second call, is repaired when damaged, and survives a
//! concurrent pair of callers; a stale sibling leaf is swept.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use storyhook::daemon::verifier_bundle::{self, VERIFY_SCRIPT};
use storyhook::env::Environment;
use storyhook_test_support::scratch_dir;

/// The checkout under test — where the tracked scripts live.
fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn bundled_names() -> BTreeSet<&'static str> {
    verifier_bundle::files().map(|(name, _, _)| name).collect()
}

#[test]
fn every_embedded_file_is_the_tracked_script_byte_for_byte_with_its_executable_bit() {
    let mut seen = 0;
    for (name, executable, bytes) in verifier_bundle::files() {
        seen += 1;
        let tracked = scripts_dir().join(name);
        let on_disk = fs::read(&tracked)
            .unwrap_or_else(|e| panic!("{} is embedded but not tracked: {e}", tracked.display()));
        assert!(
            on_disk == bytes,
            "embedded {name} differs from the tracked script; rebuild (build.rs embeds at compile time)"
        );
        let mode = fs::metadata(&tracked).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o111 != 0,
            executable,
            "embedded executable bit for {name} disagrees with the tracked file"
        );
    }
    assert!(
        bundled_names().contains(VERIFY_SCRIPT),
        "the bundle must carry its own entry point"
    );
    assert!(
        seen >= 2,
        "a bundle of {seen} file(s) cannot be the verifier family"
    );
}

/// Every sibling `body` reaches through its own directory, by the shapes the
/// family actually uses. A Python `from X import …` names a sibling only when
/// `scripts/X.py` is tracked — `pathlib` is the standard library, and the
/// fence's question is whether a sibling that exists was left out of the
/// bundle, not whether Python can import its own library.
fn sibling_references(name: &str, body: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    if name.ends_with(".py") {
        for line in body.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("from ")
                && let Some((module, _)) = rest.split_once(" import ")
                && !module.contains('.')
            {
                let sibling = format!("{}.py", module.trim());
                if scripts_dir().join(&sibling).is_file() {
                    found.insert(sibling);
                }
            }
        }
        return found;
    }
    for prefix in ["$script_dir/", "$(dirname \"${BASH_SOURCE[0]}\")/"] {
        for (index, _) in body.match_indices(prefix) {
            let rest = &body[index + prefix.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
                .unwrap_or(rest.len());
            let referenced = &rest[..end];
            if !referenced.is_empty() {
                found.insert(referenced.to_string());
            }
        }
    }
    found
}

#[test]
fn every_sibling_a_bundled_script_reaches_through_its_own_directory_is_bundled() {
    let bundled = bundled_names();
    let mut references_seen = 0;
    let mut missing = Vec::new();
    for (name, _, bytes) in verifier_bundle::files() {
        let body = String::from_utf8_lossy(bytes);
        for referenced in sibling_references(name, &strip_comments(&body)) {
            references_seen += 1;
            if !bundled.contains(referenced.as_str()) {
                missing.push(format!("{name} reaches {referenced}"));
            }
        }
    }
    assert!(
        references_seen > 0,
        "the reference scanner found no sibling references at all — it is broken, not the tree"
    );
    assert!(
        missing.is_empty(),
        "a bundled script reaches a sibling that build.rs's VERIFIER_SCRIPTS does not ship; \
         the daemon would fail by name at runtime against any non-storyhook checkout:\n{}",
        missing.join("\n")
    );
}

#[test]
fn the_reference_scanner_sees_every_shape_it_claims_to() {
    let shell = r#"x="$(bash "$script_dir/alpha.sh")"
. "$(dirname "${BASH_SOURCE[0]}")/beta-two.sh" || exit 1
python3 "$script_dir/gamma_3.py" <"$log""#;
    let found = sibling_references("probe.sh", shell);
    assert_eq!(
        found,
        ["alpha.sh", "beta-two.sh", "gamma_3.py"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let python = "import json\nfrom test_output import TestOutputParser\nfrom pathlib import Path\nfrom os.path import join\n";
    assert_eq!(
        sibling_references("probe.py", python),
        ["test_output.py".to_string()].into_iter().collect(),
        "a tracked sibling is a reference; the standard library and dotted imports are not"
    );
    assert!(sibling_references("probe.sh", "echo nothing here").is_empty());
}

/// Drops `#` comments (whole-line and trailing) so prose explaining a rule
/// cannot trip the rule. Crude on purpose: a `#` inside a quoted string is
/// rare in this family, and erring toward stripping only ever hides a
/// *forbidden* shape inside a string — which the positive control below
/// shows is not how the family spells its real references.
fn strip_comments(body: &str) -> String {
    body.lines()
        .map(|line| match line.find('#') {
            Some(0) => "",
            Some(index) if line[..index].trim().is_empty() => "",
            Some(index) if line.as_bytes()[index - 1].is_ascii_whitespace() => &line[..index],
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The shapes that reached a sibling through the checkout before SH-654.
const CHECKOUT_RELATIVE: &[&str] = &[
    "$root/scripts/",
    "\"$root\"/scripts/",
    "bash scripts/",
    ". scripts/",
    "source scripts/",
    "python3 scripts/",
];

#[test]
fn no_bundled_script_reaches_a_sibling_through_the_checkout() {
    let mut hits = Vec::new();
    for (name, _, bytes) in verifier_bundle::files() {
        let body = strip_comments(&String::from_utf8_lossy(bytes));
        for (number, line) in body.lines().enumerate() {
            for shape in CHECKOUT_RELATIVE {
                if line.contains(shape) {
                    hits.push(format!("{name}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a bundled script reaches a sibling through the project checkout, which need not be \
         storyhook's and need not hold a scripts/ tree at all:\n{}",
        hits.join("\n")
    );
    // Positive control: the fence sees the shape it forbids.
    let probe =
        strip_comments("# . \"$root/scripts/x.sh\" is fine in prose\n. \"$root/scripts/x.sh\"\n");
    assert!(
        CHECKOUT_RELATIVE.iter().any(|shape| probe.contains(shape)),
        "the fence cannot see its own forbidden shape"
    );
    assert!(
        !strip_comments("# . \"$root/scripts/x.sh\"\n").contains("$root/scripts/"),
        "prose must be stripped before the fence reads a line"
    );
}

fn fixture_env() -> (tempfile::TempDir, Environment) {
    let root = scratch_dir();
    let env = Environment::at(root.path());
    (root, env)
}

#[test]
fn materialize_projects_the_bundle_under_the_daemon_state_dir_with_executable_bits() {
    let (_root, env) = fixture_env();
    let dir = verifier_bundle::materialize(&env).expect("materialize");
    assert_eq!(dir, verifier_bundle::bundle_dir(&env));
    assert_eq!(
        dir.parent().unwrap(),
        env.daemon_state_dir().join("verifier"),
        "the leaf lives directly under the store-keyed daemon state dir"
    );
    assert_eq!(
        verifier_bundle::verify_script(&env).unwrap(),
        dir.join(VERIFY_SCRIPT)
    );
    let leaf = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        leaf.len() == 16 && leaf.bytes().all(|b| b.is_ascii_hexdigit()),
        "the leaf is named by the payload digest, got {leaf}"
    );
    for (name, executable, bytes) in verifier_bundle::files() {
        let path = dir.join(name);
        assert_eq!(fs::read(&path).unwrap(), bytes, "{name}");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111 != 0, executable, "{name} executable bit");
    }
    let on_disk: BTreeSet<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let expected: BTreeSet<String> = bundled_names().into_iter().map(str::to_string).collect();
    assert_eq!(on_disk, expected, "the leaf holds exactly the bundle");
}

#[test]
fn a_second_materialization_reuses_the_leaf_untouched_and_repairs_damage() {
    let (_root, env) = fixture_env();
    let dir = verifier_bundle::materialize(&env).unwrap();
    let script = dir.join(VERIFY_SCRIPT);
    let before = fs::metadata(&script).unwrap().ino();
    assert_eq!(verifier_bundle::materialize(&env).unwrap(), dir);
    assert_eq!(
        fs::metadata(&script).unwrap().ino(),
        before,
        "an exact leaf is reused, never rewritten"
    );

    fs::write(&script, "damaged\n").unwrap();
    fs::write(dir.join("stray.txt"), "not part of the bundle\n").unwrap();
    assert_eq!(verifier_bundle::materialize(&env).unwrap(), dir);
    let (_, _, expected) = verifier_bundle::files()
        .find(|(name, _, _)| *name == VERIFY_SCRIPT)
        .unwrap();
    assert_eq!(
        fs::read(&script).unwrap(),
        expected,
        "damage is repaired from the binary"
    );
    assert!(
        !dir.join("stray.txt").exists(),
        "a stray file is gone with the replaced leaf"
    );
}

#[test]
fn a_stale_leaf_from_another_build_is_swept_and_the_lock_is_not() {
    let (_root, env) = fixture_env();
    let root = verifier_bundle::bundle_root(&env);
    let stale = root.join("0123456789abcdef");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join(VERIFY_SCRIPT), "old build\n").unwrap();
    let dir = verifier_bundle::materialize(&env).unwrap();
    assert!(
        !stale.exists(),
        "a sibling leaf from another build is swept"
    );
    assert!(dir.is_dir());
    assert!(
        root.join(".materialize.lock").is_file(),
        "the materializer's lock survives the sweep"
    );
}

#[test]
fn concurrent_materializations_leave_one_intact_leaf() {
    let (_root, env) = fixture_env();
    let dirs: Vec<PathBuf> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let env = env.clone();
                scope.spawn(move || verifier_bundle::materialize(&env).expect("materialize"))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(dirs.iter().all(|d| *d == dirs[0]));
    for (name, _, bytes) in verifier_bundle::files() {
        assert_eq!(fs::read(dirs[0].join(name)).unwrap(), bytes, "{name}");
    }
    let leftovers: Vec<_> = fs::read_dir(verifier_bundle::bundle_root(&env))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".staging-") || n.starts_with(".previous-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging directories were not cleaned up: {leftovers:?}"
    );
}
