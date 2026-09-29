//! SH-844: real merges cannot borrow attributes from the machine or checkout.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use storyhook::daemon::verification::VerificationCancellation as Cancellation;
use storyhook::service::batch_assembly::{
    AssemblyMember, LastMerge, SmoothedMerge, assemble, merge_smoothed,
};
use storyhook::service::gate_snapshot;
use storyhook::service::trial_merge::{PrivateTrialMerger, TrialMerge, TrialMerger};
use storyhook_test_support::{TestEnv, scratch_dir};

fn git(root: &Path, args: &[&str]) -> String {
    let result = storyhook::env::git_env::command(root)
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "git {args:?}: {result:?}");
    String::from_utf8(result.stdout).unwrap().trim().into()
}

fn preflight(root: &Path, base: &str, head: &str) -> Output {
    Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-preflight.sh"))
        .args(["--json", base, head])
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn ambient_attributes_cannot_hide_conflicts() {
    for source in [
        "worktree",
        "info",
        "configured",
        "xdg",
        "staged",
        "committed",
        "modified",
        "global",
        "default-driver",
        "custom-driver",
        "linked-info",
        "injected",
        "unsupported",
    ] {
        let environment = TestEnv::isolated();
        let mut command = Command::new(std::env::current_exe().unwrap());
        environment.apply(&mut command);
        let old_git = scratch_dir();
        if source == "unsupported" {
            let real = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap();
            assert!(real.status.success());
            let real = String::from_utf8(real.stdout).unwrap();
            let wrapper = old_git.path().join("git");
            let real = real.trim().replace('\'', "'\"'\"'");
            std::fs::write(&wrapper, format!("#!/bin/sh\nfor arg do\ncase \"$arg\" in --attr-source=*) echo 'unsupported --attr-source' >&2; exit 129;; esac\ndone\nexec '{real}' \"$@\"\n")).unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            command.env(
                "PATH",
                format!(
                    "{}:{}",
                    old_git.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            );
        }
        if source == "injected" {
            command
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "merge.default")
                .env("GIT_CONFIG_VALUE_0", "union");
        }
        let output = command
            .args(["--exact", "attribute_child", "--nocapture"])
            .env("SH844_ATTRIBUTE_SOURCE", source)
            .env("TMPDIR", old_git.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "source {source}: {output:?}");
        let leftovers: Vec<_> = std::fs::read_dir(old_git.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("storyhook-merge-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "{source}: leaked merge administration or objects: {leftovers:?}"
        );
    }
}

#[test]
fn attribute_child() {
    let Ok(source) = std::env::var("SH844_ATTRIBUTE_SOURCE") else {
        return;
    };
    let dir = scratch_dir();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "t"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
    let attributes = "*.rs merge=union\n";
    if matches!(source.as_str(), "committed" | "modified") {
        std::fs::write(
            root.join(".gitattributes"),
            if source == "modified" {
                "*.rs merge=text\n"
            } else {
                attributes
            },
        )
        .unwrap();
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("src/a.rs"), "fn a() { x(); }\n").unwrap();
    git(root, &["commit", "-qam", "x"]);
    let x = git(root, &["rev-parse", "HEAD"]);
    git(root, &["checkout", "-q", "--detach", &base]);
    std::fs::write(root.join("src/a.rs"), "fn a() { y(); }\n").unwrap();
    git(root, &["commit", "-qam", "y"]);
    let y = git(root, &["rev-parse", "HEAD"]);
    let linked = dir.path().join("linked");
    let root = if source == "linked-info" {
        std::fs::write(root.join(".git/info/attributes"), attributes).unwrap();
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
                &y,
            ],
        );
        linked.as_path()
    } else {
        root
    };
    let marker = dir.path().join("driver-ran");
    match source.as_str() {
        "worktree" | "staged" => {
            std::fs::write(root.join("src/.gitattributes"), attributes).unwrap();
            if source == "staged" {
                git(root, &["add", "src/.gitattributes"]);
                std::fs::remove_file(root.join("src/.gitattributes")).unwrap();
            }
        }
        "info" => std::fs::write(root.join(".git/info/attributes"), attributes).unwrap(),
        "configured" => {
            let file = root.join("local-attributes");
            std::fs::write(&file, attributes).unwrap();
            git(
                root,
                &["config", "core.attributesFile", file.to_str().unwrap()],
            );
        }
        "xdg" => {
            let path = Path::new(&std::env::var("XDG_CONFIG_HOME").unwrap()).join("git");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("attributes"), attributes).unwrap();
        }
        "committed" => (),
        "modified" => std::fs::write(root.join(".gitattributes"), attributes).unwrap(),
        "global" => {
            let file = dir.path().join("global-attributes");
            std::fs::write(&file, attributes).unwrap();
            git(
                root,
                &[
                    "config",
                    "--global",
                    "core.attributesFile",
                    file.to_str().unwrap(),
                ],
            );
        }
        "default-driver" => {
            git(root, &["config", "merge.default", "union"]);
        }
        "custom-driver" => {
            std::fs::write(root.join(".git/info/attributes"), "*.rs merge=custom\n").unwrap();
            let driver = format!(
                "printf ran > '{}'; git merge-file --union %A %O %B",
                marker.display()
            );
            git(root, &["config", "merge.custom.driver", &driver]);
        }
        "linked-info" | "injected" | "unsupported" => (),
        "system" => {
            let file = git(root, &["var", "GIT_ATTR_SYSTEM"]);
            assert_eq!(
                file,
                std::env::var("SH844_SYSTEM_ATTRIBUTES").expect("system runner must own this path")
            );
            assert!(
                Path::new(&file).starts_with("/tmp")
                    || Path::new(&file).starts_with("/private/tmp")
            );
            std::fs::write(file, attributes).unwrap();
        }
        _ => panic!("unknown source {source}"),
    }
    if source == "unsupported" {
        let mut merger = PrivateTrialMerger::open(root).unwrap();
        assert!(
            merger
                .merge(&x, &y)
                .unwrap_err()
                .to_string()
                .contains("unsupported --attr-source")
        );
        let tree = git(root, &["rev-parse", &format!("{x}^{{tree}}")]);
        assert!(
            gate_snapshot::inspect(root, &base, &x, &tree)
                .unwrap_err()
                .to_string()
                .contains("unsupported --attr-source")
        );
        for structured in [true, false] {
            let mut command = Command::new("bash");
            command.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-preflight.sh"));
            if structured {
                command.arg("--json");
            }
            let result = command
                .args([&base, &x])
                .current_dir(root)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(3), "{result:?}");
            assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported --attr-source"));
        }
        let member = AssemblyMember {
            story_id: "SH-1".into(),
            branch: "x".into(),
            commit: x,
        };
        assert!(
            assemble(root, "batch", &base, &[member], &Cancellation::default())
                .unwrap_err()
                .to_string()
                .contains("unsupported --attr-source")
        );
        return;
    }
    // The source index alone does not affect merge-tree. Every actual
    // ambient source must demonstrably hide the conflict without isolation.
    let raw = Command::new("git")
        .current_dir(root)
        .args(["merge-tree", "--write-tree", &x, &y])
        .output()
        .unwrap();
    assert_eq!(
        raw.status.code(),
        Some(i32::from(source == "staged")),
        "positive control {source}: {raw:?}"
    );
    let union_tree = String::from_utf8(raw.stdout)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    if source == "custom-driver" {
        assert!(
            marker.exists(),
            "custom driver must run in positive control"
        );
        std::fs::remove_file(&marker).unwrap();
    }
    let state = git(root, &["status", "--porcelain=v1"]);
    let objects = git(root, &["count-objects", "-v"]);
    let mut trial = PrivateTrialMerger::open(root).unwrap();
    let clean_tree = git(root, &["rev-parse", &format!("{x}^{{tree}}")]);
    assert_eq!(
        trial.merge(&base, &x).unwrap(),
        TrialMerge::Clean {
            tree: clean_tree.clone()
        }
    );
    assert!(gate_snapshot::inspect(root, &base, &x, &clean_tree).is_ok());
    let clean = preflight(root, &base, &x);
    assert_eq!(clean.status.code(), Some(1), "clean parity: {clean:?}");
    let clean: serde_json::Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert_eq!(clean["tree"], clean_tree);
    assert!(
        matches!(trial.merge(&x, &y).unwrap(), TrialMerge::Conflict { .. }),
        "{source}: trial"
    );
    assert!(
        gate_snapshot::inspect(root, &x, &y, &union_tree).is_err(),
        "{source}: inspection"
    );
    let output = preflight(root, &x, &y);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{source}: preflight {output:?}"
    );
    assert_eq!(
        git(root, &["count-objects", "-v"]),
        objects,
        "inspection wrote source objects"
    );
    let member = AssemblyMember {
        story_id: "SH-2".into(),
        branch: "y".into(),
        commit: y,
    };
    let cancel = Cancellation::default();
    assert!(
        assemble(root, "batch", &x, std::slice::from_ref(&member), &cancel).is_err(),
        "{source}: assembly"
    );
    let first = AssemblyMember {
        story_id: "SH-1".into(),
        branch: "x".into(),
        commit: x.clone(),
    };
    let mut assembly =
        assemble(root, "batch", &base, std::slice::from_ref(&first), &cancel).unwrap();
    let outcome = merge_smoothed(
        &SmoothedMerge {
            repository: root,
            branch: "batch",
            batch: "0123456789ab",
            base: &base,
            earlier: &[first],
            member: &member,
        },
        &mut assembly,
        &cancel,
    )
    .unwrap();
    assert!(
        matches!(outcome, LastMerge::Refused(_)),
        "{source}: smoothing {outcome:?}"
    );
    assert_eq!(git(root, &["status", "--porcelain=v1"]), state);
    assert!(!marker.exists(), "protected merge executed a local driver");
    shell_paths(root, &x, &member.commit, &union_tree);
}

#[test]
fn production_merge_commands_have_only_isolated_entry_points() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listing = git(
        checkout,
        &["ls-files", "-z", "--", "src", "scripts", "plugins"],
    );
    let mut found = Vec::new();
    for name in listing.split('\0') {
        if name.is_empty() || name.contains("/tests/") || name.starts_with("scripts/test-") {
            continue;
        }
        if !name.ends_with(".rs") && !name.ends_with(".sh") {
            continue;
        }
        let source = std::fs::read_to_string(checkout.join(name)).unwrap();
        if has_merge_command(&source, name.ends_with(".rs")) {
            found.push(name.to_owned());
        }
    }
    found.sort();
    assert_eq!(
        found,
        [
            "scripts/merge-preflight.sh",
            "src/service/isolated_merge.rs"
        ],
        "every production merge must use the isolated attribute policy"
    );
    assert!(has_merge_command(
        "command.args([\"merge-tree\", \"--write-tree\"])",
        true
    ));
    assert!(has_merge_command("git merge-file -p a b c", false));
    assert!(has_merge_command(
        "git \"merge-tree\" --write-tree a b",
        false
    ));
    assert!(!has_merge_command(
        "die \"use the merge-tree verifier\"",
        false
    ));
    assert!(!has_merge_command("// mentions merge-tree\n", true));
}

fn has_merge_command(source: &str, rust: bool) -> bool {
    let source = if rust {
        storyhook_test_support::without_rust_comments(source)
    } else {
        source.to_owned()
    };
    source.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#')
            && if rust {
                line.contains("\"merge-tree\"") || line.contains("\"merge-file\"")
            } else {
                shell_has_merge_word(line)
            }
    })
}

fn shell_has_merge_word(line: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut word = String::new();
    for ch in line.chars().chain(std::iter::once(' ')) {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if quote.is_none() && (ch.is_whitespace() || matches!(ch, ';' | '|' | '&')) {
            if matches!(word.as_str(), "merge-tree" | "merge-file") {
                return true;
            }
            word.clear();
        } else if quote.is_none() && ch == '#' && word.is_empty() {
            break;
        } else {
            word.push(ch);
        }
    }
    false
}

/// Exercise the production speculative executor and the verifier's real
/// landing-reconcile flow. Only the remote endpoint is substituted.
fn shell_paths(root: &Path, base: &str, head: &str, union_tree: &str) {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let poller = Path::new(&common).join("storyhook/verification-worktree");
    let marker = root.join("gate-ran");
    let speculative = Command::new("bash")
        .arg(scripts.join("merge-watch.sh"))
        .args([
            "--speculative-run",
            union_tree,
            base,
            head,
            poller.to_str().unwrap(),
            "--",
            "touch",
            marker.to_str().unwrap(),
        ])
        .current_dir(root)
        .env("STORYHOOK_VERIFIER_MIRROR", "0")
        .env("STORYHOOK_LOCK_DIR", root.join("locks"))
        .env_remove("STORYHOOK_MACHINE_LOCKS")
        .output()
        .unwrap();
    assert_eq!(
        speculative.status.code(),
        Some(2),
        "speculative: {speculative:?}"
    );
    assert!(!marker.exists());

    git(root, &["update-ref", "refs/heads/main", base]);
    git(root, &["update-ref", "refs/pull/42/head", head]);
    git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    let bin = root.join("bin");
    storyhook_test_support::install_git_endpoint(
        &bin,
        &[("https://github.com/acme/widgets.git", root)],
    );
    let metadata = serde_json::json!({"number":42, "state":"OPEN", "isDraft":false,
        "isCrossRepository":false, "baseRefName":"main", "headRefName":"feature",
        "headRefOid":head, "mergeCommit":null})
    .to_string();
    let verified = Command::new("bash")
        .arg(scripts.join("verify-pr.sh"))
        .args([
            "--reconcile-land-refusal",
            "0",
            &metadata,
            "42",
            "main",
            head,
            union_tree,
            "fixture landing refusal",
        ])
        .current_dir(root)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("STORY_BIN", storyhook_test_support::story_binary())
        .env("STORYHOOK_VERIFIER_MIRROR", "0")
        .env("STORYHOOK_LOCK_DIR", root.join("locks"))
        .env_remove("STORYHOOK_MACHINE_LOCKS")
        .output()
        .unwrap();
    assert!(verified.status.success(), "verifier: {verified:?}");
    let answer: serde_json::Value =
        serde_json::from_slice(&verified.stdout).expect("verifier JSON");
    assert_eq!(answer["result"], "conflict", "{answer}");
}
