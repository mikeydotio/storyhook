//! One product, one version (SH-530).
//!
//! The binary, the `VERSION` file and every plugin manifest must agree, because
//! on this machine they did not: the manifests declared
//! `0.6.0+codex.20260823221659` while `Cargo.toml` declared `2.2.0`, and nothing
//! required the two to move together. `tests/plugin_contract.rs` checked that
//! the three manifests agreed with **each other** and never against the crate,
//! so three files agreeing on a stale answer passed.
//!
//! That is not a cosmetic drift. `src/plugin.rs` derives the Codex plugin cache
//! path *from the version string* —
//! `~/.codex/plugins/cache/storyhook/story/<version>/` — so a version that never
//! advances is a cache key that never advances, and every content change
//! silently coalesces onto one directory. SH-406's lesson ("a version string is
//! not a build identity") in a place where the string is load-bearing rather
//! than merely reported.
//!
//! # Derived, never hand-kept
//!
//! The document set comes from `git ls-files`, not from a list in this file. A
//! hand-kept list is precisely how a fourth manifest gets added and never
//! checked, and this project has paid for that shape in SH-136, SH-198, SH-258,
//! SH-260/276, SH-360 and SH-364. The same derivation drives
//! `.semver/hooks/pre-bump/sync-plugin-version.sh`, so the hook that writes and
//! the test that checks cannot disagree about which files are in scope.
//!
//! # The positive control is not optional
//!
//! A scan that stops recognising its input reports a clean tree, which is the
//! failure `tests/dashboard_focus_coverage.rs` was written after. So this file
//! asserts it found documents *and* found version fields before it believes any
//! of them: renaming `.claude-plugin/` fails the run rather than passing it.

use std::path::PathBuf;
use std::process::Command;

/// Every tracked plugin manifest, derived rather than listed.
fn manifests() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("git")
        .current_dir(&root)
        .args([
            "ls-files",
            "--",
            "*.claude-plugin/*.json",
            "*.codex-plugin/*.json",
            ".agents/plugins/*.json",
        ])
        .output()
        .expect("running `git ls-files`");
    assert!(out.status.success(), "`git ls-files` failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| root.join(l))
        .collect()
}

/// Every `"version"` key in a document, at any depth, with a path naming where
/// it was found so a failure points at the field rather than at the file.
fn version_fields(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "version"
                    && let Some(text) = child.as_str()
                {
                    out.push((format!("{path}/{key}"), text.to_string()));
                }
                version_fields(child, &format!("{path}/{key}"), out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                version_fields(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_version_file_agrees_with_the_crate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let declared = std::fs::read_to_string(root.join("VERSION")).expect("reading VERSION");
    assert_eq!(
        declared.trim().trim_start_matches('v'),
        env!("CARGO_PKG_VERSION"),
        "VERSION and Cargo.toml must name the same release; \
         `.semver/hooks/pre-bump/sync-cargo-toml.sh` is what keeps them together"
    );
}

#[test]
fn every_plugin_manifest_declares_the_crate_version() {
    let crate_version = env!("CARGO_PKG_VERSION");
    let manifests = manifests();

    // Positive control: a derivation that found nothing must fail, not pass.
    assert!(
        manifests.len() >= 3,
        "expected at least the three known plugin manifests, found {manifests:?} — \
         if a directory was renamed this scan is looking at nothing and would \
         otherwise report a clean tree"
    );

    let mut checked = 0usize;
    for path in &manifests {
        let text = std::fs::read_to_string(path).expect("reading a manifest");
        let value: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut fields = Vec::new();
        version_fields(&value, "", &mut fields);
        for (field, found) in fields {
            checked += 1;
            assert_eq!(
                found,
                crate_version,
                "{}{field} declares {found:?}, but this crate is {crate_version:?}. \
                 One product, one version: run a release bump, which invokes \
                 `.semver/hooks/pre-bump/sync-plugin-version.sh`, rather than editing \
                 a manifest by hand",
                path.display()
            );
        }
    }

    // The second half of the control: finding the files but no version fields
    // in them is the same blindness one layer down.
    assert!(
        checked >= 3,
        "expected at least three version fields across {} manifests, checked {checked}",
        manifests.len()
    );
}
