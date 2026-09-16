//! Shell adapters must reach the production local GitHub boundary.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use storyhook_test_support::scratch_dir;

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn bundled_adapter_pins_enterprise_origin_and_rejects_foreign_authority() {
    let root = scratch_dir();
    let repo = root.path().join("repo with spaces");
    let foreign = root.path().join("foreign");
    let bin = root.path().join("bin");
    for path in [&repo, &foreign, &bin] {
        fs::create_dir(path).unwrap();
    }
    for (path, origin) in [
        (&repo, "git@github.example.com:acme/widgets.git"),
        (&foreign, "https://github.com/other/repo"),
    ] {
        git(path, &["init", "-q"]);
        git(path, &["remote", "add", "origin", origin]);
    }
    let log = root.path().join("calls.json");
    fs::write(bin.join("gh"), format!("#!/usr/bin/env python3\nimport json,os,sys\nwith open({},'w') as f: json.dump([sys.argv[1:],os.environ.get('GH_HOST'),os.environ.get('GH_REPO')],f)\nprint('accepted')\n", serde_json::to_string(&log).unwrap())).unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/github-access.sh");
    let run = |authority: &Path, body: &str| {
        Command::new("bash")
            .args(["-c", body, "fixture"])
            .arg(&adapter)
            .current_dir(&repo)
            .env("STORY_BIN", env!("CARGO_BIN_EXE_story"))
            .env("STORYHOOK_GITHUB_AUTHORITY", authority)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("GH_HOST", "wrong.example")
            .env("GH_REPO", "wrong.example/x/y")
            .env("HOME", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    let body = "source \"$1\" && github_exec pr view 12 --json number";
    let out = run(&repo, body);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls: serde_json::Value = serde_json::from_slice(&fs::read(&log).unwrap()).unwrap();
    assert_eq!(
        calls,
        serde_json::json!([
            [
                "pr",
                "view",
                "12",
                "--json",
                "number",
                "--repo",
                "github.example.com/acme/widgets"
            ],
            "github.example.com",
            "github.example.com/acme/widgets"
        ])
    );
    fs::remove_file(&log).unwrap();
    let refused = run(&foreign, body);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("differs from source authority"));
    assert!(!log.exists(), "foreign authority must fail before gh");
    let changed = run(
        &repo,
        "source \"$1\" && github_begin && git remote set-url origin https://github.com/other/repo && github_exec pr merge 12 --merge",
    );
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("origin changed"));
    assert!(
        !log.exists(),
        "origin changes must fail before a later mutation"
    );
}

#[test]
fn verifier_keeps_host_authentication_diagnostics() {
    let root = scratch_dir();
    let repo = root.path().join("repo");
    let bin = root.path().join("bin");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.test"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "fixture"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.example.com/acme/widgets.git",
        ],
    );
    fs::write(
        bin.join("gh"),
        "#!/bin/sh\necho 'host authentication expired' >&2\nexit 4\n",
    )
    .unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-pr.sh"))
        .args([
            "https://github.example.com/acme/widgets/pull/12",
            "--",
            "true",
        ])
        .current_dir(&repo)
        .envs(storyhook_test_support::daemon_containment())
        .env("STORY_BIN", env!("CARGO_BIN_EXE_story"))
        .env("STORYHOOK_VERIFIER_MIRROR", "0")
        .env("STORYHOOK_LOCK_DIR", root.path().join("locks"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("HOME", root.path())
        .output()
        .unwrap();
    let result: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{out:?}"));
    assert!(
        result["detail"]
            .as_str()
            .unwrap()
            .contains("gh auth login --hostname github.example.com"),
        "{result}"
    );
}

#[test]
fn verifier_network_calls_cannot_bypass_the_origin_boundary() {
    for name in [
        "scripts/verify-pr.sh",
        "scripts/land-pr.sh",
        "scripts/landing-intent.sh",
        "scripts/release.sh",
        "scripts/browser-watch.sh",
        "scripts/coverage-watch.sh",
        "scripts/origin-default-branch.sh",
        "plugins/story/lib/session.sh",
    ] {
        let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap();
        for (n, line) in source.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            for bypass in [
                "git fetch ",
                "git push ",
                "git ls-remote ",
                "$(gh ",
                "    gh ",
            ] {
                // The adapter's function name includes `git`; only a separate
                // command word is a bypass, not `github_git`.
                let code = line
                    .replace("github_git", "routed_transport")
                    .replace("origin_git", "routed_transport");
                assert!(
                    !code.contains(bypass),
                    "{name}:{} bypasses origin validation: {line}",
                    n + 1
                );
            }
        }
    }
}

#[test]
fn endpoint_fixture_refuses_live_urls_without_rewriting_resolution_reads() {
    let root = scratch_dir();
    storyhook_test_support::install_git_endpoint(root.path(), &[]);
    let run = |args: &[&str]| {
        Command::new(root.path().join("git"))
            .args(args)
            .output()
            .unwrap()
    };
    let read = run(&[
        "ls-remote",
        "--get-url",
        "https://unmapped.example/acme/repo.git",
    ]);
    assert!(read.status.success());
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "https://unmapped.example/acme/repo.git"
    );
    let network = run(&["ls-remote", "https://unmapped.example/acme/repo.git"]);
    assert!(!network.status.success());
    assert!(
        String::from_utf8_lossy(&network.stderr)
            .contains("fixture refuses unmapped network destination")
    );
}

#[test]
fn plugin_and_verifier_ship_the_same_thin_adapter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        fs::read(root.join("plugins/story/lib/github-access.sh")).unwrap(),
        fs::read(root.join("scripts/github-access.sh")).unwrap(),
    );
}

#[test]
fn test_children_cannot_inherit_gh_authentication_selectors() {
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/github-access.sh");
    let out = Command::new("bash")
        .args([
            "-c",
            "source \"$1\"; github_without_credentials /usr/bin/env",
            "fixture",
        ])
        .arg(adapter)
        .env("GH_CONFIG_DIR", "/private/credential-fixture")
        .env("GH_TOKEN", "public-fixture")
        .env("GITHUB_TOKEN", "fallback-fixture")
        .env("GH_ENTERPRISE_TOKEN", "enterprise-fixture")
        .env("GITHUB_ENTERPRISE_TOKEN", "enterprise-fallback-fixture")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let environment = String::from_utf8(out.stdout).unwrap();
    for name in [
        "GH_CONFIG_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ] {
        assert!(
            !environment
                .lines()
                .any(|line| line.starts_with(&format!("{name}=")))
        );
    }
}

#[test]
fn watch_refreshes_update_the_tracking_ref_read_by_their_plan() {
    let root = scratch_dir();
    let source = root.path().join("source");
    let checkout = root.path().join("checkout");
    let bin = root.path().join("bin");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&bin).unwrap();
    git(&source, &["init", "-q", "-b", "dev"]);
    git(&source, &["config", "user.name", "Fixture"]);
    git(&source, &["config", "user.email", "fixture@example.test"]);
    git(&source, &["commit", "--allow-empty", "-qm", "initial"]);
    git(
        root.path(),
        &[
            "clone",
            "-q",
            source.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    let endpoint = "https://github.example.com/acme/watches.git";
    storyhook_test_support::install_git_endpoint(&bin, &[(endpoint, &source)]);
    git(
        &checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.example.com:acme/watches.git",
        ],
    );
    for script in ["browser-watch.sh", "coverage-watch.sh"] {
        git(&source, &["commit", "--allow-empty", "-qm", script]);
        let expected = Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let expected = String::from_utf8(expected.stdout).unwrap();
        let out = Command::new("bash")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(script),
            )
            .arg("--plan")
            .current_dir(&checkout)
            .env("STORY_BIN", env!("CARGO_BIN_EXE_story"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("HOME", root.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("tip={}", expected.trim())));
    }
}

#[test]
fn production_github_access_has_one_cli_executor_and_no_http_or_pat_bypass() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tracked = Command::new("git")
        .args(["ls-files", "src", "scripts", "plugins", "install.sh"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(tracked.status.success());
    for name in String::from_utf8(tracked.stdout).unwrap().lines() {
        if name.contains("/tests/")
            || name.ends_with("/tests.rs")
            || name.contains("/fakes/")
            || name.contains("/_vendor/")
            || name == "src/store/conformance.rs"
            || name == "scripts/run-e2e.sh"
            || !(name.ends_with(".rs") || name.ends_with(".sh") || name.ends_with(".py"))
        {
            continue;
        }
        let text = fs::read_to_string(root.join(name)).unwrap();
        let production = text.split("#[cfg(test)]").next().unwrap();
        let code = production
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                !line.starts_with("//") && !line.starts_with('#')
            })
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "github.com",
            "api.github.com",
            "raw.githubusercontent.com",
            "STORYHOOK_GITHUB_TOKEN",
        ] {
            if name == "src/help_topics.rs" {
                continue;
            } // explicit migration guidance
            assert!(
                !code.contains(forbidden),
                "{name} contains runtime bypass {forbidden}"
            );
        }
        if name != "src/github_access/command.rs" {
            assert!(
                !code.contains("Command::new(\"gh\")"),
                "{name} bypasses the shared gh executor"
            );
        }
        if name.starts_with("src/github/") || name == "src/update.rs" {
            for forbidden in ["ureq::", "reqwest::", "keyring", "Authorization"] {
                assert!(
                    !code.contains(forbidden),
                    "{name} bypasses gh with {forbidden}"
                );
            }
        }
    }
}
