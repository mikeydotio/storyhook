//! Production origin resolution with real isolated Git checkouts.
use std::path::Path;
use std::process::Command;
use storyhook::github_access::Repository;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkout(origin: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["remote", "add", "origin", origin]);
    root
}

#[test]
fn public_and_enterprise_origins_keep_their_destination() {
    for host in ["github.com", "github.example.com"] {
        for origin in [
            format!("https://{host}/Acme/Widgets.git"),
            format!("git@{host}:Acme/Widgets"),
            format!("ssh://git@{host}/Acme/Widgets.git"),
            format!("ssh://git@{host}:22/Acme/Widgets.git"),
        ] {
            let root = checkout(&origin);
            let resolved = Repository::resolve(root.path()).unwrap();
            assert_eq!(resolved.qualified(), format!("{host}/acme/widgets"));
            assert_eq!(
                resolved.transport_url(),
                format!("https://{host}/acme/widgets.git")
            );
        }
    }
}

#[test]
fn https_port_is_preserved_and_ssh_port_is_not_guessed() {
    let root = checkout("https://github.example.com:8443/acme/widgets.git");
    assert_eq!(
        Repository::resolve(root.path()).unwrap().identity().host,
        "github.example.com:8443"
    );
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@github.example.com:2222/acme/widgets",
        ],
    );
    assert!(
        Repository::resolve(root.path())
            .unwrap_err()
            .to_string()
            .contains("HTTPS")
    );
}

#[test]
fn changed_origin_is_read_again_instead_of_registered_or_cached_identity() {
    let root = checkout("https://github.com/old/widgets.git");
    assert_eq!(
        Repository::resolve(root.path()).unwrap().qualified(),
        "github.com/old/widgets"
    );
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.example.com:new/moved.git",
        ],
    );
    assert_eq!(
        Repository::resolve(root.path()).unwrap().qualified(),
        "github.example.com/new/moved"
    );
}

#[test]
fn unsupported_and_credential_bearing_origins_are_refused_without_secrets() {
    for origin in [
        "http://host/acme/widgets",
        "git://host/acme/widgets",
        "/tmp/local.git",
        "https://user:never-print-me@host/acme/widgets",
        "https://host/acme/widgets?secret=never-print-me",
        "https://host/acme/widgets/tree/main",
    ] {
        let root = checkout(origin);
        let error = Repository::resolve(root.path()).unwrap_err().to_string();
        assert!(!error.contains("never-print-me"), "{error}");
    }
}

#[test]
fn missing_checkout_missing_origin_and_multiple_origins_fail_closed() {
    let root = checkout("https://host/acme/widgets");
    assert!(Repository::resolve(&root.path().join("missing")).is_err());
    git(root.path(), &["remote", "remove", "origin"]);
    assert!(Repository::resolve(root.path()).is_err());
    git(
        root.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://host/acme/widgets",
        ],
    );
    git(
        root.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://other/acme/widgets",
        ],
    );
    assert!(Repository::resolve(root.path()).is_err());
}

#[test]
fn a_directory_inside_an_unrelated_checkout_is_not_a_checkout() {
    let root = checkout("https://host/acme/widgets");
    let child = root.path().join("missing-project");
    std::fs::create_dir(&child).unwrap();
    assert!(Repository::resolve(&child).is_err());
}

#[test]
fn raw_origin_is_not_changed_by_transport_rewrites() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    git(
        root.path(),
        &[
            "config",
            "url.https://unrelated.invalid/.insteadOf",
            "git@github.example.com:",
        ],
    );
    assert_eq!(
        Repository::resolve(root.path()).unwrap().identity().host,
        "github.example.com"
    );
}

#[test]
fn linked_worktree_uses_its_actual_shared_origin() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let lane = root.path().join("lane");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "lane",
            lane.to_str().unwrap(),
        ],
    );
    assert_eq!(
        Repository::resolve(&lane).unwrap().identity().host,
        "github.example.com"
    );
}

fn helper(root: &Path, arguments: &[&str], fake: Option<&str>) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    if let Some(script) = fake {
        std::fs::write(bin.join("gh"), format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_story"))
        .env_clear()
        .env("HOME", root)
        .env("TMPDIR", "/tmp")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GH_HOST", "unrelated.invalid")
        .env("GH_REPO", "unrelated.invalid/wrong/repo")
        .env("GH_DEBUG", "api")
        .env("GH_ENTERPRISE_TOKEN", "opaque-fixture-secret")
        .env("GH_CONFIG_DIR", root.join("gh-config"))
        .arg("github")
        .args(arguments)
        .output()
        .unwrap()
}

const RECORD_GH: &str =
    r#"printf '%s\n' "$GH_HOST" "$GH_REPO" "$GH_PROMPT_DISABLED" "${GH_DEBUG-unset}" "$@""#;

#[test]
fn helper_overrides_ambient_routing_and_passes_explicit_enterprise_destination() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "pr",
            "view",
            "7",
            "--json",
            "state",
        ],
        Some(RECORD_GH),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        lines,
        "github.example.com\ngithub.example.com/acme/widgets\n1\nunset\npr\nview\n7\n--json\nstate\n--repo\ngithub.example.com/acme/widgets\n"
    );
}

#[test]
fn api_calls_use_explicit_host_and_repository_paths() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "api",
            "repos/acme/widgets/pulls/7",
        ],
        Some(RECORD_GH),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("api\nrepos/acme/widgets/pulls/7\n--hostname\ngithub.example.com\n")
    );
}

#[test]
fn helpers_reject_routing_overrides_and_foreign_prs_before_spawning() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    for args in [
        vec!["pr", "view", "7", "--repo", "other/wrong"],
        vec!["pr", "view", "https://other/acme/widgets/pull/7"],
        vec!["api", "https://other/api/v3/repos/acme/widgets/pulls/7"],
        vec!["api", "repos/acme/foreign/pulls/7"],
        vec!["api", "repos/acme/widgets/pulls/7", "--hostname=other"],
        vec!["auth", "login"],
    ] {
        let mut arguments = vec!["exec", "--checkout", root.path().to_str().unwrap(), "--"];
        arguments.extend(args);
        let output = helper(root.path(), &arguments, Some("echo should-not-run"));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("not implemented"));
    }
}

#[test]
fn missing_gh_is_an_actionable_local_failure() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "pr",
            "view",
            "7",
        ],
        None,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("install gh"));
}

#[test]
fn failed_gh_reports_context_without_credentials() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "pr",
            "view",
            "7",
        ],
        Some("echo \"HTTP 401 $GH_ENTERPRISE_TOKEN\" >&2; exit 4"),
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("github.example.com/acme/widgets"));
    assert!(error.contains("gh auth login --hostname github.example.com"));
    assert!(!error.contains("opaque-fixture-secret"));
}

#[test]
fn resolve_helper_is_local_and_returns_structured_origin() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let output = helper(
        root.path(),
        &["resolve", "--checkout", root.path().to_str().unwrap()],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["identity"]["host"], "github.example.com");
    assert!(
        !root.path().join(".local").exists(),
        "must not start a daemon or open a store"
    );
}

#[test]
fn repo_view_uses_its_positional_repository_interface() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "repo",
            "view",
            "--json",
            "nameWithOwner",
        ],
        Some(RECORD_GH),
    );
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.ends_with("repo\nview\ngithub.example.com/acme/widgets\n--json\nnameWithOwner\n"),
        "{output}"
    );
}

#[test]
fn foreign_urls_after_flags_and_api_header_overrides_are_refused() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    for args in [
        vec![
            "pr",
            "view",
            "--json",
            "state",
            "https://foreign/acme/widgets/pull/1",
        ],
        vec!["api", "repos/acme/widgets/pulls/1", "-H", "Host: foreign"],
        vec![
            "api",
            "repos/acme/widgets/pulls/1",
            "--header=Authorization: bearer secret",
        ],
    ] {
        let mut arguments = vec!["exec", "--checkout", root.path().to_str().unwrap(), "--"];
        arguments.extend(args);
        let output = helper(root.path(), &arguments, Some("echo should-not-run"));
        assert!(!output.status.success(), "must refuse before gh runs");
    }
}

fn recording_transport(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("git"), "#!/bin/sh\ncase \" $* \" in *' --get-url '*) exec /usr/bin/git \"$@\";; *' fetch '*|*' push '*|*' ls-remote '*|*' clone '*) printf '%s\\n' \"$@\";; *) exec /usr/bin/git \"$@\";; esac\n").unwrap();
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn git_transport_uses_host_scoped_gh_credentials_and_explicit_https_origin() {
    for origin in [
        "git@github.example.com:acme/widgets.git",
        "ssh://git@github.example.com/acme/widgets.git",
        "https://github.example.com/acme/widgets.git",
    ] {
        let root = checkout(origin);
        recording_transport(root.path());
        for operation in ["fetch", "push", "ls-remote"] {
            let output = helper(
                root.path(),
                &[
                    "git",
                    "--checkout",
                    root.path().to_str().unwrap(),
                    "--",
                    operation,
                    "origin",
                    "refs/heads/main",
                ],
                Some(RECORD_GH),
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = String::from_utf8(output.stdout).unwrap();
            assert!(
                output.contains(
                    "credential.https://github.example.com.helper=!gh auth git-credential"
                ),
                "{output}"
            );
            assert!(
                output.contains(&format!(
                    "{operation}\nhttps://github.example.com/acme/widgets.git\nrefs/heads/main\n"
                )),
                "{output}"
            );
        }
    }
}

#[test]
fn git_transport_refuses_foreign_push_urls_and_transport_rewrites() {
    for (key, value) in [
        ("remote.origin.pushurl", "https://foreign/acme/widgets"),
        (
            "url.https://foreign/.insteadOf",
            "https://github.example.com/",
        ),
    ] {
        let root = checkout("https://github.example.com/acme/widgets.git");
        git(root.path(), &["config", key, value]);
        recording_transport(root.path());
        let output = helper(
            root.path(),
            &[
                "git",
                "--checkout",
                root.path().to_str().unwrap(),
                "--",
                "push",
                "origin",
                "refs/heads/main",
            ],
            Some(RECORD_GH),
        );
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("destination"), "{error}");
        assert!(
            output.stdout.is_empty(),
            "must refuse before network access"
        );
    }
}

#[test]
fn https_push_rewrite_cannot_redirect_a_converted_ssh_origin() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    git(
        root.path(),
        &[
            "config",
            "url.https://foreign/.pushInsteadOf",
            "https://github.example.com/",
        ],
    );
    recording_transport(root.path());
    let output = helper(
        root.path(),
        &[
            "git",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "push",
            "origin",
            "refs/heads/main",
        ],
        Some(RECORD_GH),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pushInsteadOf"));
    assert!(output.stdout.is_empty());
}

#[test]
fn clone_uses_the_authoritative_https_origin_and_an_explicit_destination() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    recording_transport(root.path());
    let destination = root.path().join("mirror with spaces");
    let output = helper(
        root.path(),
        &[
            "git",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "clone",
            "--mirror",
            "origin",
            destination.to_str().unwrap(),
        ],
        Some(RECORD_GH),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(&format!(
            "clone\n--mirror\nhttps://github.example.com/acme/widgets.git\n{}\n",
            destination.display()
        )),
        "{text}"
    );
    for destination in ["relative", "--upload-pack=evil", "https://foreign/repo"] {
        let output = helper(
            root.path(),
            &[
                "git",
                "--checkout",
                root.path().to_str().unwrap(),
                "--",
                "clone",
                "origin",
                destination,
            ],
            Some(RECORD_GH),
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn helper_checks_explicit_source_authority_on_every_call() {
    let authority = checkout("https://github.example.com/acme/widgets.git");
    let lane = checkout("git@github.example.com:acme/widgets.git");
    let args = [
        "exec",
        "--checkout",
        lane.path().to_str().unwrap(),
        "--authority",
        authority.path().to_str().unwrap(),
        "--",
        "pr",
        "view",
        "7",
        "--json",
        "state",
    ];
    let good = helper(lane.path(), &args, Some(RECORD_GH));
    assert!(
        good.status.success(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    git(
        authority.path(),
        &[
            "config",
            "remote.origin.url",
            "https://github.com/other/repo.git",
        ],
    );
    let refused = helper(lane.path(), &args, Some(RECORD_GH));
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("authority"));
}

#[test]
fn a_multistep_operation_refuses_to_reinterpret_identity_after_origin_changes() {
    let root = checkout("https://github.example.com/acme/widgets.git");
    let args = [
        "resolve",
        "--checkout",
        root.path().to_str().unwrap(),
        "--expected",
        "github.example.com/acme/widgets",
    ]
    .map(String::from);
    storyhook::github_access::run_local(&args).expect("initial identity agrees");
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/widgets.git",
        ],
    );
    let error = storyhook::github_access::run_local(&args).unwrap_err();
    assert!(error.to_string().contains("origin changed"), "{error}");
}

const CHANGE_ORIGIN_ON_FAILURE: &str = r#"
printf '%s\n' "$GH_REPO" >> attempts
if [ ! -f moved ]; then
    touch moved
    git remote set-url origin https://github.example.com/new/widgets.git
    echo stale-origin >&2
    exit 1
fi
printf '%s\n' "$GH_REPO"
"#;

#[test]
fn failed_unpinned_metadata_read_refreshes_changed_origin_once() {
    let root = checkout("https://github.com/old/widgets.git");
    let output = helper(
        root.path(),
        &[
            "exec",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "pr",
            "list",
            "--json",
            "number",
        ],
        Some(CHANGE_ORIGIN_ON_FAILURE),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "github.example.com/new/widgets"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("attempts"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn changed_origin_never_retries_mutations_or_pinned_reads() {
    for pinned in [false, true] {
        let root = checkout("https://github.com/old/widgets.git");
        let mut args = vec!["exec", "--checkout", root.path().to_str().unwrap()];
        if pinned {
            args.extend(["--expected", "github.com/old/widgets"]);
        }
        if pinned {
            args.extend(["--", "pr", "list", "--json", "number"]);
        } else {
            args.extend([
                "--", "pr", "create", "--title", "fixture", "--body", "fixture",
            ]);
        }
        let output = helper(root.path(), &args, Some(CHANGE_ORIGIN_ON_FAILURE));
        assert!(!output.status.success());
        assert_eq!(
            std::fs::read_to_string(root.path().join("attempts"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[test]
fn read_refresh_is_bounded_and_rechecks_supplied_identity_constraints() {
    for case in ["unchanged", "twice", "source", "url"] {
        let root = checkout("https://github.com/old/widgets.git");
        let authority = checkout("https://github.com/old/widgets.git");
        let mut args = vec!["exec", "--checkout", root.path().to_str().unwrap()];
        if case == "source" {
            args.extend(["--authority", authority.path().to_str().unwrap()]);
        }
        if case == "url" {
            args.extend([
                "--",
                "pr",
                "view",
                "https://github.com/old/widgets/pull/1",
                "--json",
                "number",
            ]);
        } else {
            args.extend(["--", "pr", "list", "--json", "number"]);
        }
        let script = match case {
            "unchanged" => "echo attempt >> attempts; exit 1",
            "twice" => {
                "echo attempt >> attempts; n=$(wc -l < attempts | tr -d ' '); if [ \"$n\" -lt 3 ]; then git remote set-url origin https://github.example.com/new$n/widgets.git; fi; exit 1"
            }
            _ => CHANGE_ORIGIN_ON_FAILURE,
        };
        let output = helper(root.path(), &args, Some(script));
        assert!(!output.status.success(), "{case}");
        let attempts = std::fs::read_to_string(root.path().join("attempts")).unwrap();
        assert_eq!(
            attempts.lines().count(),
            if case == "twice" { 2 } else { 1 },
            "{case}"
        );
    }
}

#[test]
fn generic_observations_keep_filesystem_origins_off_the_network() {
    let bare = tempfile::tempdir_in("/tmp").unwrap();
    git(bare.path(), &["init", "--bare", "--quiet"]);
    let root = checkout(bare.path().to_str().unwrap());
    let call = || {
        Command::new(env!("CARGO_BIN_EXE_story"))
            .args([
                "github",
                "observe",
                "--checkout",
                root.path().to_str().unwrap(),
                "--",
                "ls-remote",
                "--symref",
                "origin",
                "HEAD",
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    };
    let local = call();
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    git(
        root.path(),
        &[
            "config",
            "url.https://must-not-contact.invalid/repo.insteadOf",
            bare.path().to_str().unwrap(),
        ],
    );
    let redirected = call();
    assert!(!redirected.status.success());
    assert!(String::from_utf8_lossy(&redirected.stderr).contains("rewrite"));
    assert!(
        Repository::resolve(root.path()).is_err(),
        "local origin never grants GitHub authority"
    );
}

#[test]
fn observations_pin_origin_and_cannot_write_remotes() {
    use storyhook::github_access::OriginObservation;
    let root = checkout("https://github.example.com/acme/widgets.git");
    let observation = OriginObservation::resolve(root.path()).unwrap();
    let push = ["push".into(), "origin".into(), "main".into()];
    assert!(
        observation
            .git(&push)
            .unwrap_err()
            .to_string()
            .contains("only ls-remote and fetch")
    );
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/widgets.git",
        ],
    );
    let read = ["ls-remote".into(), "origin".into(), "HEAD".into()];
    assert!(
        observation
            .git(&read)
            .unwrap_err()
            .to_string()
            .contains("origin changed")
    );
}

#[test]
fn generic_enterprise_observations_use_the_https_credential_boundary() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    recording_transport(root.path());
    let output = helper(
        root.path(),
        &[
            "observe",
            "--checkout",
            root.path().to_str().unwrap(),
            "--",
            "fetch",
            "origin",
            "refs/heads/main",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.contains("credential.https://github.example.com.helper=!gh auth git-credential")
    );
    assert!(
        output.contains("fetch\nhttps://github.example.com/acme/widgets.git\nrefs/heads/main")
    );
}

#[test]
fn observation_mirrors_require_matching_current_source_authority() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    let source = checkout("https://github.example.com/acme/widgets.git");
    recording_transport(root.path());
    let call = || {
        helper(
            root.path(),
            &[
                "observe",
                "--checkout",
                root.path().to_str().unwrap(),
                "--authority",
                source.path().to_str().unwrap(),
                "--",
                "fetch",
                "origin",
                "main",
            ],
            None,
        )
    };
    let matched = call();
    assert!(
        matched.status.success(),
        "{}",
        String::from_utf8_lossy(&matched.stderr)
    );
    git(
        source.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "https://other.example/acme/widgets",
        ],
    );
    let refused = call();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("differs from source authority"));
}
