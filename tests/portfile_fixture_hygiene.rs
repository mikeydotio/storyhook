//! A fence around the daemon-portfile-doctoring fixture class (SH-345).
//!
//! `tests/daemon_lifecycle.rs::a_daemon_from_another_build_is_replaced_rather_than_reused`
//! doctored a *live* daemon's portfile and needed the doctoring to hold still
//! until the next `daemon start` read it. It didn't always: the daemon's own
//! background tailnet probe (`serve::tailnet_reprobe`) rewrites the portfile
//! with its own correct `exe_mtime` the instant it binds (SH-186), and on a
//! tailnet-equipped machine that happens shortly after nearly every daemon
//! starts. Under load, that rewrite can land between the test's doctored
//! write and the next command's read of it, silently undoing the corruption
//! and making the daemon look current again — so the intended replace never
//! happens, which is exactly what SH-345 reported. The fix starts the daemon
//! through `storyhook_test_support::path_without_tailscale`, which denies
//! `tailscale` outright so the probe can never succeed and never rewrites
//! anything. `tests/web_test.rs`'s
//! `web_open_falls_back_to_the_bare_url_when_arming_fails` had already hit and
//! fixed the identical hazard once, against a different doctored field.
//!
//! This is the fence that keeps a *third* site from reintroducing it
//! silently. A file-level scan — "does this file mention `daemon_file()` and
//! also mention `path_without_tailscale`, anywhere" — was considered and
//! rejected (the council verdict on SH-345, `story show SH-345`): the only
//! two files that have ever doctored a portfile are already permanently
//! "compliant" by that measure from the SH-345 fix itself, so a file-level
//! fence could only ever catch a hypothetical third *file* — near-zero real
//! coverage while reading as protection.
//!
//! # The rule
//!
//! Per function: **W** is the byte offset of the first write to a path bound
//! from `daemon_file()`; **S** is the byte offset of the first call that
//! starts a `story` daemon (`env.story(`, `.raw_story(`, `spawn_daemon(`, or
//! this suite's own `start(&env)` helper). A function with a W is a portfile
//! writer. One where `S < W` is HAZARDOUS — a daemon is already running when
//! the write happens, so its background probe is a live threat — and must
//! also contain `path_without_tailscale` in the same function.
//!
//! No allowlist, and none needed: the two sites that hand-write a portfile
//! with no live daemon involved (`a_portfile_without_a_daemon_does_not_stop_
//! one_starting`, whose own `daemon start` comes *after* its write, and
//! `wedge_the_daemon`, which never starts one at all) are immune by
//! construction and this rule reads them that way without being told.
//!
//! # Known blind spot, and why it fails safe
//!
//! A portfile write performed inside a *called* helper, whose own caller
//! started the daemon, is attributed to the helper's body — where no
//! daemon-start call appears — so it reads as immune. That is a false
//! negative, never a false alarm: this fence can miss a hazardous write
//! reached through indirection, but it will not block a legitimate one.

use std::path::Path;

/// Every tracked Rust source this fence scans, paired with its contents.
fn scanned_files(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--",
            "tests/*.rs",
            "crates/storyhook-test-support/src/*.rs",
        ])
        .output()
        .expect("listing this repository's tracked test sources");
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
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// Whether `line`, once its leading whitespace is trimmed, opens a function.
fn opens_a_function(line: &str) -> bool {
    let trimmed = line.trim_start();
    [
        "pub(crate) async fn ",
        "pub async fn ",
        "async fn ",
        "pub(crate) fn ",
        "pub fn ",
        "fn ",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

/// Splits `text` into function bodies: from each line that opens a function
/// to the line before the next one (or EOF). Textual, like every other fence
/// in this suite — it does not track brace nesting.
fn function_blocks(text: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if opens_a_function(line) {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(text.len());
            &text[start..end]
        })
        .collect()
}

/// Every identifier `block` binds to an expression naming `daemon_file()` —
/// `let portfile = env.environment().daemon_file();` and its siblings.
fn portfile_bindings(block: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in block.match_indices("let ") {
        let rest = &block[at + "let ".len()..];
        let Some(eq) = rest.find('=') else { continue };
        let name = rest[..eq].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let Some(semi) = rest[eq + 1..].find(';') else {
            continue;
        };
        let expr = &rest[eq + 1..eq + 1 + semi];
        if expr.contains("daemon_file()") {
            names.push(name.to_string());
        }
    }
    names
}

/// Whether `target` names `ident` as a whole identifier — `&path` and
/// `path.clone()` both count; `no_tailscale_path` must not match `path`.
fn target_names_ident(target: &str, ident: &str) -> bool {
    target.match_indices(ident).any(|(at, _)| {
        let before_ok = target[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let after = at + ident.len();
        let after_ok = target[after..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        before_ok && after_ok
    })
}

/// The byte offset of the first `fs::write(` call in `block` whose target
/// names the portfile — the literal `daemon_file()` or one of `bindings`.
fn first_portfile_write(block: &str, bindings: &[String]) -> Option<usize> {
    for (at, _) in block.match_indices("fs::write(") {
        let call_start = at + "fs::write(".len();
        let window_end = (call_start + 200).min(block.len());
        let window = &block[call_start..window_end];
        let arg_end = window.find(',').unwrap_or(window.len());
        let target = &window[..arg_end];
        if target.contains("daemon_file()")
            || bindings.iter().any(|name| target_names_ident(target, name))
        {
            return Some(at);
        }
    }
    None
}

/// The byte offset of the first call in `block` that starts a `story` daemon.
fn first_daemon_start(block: &str) -> Option<usize> {
    ["env.story(", ".raw_story(", "spawn_daemon(", "start(&env)"]
        .iter()
        .filter_map(|needle| block.find(needle))
        .min()
}

/// One write site this fence found, and what it makes of it.
#[derive(Debug)]
struct WriteSite {
    /// `file:line` of the write, for a human-readable failure message.
    location: String,
    /// Whether a daemon was already started, in the same function, before
    /// this write.
    hazardous: bool,
    /// Whether the function also mentions `path_without_tailscale`.
    guarded: bool,
}

/// Every portfile-write site `classify` finds in `text` (from `file`).
fn classify(file: &str, text: &str) -> Vec<WriteSite> {
    let mut sites = Vec::new();
    for block in function_blocks(text) {
        // Safe: `block` is always a subslice of `text`, returned by
        // `function_blocks(text)` a few lines above.
        let block_start = block.as_ptr() as usize - text.as_ptr() as usize;
        let bindings = portfile_bindings(block);
        let Some(write_at) = first_portfile_write(block, &bindings) else {
            continue;
        };
        let hazardous = first_daemon_start(block).is_some_and(|start_at| start_at < write_at);
        let guarded = block.contains("path_without_tailscale");
        let line = text[..block_start + write_at].matches('\n').count() + 1;
        sites.push(WriteSite {
            location: format!("{file}:{line}"),
            hazardous,
            guarded,
        });
    }
    sites
}

/// Every portfile write that happens after a daemon has been started, in the
/// same function, runs with `path_without_tailscale`.
#[test]
fn every_write_after_a_daemon_start_is_guarded_against_the_daemons_own_rewrite() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = scanned_files(root);

    let sites: Vec<WriteSite> = files
        .iter()
        .flat_map(|(file, text)| classify(file, text))
        .collect();

    // A scan that matches nothing passes every assertion built on top of it.
    assert!(
        sites.len() >= 4,
        "expected at least 4 tracked portfile-write sites, found {}: {:?}. Either the \
         suite shrank or this scan's `fs::write(` pattern no longer matches how a \
         portfile is written.",
        sites.len(),
        sites.iter().map(|s| &s.location).collect::<Vec<_>>()
    );

    let hazardous: Vec<&WriteSite> = sites.iter().filter(|s| s.hazardous).collect();
    assert!(
        hazardous.len() >= 2,
        "expected at least 2 write sites where a daemon is already running when the \
         write happens (SH-345's own test and web_test.rs's), found {}: either they \
         were removed or this scan's daemon-start patterns no longer match how one is \
         started.",
        hazardous.len()
    );

    let unguarded: Vec<&str> = hazardous
        .iter()
        .filter(|s| !s.guarded)
        .map(|s| s.location.as_str())
        .collect();
    assert!(
        unguarded.is_empty(),
        "{unguarded:?} write to a live daemon's portfile without `path_without_tailscale`. \
         A running daemon's own background tailnet probe (`serve::tailnet_reprobe`) \
         rewrites that same file with its own correct `exe_mtime` the instant it binds \
         (SH-186), and on a tailnet-equipped machine under load that rewrite can land \
         between this write and the next command reading it — silently undoing the \
         doctoring (SH-345). Start the daemon through \
         `storyhook_test_support::path_without_tailscale` before doctoring its portfile."
    );
}

#[test]
fn classifier_reads_the_shapes_the_suite_actually_writes() {
    // Hazardous and guarded — SH-345's own fix.
    let version_skew = r#"
fn a_daemon_from_another_build_is_replaced_rather_than_reused() {
    let env = TestEnv::isolated();
    let (_no_tailscale, no_tailscale_path) = path_without_tailscale(&env);
    env.story(dir.path())
        .env("PATH", &no_tailscale_path)
        .args(["daemon", "start"])
        .assert()
        .success();
    let path = env.environment().daemon_file();
    std::fs::write(&path, serde_json::to_string_pretty(&stale).unwrap())
        .expect("rewriting the portfile");
}
"#;
    let sites = classify("fixture.rs", version_skew);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert!(sites[0].hazardous, "{sites:?}");
    assert!(sites[0].guarded, "{sites:?}");

    // Hazardous and guarded — web_test.rs's arming test.
    let arming = r#"
fn web_open_falls_back_to_the_bare_url_when_arming_fails() {
    let (_no_tailscale, path) = path_without_tailscale(&env);
    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .assert()
        .success();
    let portfile = env.environment().daemon_file();
    let real = std::fs::read_to_string(&portfile).expect("reading the portfile");
    std::fs::write(&portfile, info.to_string()).expect("rewriting the portfile");
    std::fs::write(&portfile, &real).expect("restoring the portfile");
}
"#;
    let sites = classify("fixture.rs", arming);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert!(sites[0].hazardous, "{sites:?}");
    assert!(sites[0].guarded, "{sites:?}");

    // Immune: the write comes before any daemon is started.
    let orphan_portfile = r#"
fn a_portfile_without_a_daemon_does_not_stop_one_starting() {
    let environment = env.environment();
    std::fs::write(
        environment.daemon_file(),
        serde_json::to_string_pretty(&orphan).unwrap(),
    )
    .unwrap();
    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
}
"#;
    let sites = classify("fixture.rs", orphan_portfile);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert!(!sites[0].hazardous, "{sites:?}");

    // Immune: no daemon is ever started in this block at all.
    let wedge = r#"
fn wedge_the_daemon(env: &TestEnv) -> std::fs::File {
    let environment = env.environment();
    std::fs::write(
        environment.daemon_file(),
        serde_json::to_string(&wedged).expect("serializing the portfile"),
    )
    .expect("writing the portfile");
    pidfile
}
"#;
    let sites = classify("fixture.rs", wedge);
    assert_eq!(sites.len(), 1, "{sites:?}");
    assert!(!sites[0].hazardous, "{sites:?}");

    // A read-only helper never counts as a write site at all.
    let read_only = r#"
fn await_daemon(env: &TestEnv) {
    let portfile = env.environment().daemon_file();
    if storyhook::daemon::lifecycle::read_info_at(&portfile).is_some() {
        return;
    }
}
"#;
    assert!(
        classify("fixture.rs", read_only).is_empty(),
        "a read-only helper must never be counted as a write site"
    );
}
