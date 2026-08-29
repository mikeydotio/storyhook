//! A fence around the daemon-portfile-mutation fixture class (SH-345, SH-417).
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
//! SH-417 found the same race in a third file through a different mutation:
//! `tests/store_isolation.rs` renamed a live daemon's keyed portfile away, and
//! the background probe could recreate it before the next command looked.
//! Removing the file was no safer than rewriting it.
//!
//! This fence works per function. A file-level scan — "does this file mention
//! a portfile and also mention `path_without_tailscale`, anywhere" — was
//! rejected by the council verdict on SH-345 (`story show SH-345`) because the
//! two files then known were already permanently "compliant." SH-417 became
//! the hypothetical third file that coarse rule could have caught once, but
//! after this fix it too is permanently compliant at file level. Only the
//! per-function rule can catch the next unguarded mutation in any of them.
//!
//! # The rule
//!
//! Per function: **M** is the byte offset of the first mutation of a path
//! bound from `daemon_file()` or `join("daemon.json")`; **S** is the byte
//! offset of the first call that starts a `story` daemon (`.story(`,
//! `.raw_story(`, `spawn_daemon(`, or
//! this suite's own `start(&env)` helper). A function with an M is a portfile
//! mutator. One where `S < M` is HAZARDOUS — a daemon is already running when
//! the mutation happens, so its background probe is a live threat — and must
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
//! A portfile mutation performed inside a *called* helper, whose own caller
//! started the daemon, is attributed to the helper's body — where no
//! daemon-start call appears — so it reads as immune. That is a false
//! negative, never a false alarm: this fence can miss a hazardous mutation
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

/// Whether `expression` names a portfile directly.
fn names_portfile_expression(expression: &str) -> bool {
    expression.contains("daemon_file()") || expression.contains("join(\"daemon.json\")")
}

/// Every identifier `block` binds to an expression naming a portfile —
/// `let portfile = env.environment().daemon_file();`,
/// `let keyed = daemon_dir.join("daemon.json");`, and their siblings.
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
        if names_portfile_expression(expr) {
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

/// The arguments of a function call, where `call` begins immediately after
/// its opening parenthesis. Nested calls and indexing may contain commas of
/// their own, so only separators at the outer call's depth count.
fn call_arguments(call: &str) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (at, character) in call.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                arguments.push(&call[start..at]);
                return arguments;
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments.push(&call[start..at]);
                start = at + character.len_utf8();
            }
            _ => {}
        }
    }
    arguments
}

/// Whether `target` names the portfile — directly or through one of
/// `bindings`.
fn target_names_portfile(target: &str, bindings: &[String]) -> bool {
    names_portfile_expression(target)
        || bindings.iter().any(|name| target_names_ident(target, name))
}

/// The byte offset of the first portfile mutation in `block`.
///
/// A write and remove mutate their first argument, a rename mutates both the
/// path it removes and the path it replaces, and a copy mutates only its
/// destination. Copying *from* a portfile is read-only and does not count.
fn first_portfile_mutation(block: &str, bindings: &[String]) -> Option<usize> {
    let operations: [(&str, &[usize]); 4] = [
        ("fs::write(", &[0]),
        ("fs::remove_file(", &[0]),
        ("fs::rename(", &[0, 1]),
        ("fs::copy(", &[1]),
    ];
    let mut first = None;
    for (call, targets) in operations {
        for (at, _) in block.match_indices(call) {
            let arguments = call_arguments(&block[at + call.len()..]);
            if targets.iter().any(|target| {
                arguments
                    .get(*target)
                    .is_some_and(|argument| target_names_portfile(argument, bindings))
            }) {
                first = Some(first.map_or(at, |previous: usize| previous.min(at)));
            }
        }
    }
    first
}

/// The byte offset of the first call in `block` that starts a `story` daemon.
fn first_daemon_start(block: &str) -> Option<usize> {
    [".story(", ".raw_story(", "spawn_daemon(", "start(&env)"]
        .iter()
        .filter_map(|needle| block.find(needle))
        .min()
}

/// One mutation site this fence found, and what it makes of it.
#[derive(Debug)]
struct MutationSite {
    /// `file:line` of the mutation, for a human-readable failure message.
    location: String,
    /// Whether a daemon was already started, in the same function, before
    /// this mutation.
    hazardous: bool,
    /// Whether the function also mentions `path_without_tailscale`.
    guarded: bool,
}

/// Every portfile-mutation site `classify` finds in `text` (from `file`).
fn classify(file: &str, text: &str) -> Vec<MutationSite> {
    let mut sites = Vec::new();
    for block in function_blocks(text) {
        // Safe: `block` is always a subslice of `text`, returned by
        // `function_blocks(text)` a few lines above.
        let block_start = block.as_ptr() as usize - text.as_ptr() as usize;
        let bindings = portfile_bindings(block);
        let Some(mutation_at) = first_portfile_mutation(block, &bindings) else {
            continue;
        };
        let hazardous = first_daemon_start(block).is_some_and(|start_at| start_at < mutation_at);
        let guarded = block.contains("path_without_tailscale");
        let line = text[..block_start + mutation_at].matches('\n').count() + 1;
        sites.push(MutationSite {
            location: format!("{file}:{line}"),
            hazardous,
            guarded,
        });
    }
    sites
}

/// Every portfile mutation that happens after a daemon has been started, in the
/// same function, runs with `path_without_tailscale`.
#[test]
fn every_mutation_after_a_daemon_start_is_guarded_against_the_daemons_own_rewrite() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = scanned_files(root);

    let sites: Vec<MutationSite> = files
        .iter()
        .flat_map(|(file, text)| classify(file, text))
        .collect();

    // A scan that matches nothing passes every assertion built on top of it.
    assert!(
        sites.len() >= 5,
        "expected at least 5 tracked portfile-mutation sites, found {}: {:?}. Either the \
         suite shrank or this scan's mutation patterns no longer match how a portfile \
         is changed.",
        sites.len(),
        sites.iter().map(|s| &s.location).collect::<Vec<_>>()
    );

    let hazardous: Vec<&MutationSite> = sites.iter().filter(|s| s.hazardous).collect();
    assert!(
        hazardous.len() >= 3,
        "expected at least 3 mutation sites where a daemon is already running when the \
         mutation happens (SH-345's own test, web_test.rs's, and SH-417's rename), \
         found {}: either they were removed or this scan's daemon-start patterns no \
         longer match how one is started.",
        hazardous.len()
    );

    let unguarded: Vec<&str> = hazardous
        .iter()
        .filter(|s| !s.guarded)
        .map(|s| s.location.as_str())
        .collect();
    assert!(
        unguarded.is_empty(),
        "{unguarded:?} mutation of a live daemon's portfile without `path_without_tailscale`. \
         A running daemon's own background tailnet probe (`serve::tailnet_reprobe`) \
         rewrites that same file with its own correct `exe_mtime` the instant it binds \
         (SH-186), and on a tailnet-equipped machine under load that rewrite can land \
         between this mutation and the next command reading it — silently undoing the \
         fixture setup (SH-345, SH-417). Start the daemon through \
         `storyhook_test_support::path_without_tailscale` before mutating its portfile."
    );
}

#[test]
fn classifier_reads_the_shapes_the_suite_actually_uses() {
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

    // A read-only helper never counts as a mutation site at all.
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
        "a read-only helper must never be counted as a mutation site"
    );

    let mutation_shapes = r#"
fn rename_away_from_the_portfile() {
    let (_guard, path) = path_without_tailscale(&env);
    probe.story(&repo).args(["list"]);
    let keyed = daemon_dir.join("daemon.json");
    std::fs::rename(&keyed, &legacy).unwrap();
}

fn rename_onto_the_portfile() {
    let (_guard, path) = path_without_tailscale(&env);
    probe.story(&repo).args(["list"]);
    let keyed = daemon_dir.join("daemon.json");
    std::fs::rename(&legacy, &keyed).unwrap();
}

fn remove_the_portfile() {
    let (_guard, path) = path_without_tailscale(&env);
    probe.story(&repo).args(["list"]);
    std::fs::remove_file(env.environment().daemon_file()).unwrap();
}

fn copy_onto_the_portfile() {
    let (_guard, path) = path_without_tailscale(&env);
    probe.story(&repo).args(["list"]);
    let keyed = daemon_dir.join("daemon.json");
    std::fs::copy(source_with_nested_call(1, 2), &keyed).unwrap();
}
"#;
    let sites = classify("fixture.rs", mutation_shapes);
    assert_eq!(sites.len(), 4, "{sites:?}");
    assert!(sites.iter().all(|site| site.hazardous), "{sites:?}");
    assert!(sites.iter().all(|site| site.guarded), "{sites:?}");

    // Copying from the portfile does not mutate it; only the destination does.
    let copy_from = r#"
fn archive_the_portfile() {
    probe.story(&repo).args(["list"]);
    let keyed = daemon_dir.join("daemon.json");
    std::fs::copy(&keyed, &backup).unwrap();
}
"#;
    assert!(
        classify("fixture.rs", copy_from).is_empty(),
        "a copy source must not be classified as a mutation"
    );
}
