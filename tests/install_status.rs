//! `story doctor install` reports the installed set (SH-530).
//!
//! The load-bearing property is that it answers **when the store will not open
//! normally**. That is its own headline — a machine whose store was carried
//! past every release by an unreleased build is exactly the machine that needs
//! to be told so — and a verb that resolved a store before speaking could never
//! deliver it. So the degraded case here is not an edge case; it is the point.

use std::process::Output;

use storyhook_test_support::TestEnv;

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn report_for_codex_source(source: Option<&str>) -> String {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let source = match source {
        Some(version) if version.starts_with("release:") => env
            .data_dir()
            .join("plugins")
            .join(version.trim_start_matches("release:"))
            .display()
            .to_string(),
        Some(source) => source.to_string(),
        None => env
            .data_dir()
            .join("plugins")
            .join(env!("CARGO_PKG_VERSION"))
            .display()
            .to_string(),
    };
    std::fs::create_dir_all(env.home().join(".codex")).unwrap();
    std::fs::write(
        env.home().join(".codex/config.toml"),
        format!("[marketplaces.storyhook]\nsource_type = \"local\"\nsource = \"{source}\"\n"),
    )
    .unwrap();
    text(
        &env.story(project.path())
            .args(["doctor", "install"])
            .output()
            .expect("running `story doctor install`"),
    )
}

fn report_for_claude_source(source: &str) -> String {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let plugins = env.home().join(".claude/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "storyhook": {
                "source": { "source": "directory", "path": source },
                "installLocation": source,
            }
        }))
        .unwrap(),
    )
    .unwrap();
    text(
        &env.story(project.path())
            .args(["doctor", "install"])
            .output()
            .expect("running `story doctor install`"),
    )
}

#[test]
fn codex_multiline_marketplace_source_is_detected() {
    let report = report_for_codex_source(Some("/Volumes/Code/storyhook"));
    assert!(report.contains("CHECKOUT"), "{report}");
}

#[test]
fn claude_nested_marketplace_source_is_detected() {
    let report = report_for_claude_source("/Volumes/Code/storyhook");
    assert!(report.contains("CHECKOUT"), "{report}");
}

#[test]
fn it_reports_the_installed_set_on_an_ordinary_machine() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    assert!(out.status.success(), "{}", text(&out));
    let report = text(&out);
    for row in ["running", "binary", "store", "edit guard"] {
        assert!(
            report.contains(row),
            "the report must name `{row}`:\n{report}"
        );
    }
}

/// The `binary` row used to answer `ok` whenever the `story` on `$PATH` was
/// the one running — which the harness arranges by putting the build
/// directory first on `$PATH`, exactly the SH-630 invocation. The binary under
/// test has never left the directory cargo wrote it into, and the row says so
/// rather than calling it installed.
#[test]
fn the_binary_row_flags_a_binary_still_in_its_build_directory() {
    let env = TestEnv::isolated();
    let project = env.project().build();

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    assert!(out.status.success(), "{}", text(&out));
    let report = text(&out);
    let build_dir = storyhook::path_identity::build_dir()
        .expect("a cargo-built test binary must carry STORYHOOK_BUILD_DIR");
    assert!(
        report.contains("not installed") && report.contains(&build_dir.display().to_string()),
        "the binary row must say this build never left {}:\n{report}",
        build_dir.display()
    );
}

#[test]
fn it_still_answers_when_the_store_is_from_a_newer_storyhook() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    let future = storyhook::store::current_schema_version() + 1;
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch(&format!("PRAGMA user_version = {future}"))
        .expect("claiming a future schema");

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    assert!(
        out.status.success(),
        "the verb whose headline is `your store is out of range` must not need \
         that store to say so:\n{}",
        text(&out)
    );
    let report = text(&out);
    assert!(
        report.contains("READ-ONLY"),
        "it must name the condition:\n{report}"
    );
    assert!(
        report.contains(&future.to_string()),
        "it must name the version found:\n{report}"
    );
}

#[test]
fn a_pending_one_way_migration_is_reported_before_it_happens() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    // A store one version BEHIND this build: the shape that, on 2026-08-28,
    // silently carried this project's real tracker past every published
    // release. It is reported here before anything runs it.
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch("PRAGMA user_version = 1")
        .expect("planting an older schema");

    let out = env
        .story(project.path())
        .args(["doctor", "install"])
        .output()
        .expect("running `story doctor install`");

    let report = text(&out);
    assert!(
        report.contains("PENDING") && report.contains("one-way"),
        "a pending migration must be named, and named as irreversible:\n{report}"
    );
}

#[test]
fn the_summary_never_tells_anyone_to_revert_their_work() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    env.stop_daemon();
    rusqlite::Connection::open(env.store_path())
        .expect("opening the store")
        .execute_batch("PRAGMA user_version = 1")
        .expect("planting an older schema");

    let report = text(
        &env.story(project.path())
            .args(["doctor", "install"])
            .output()
            .expect("running `story doctor install`"),
    );

    // The whole doctrine of this verb in one assertion: a change sitting in a
    // checkout is aimed at the next release, so the remedy is always the
    // release. A detector that advised throwing the work away would be worse
    // than no detector.
    assert!(
        report.contains("never to revert"),
        "the summary must say the work survives:\n{report}"
    );
    assert!(
        !report.to_lowercase().contains("discard"),
        "nothing here may suggest discarding work:\n{report}"
    );
}

#[test]
fn plugin_sources_distinguish_current_stale_unpinned_and_checkout_installations() {
    let report = report_for_codex_source(None);
    assert!(
        report.contains("release ") && !report.contains("CHECKOUT"),
        "{report}"
    );

    let stale = report_for_codex_source(Some("release:2.2.0"));
    assert!(stale.contains("STALE RELEASE"), "{stale}");

    let unpinned = report_for_codex_source(Some("mikeydotio/storyhook"));
    assert!(unpinned.contains("UNPINNED"), "{unpinned}");

    let checkout = report_for_codex_source(Some("/Volumes/Code/storyhook"));
    assert!(checkout.contains("CHECKOUT"), "{checkout}");
}

/// The finding count the summary prints — the one number a flagged row moves.
///
/// A test build's `binary` row is always flagged (it never left cargo's build
/// directory, SH-630), so `every component agrees.` is unreachable from this
/// suite and cannot be asserted on directly. The summary is proven instead by
/// the count it derives that line from: one row flagged is one more finding.
fn finding_count(report: &str) -> usize {
    report
        .lines()
        .find_map(|line| {
            let (count, rest) = line.split_once(' ')?;
            rest.starts_with("finding(s).")
                .then(|| count.parse().ok())
                .flatten()
        })
        .unwrap_or_else(|| panic!("the report must print `N finding(s).`:\n{report}"))
}

/// The row's own finding line: the `!` line directly beneath `label`, if any.
fn finding_for(report: &str, label: &str) -> Option<String> {
    let mut lines = report.lines();
    lines.find(|line| line.starts_with(label))?;
    let next = lines.next()?;
    next.trim_start().strip_prefix("! ").map(str::to_string)
}

fn doctor_install(env: &TestEnv) -> String {
    let project = env.project().build();
    text(
        &env.story(project.path())
            .args(["doctor", "install"])
            .output()
            .expect("running `story doctor install`"),
    )
}

/// What a Claude Code install leaves behind that a lost registration does not
/// take with it: the plugin cache — the exact directory that survived on the
/// filing machine (SH-640).
fn plant_claude_cache(env: &TestEnv) -> std::path::PathBuf {
    let cache = env
        .home()
        .join(".claude/plugins/cache/storyhook/story/2.4.2");
    std::fs::create_dir_all(&cache).unwrap();
    env.home().join(".claude/plugins/cache/storyhook")
}

fn plant_codex_cache(env: &TestEnv) -> std::path::PathBuf {
    let cache = env
        .home()
        .join(".codex/plugins/cache/storyhook/story/2.4.2");
    std::fs::create_dir_all(&cache).unwrap();
    env.home().join(".codex/plugins/cache/storyhook")
}

fn write_claude_config_without_storyhook(env: &TestEnv) {
    let plugins = env.home().join(".claude/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "claude-plugins-official": {
                "source": { "source": "git", "url": "git@github.com:anthropics/claude-plugins-official.git" },
                "installLocation": env.home().join(".claude/plugins/marketplaces/claude-plugins-official"),
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

/// The incident as filed: `known_marketplaces.json` rewritten without its
/// `storyhook` key while the plugin cache survived. The row used to read
/// `not registered` unflagged, and the summary `every component agrees.`
#[test]
fn a_claude_registration_that_was_lost_is_flagged_not_reported_ok() {
    let control = doctor_install(&TestEnv::isolated());
    assert!(
        finding_for(&control, "claude plugin").is_none(),
        "positive control: an untouched machine's claude row carries no finding:\n{control}"
    );

    let env = TestEnv::isolated();
    let residue = plant_claude_cache(&env);
    write_claude_config_without_storyhook(&env);
    let report = doctor_install(&env);

    let finding = finding_for(&report, "claude plugin")
        .unwrap_or_else(|| panic!("the claude row must carry a finding:\n{report}"));
    assert!(
        finding.contains("DEREGISTERED"),
        "the finding must name the condition:\n{report}"
    );
    assert!(
        finding.contains(&residue.display().to_string()),
        "the finding must name the surviving copies:\n{report}"
    );
    assert!(
        finding.contains("story plugin install claude"),
        "the finding must name the remedy:\n{report}"
    );
    assert_eq!(
        finding_count(&report),
        finding_count(&control) + 1,
        "the summary must count the flagged row — it cannot read `every component \
         agrees` over it:\n{report}"
    );
}

/// The same loss with the whole configuration file gone, not just the key.
#[test]
fn a_claude_registration_lost_with_its_config_file_is_flagged() {
    let env = TestEnv::isolated();
    plant_claude_cache(&env);
    let report = doctor_install(&env);
    let finding = finding_for(&report, "claude plugin")
        .unwrap_or_else(|| panic!("the claude row must carry a finding:\n{report}"));
    assert!(finding.contains("DEREGISTERED"), "{report}");
}

/// A machine that never had the provider is the reason the quiet row exists,
/// and it must stay quiet: no residue, no registration, no finding.
#[test]
fn a_provider_that_was_never_installed_stays_quiet() {
    let env = TestEnv::isolated();
    write_claude_config_without_storyhook(&env);
    let report = doctor_install(&env);
    assert!(
        report.contains("claude plugin"),
        "the row must still be printed:\n{report}"
    );
    assert!(
        finding_for(&report, "claude plugin").is_none(),
        "a never-installed provider is not a finding:\n{report}"
    );
    assert!(
        finding_for(&report, "codex plugin").is_none(),
        "a never-installed provider is not a finding:\n{report}"
    );
}

/// The Codex row has the identical shape and the identical defect.
#[test]
fn a_codex_registration_that_was_lost_is_flagged_with_or_without_its_config() {
    let without_config = TestEnv::isolated();
    let residue = plant_codex_cache(&without_config);
    let report = doctor_install(&without_config);
    let finding = finding_for(&report, "codex plugin")
        .unwrap_or_else(|| panic!("the codex row must carry a finding:\n{report}"));
    assert!(finding.contains("DEREGISTERED"), "{report}");
    assert!(finding.contains(&residue.display().to_string()), "{report}");
    assert!(finding.contains("story plugin install codex"), "{report}");

    let with_config = TestEnv::isolated();
    plant_codex_cache(&with_config);
    std::fs::create_dir_all(with_config.home().join(".codex")).unwrap();
    std::fs::write(
        with_config.home().join(".codex/config.toml"),
        "[marketplaces.other]\nsource_type = \"local\"\nsource = \"/elsewhere\"\n",
    )
    .unwrap();
    let report = doctor_install(&with_config);
    let finding = finding_for(&report, "codex plugin")
        .unwrap_or_else(|| panic!("the codex row must carry a finding:\n{report}"));
    assert!(finding.contains("DEREGISTERED"), "{report}");
}

/// A file storyhook writes is storyhook's only while it carries the marker
/// storyhook wrote it with — `story plugin uninstall codex` preserves an
/// unmarked file at the same path as the user's, and the detector must read
/// it the same way, or a user-authored launcher reads as a lost install.
#[test]
fn codex_residue_counts_a_managed_file_by_its_marker_not_its_name() {
    let marked = TestEnv::isolated();
    let launcher = marked.home().join(".codex/storyhook/story.sh");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::write(
        &launcher,
        "# storyhook-managed: codex-launcher-v1\nexec story \"$@\"\n",
    )
    .unwrap();
    let report = doctor_install(&marked);
    let finding = finding_for(&report, "codex plugin")
        .unwrap_or_else(|| panic!("a marked launcher is storyhook's residue:\n{report}"));
    assert!(
        finding.contains(&launcher.display().to_string()),
        "{report}"
    );

    let unmarked = TestEnv::isolated();
    let launcher = unmarked.home().join(".codex/storyhook/story.sh");
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::write(&launcher, "#!/bin/sh\necho mine\n").unwrap();
    let report = doctor_install(&unmarked);
    assert!(
        finding_for(&report, "codex plugin").is_none(),
        "an unmarked file at a managed path is the user's, not residue:\n{report}"
    );
}
