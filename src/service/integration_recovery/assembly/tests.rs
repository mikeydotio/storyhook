//! Private native Git fixtures; no daemon, provider, remote or live-store action.
use super::*;
use crate::process::run_captured_query;
use crate::service::integration_recovery::{Inspection, inspect};
use std::{cell::Cell, collections::BTreeMap, os::unix::fs::symlink, time::Duration};

fn deadline() -> Instant {
    Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60))
}

fn git(root: &Path, args: &[&str]) -> String {
    let mut command = crate::env::git_env::command(root);
    command.args(args);
    let result = run_captured_query(
        command,
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
        &|| false,
        ANSWER_LIMIT,
        &[],
    )
    .unwrap_or_else(|error| panic!("fixture Git: {}", error.detail()));
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!result.stdout_truncated);
    text(&result.stdout).unwrap()
}

fn commit(root: &Path, message: &str) -> String {
    git(root, &["add", "."]);
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", message],
    );
    git(root, &["rev-parse", "HEAD"])
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    workspace: PathBuf,
    submission: SubmissionObservation,
    plan: IntegrationPlan,
}

impl Fixture {
    fn new() -> Self {
        let root = storyhook_test_support::scratch_dir();
        let base_dir = root.path().canonicalize().unwrap();
        let source = base_dir.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-q", "--initial-branch=dev"]);
        storyhook_test_support::approve_fixture_identity(
            &source,
            "Actual Assembly Fixture",
            "assembly@example.test",
        );
        git(
            &source,
            &["config", "--local", "user.name", "Actual Assembly Fixture"],
        );
        git(
            &source,
            &["config", "--local", "user.email", "assembly@example.test"],
        );
        fs::create_dir(source.join("docs")).unwrap();
        fs::write(source.join("docs/guide.md"), "start\nend\n").unwrap();
        fs::write(source.join("unrelated.txt"), "unchanged\n").unwrap();
        fs::write(source.join(".storyhook.toml"), "schema = 1\nuuid = \"fixture\"\nprefix = \"SH\"\n[integration]\nversion = 1\nenabled = true\npublication = \"managed-pr\"\nsmooth = [\"docs/\"]\n").unwrap();
        let common = commit(&source, "common");
        fs::write(source.join("docs/guide.md"), "start\nbase addition\nend\n").unwrap();
        let base = commit(&source, "base addition");
        git(&source, &["checkout", "-q", "-b", "author", &common]);
        fs::write(
            source.join("docs/guide.md"),
            "start\nauthor addition\nend\n",
        )
        .unwrap();
        let head = commit(&source, "author addition");
        // Local and uncommitted attributes/config must not influence the merge.
        fs::write(
            source.join(".git/info/attributes"),
            "* merge=ours filter=host\n",
        )
        .unwrap();
        git(&source, &["config", "merge.ours.driver", "false"]);
        git(&source, &["config", "filter.host.clean", "false"]);
        fs::write(source.join("dirty.txt"), "author work stays\n").unwrap();
        let Inspection::Proposed(proof) =
            inspect(&source, &base, &head, deadline(), Cancellation::default()).unwrap()
        else {
            panic!("fixture requires a real native insertion proposal")
        };
        let plan = proof.settle().unwrap();
        let submission = SubmissionObservation {
            checkout: source.clone(),
            repository: "github.com/acme/widgets".into(),
            pull_request: "https://github.com/acme/widgets/pull/7".into(),
            base_branch: "dev".into(),
            base,
            head,
        };
        Self {
            _root: root,
            source,
            workspace: base_dir.join("private/integration"),
            submission,
            plan,
        }
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            owner: "native-fixture-owner",
            epoch: 1,
            branch: "storyhook/integration/native-fixture-owner",
            workspace: &self.workspace,
            submission: &self.submission,
            plan: &self.plan,
        }
    }
}

#[test]
fn sh871_native_assembly_retains_both_parents_actual_identity_and_self_contained_objects() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.source);
    let calls = Cell::new(0);
    let permit = || {
        calls.set(calls.get() + 1);
        Ok(())
    };
    let assembled = assemble(
        fixture.inputs(),
        &permit,
        deadline(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(
        calls.get() > 20,
        "effects did not recheck durable permission"
    );
    assert_eq!(
        snapshot(&fixture.source),
        before,
        "author files, index, refs, config or objects changed"
    );
    let evidence = assembled.evidence();
    assert_eq!(evidence.plan, fixture.plan);
    assert_eq!(
        git(
            &fixture.workspace,
            &["show", "-s", "--format=%P", &evidence.commit]
        ),
        format!("{} {}", fixture.plan.base, fixture.plan.head)
    );
    assert_eq!(
        git(
            &fixture.workspace,
            &[
                "show",
                "-s",
                "--format=%an <%ae>%n%cn <%ce>",
                &evidence.commit
            ]
        ),
        "Actual Assembly Fixture <assembly@example.test>\nActual Assembly Fixture <assembly@example.test>"
    );
    assert_eq!(
        git(
            &fixture.workspace,
            &["show", &format!("{}:docs/guide.md", evidence.commit)]
        ),
        "start\nbase addition\nauthor addition\nend"
    );
    assert_eq!(
        git(
            &fixture.workspace,
            &["show", &format!("{}:unrelated.txt", evidence.commit)]
        ),
        "unchanged"
    );
    assert!(!fixture.workspace.join("objects/info/alternates").exists());
    assert!(!fixture.workspace.join("FETCH_HEAD").exists());
    assert!(
        fs::read_dir(fixture.workspace.join("refs/heads"))
            .unwrap()
            .next()
            .is_none()
    );
    // Removing the source pathname proves the receipt has no alternate dependency.
    fs::rename(
        &fixture.source,
        fixture.source.with_file_name("source-kept"),
    )
    .unwrap();
    assembled.validate_custody().unwrap();
    git(
        &fixture.workspace,
        &[
            "fsck",
            "--connectivity-only",
            "--no-reflogs",
            &evidence.commit,
        ],
    );
    let saved = serde_json::to_value(evidence).unwrap();
    assert_eq!(saved["epoch"], 1);
    assert_eq!(saved["plan"]["head"], fixture.plan.head);
}

#[test]
fn sh871_native_assembly_refuses_existing_and_symlink_workspaces_without_adoption() {
    for link in [false, true] {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.workspace.parent().unwrap()).unwrap();
        let existing = fixture.workspace.with_file_name("existing");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("keep"), "existing owner").unwrap();
        if link {
            symlink(&existing, &fixture.workspace).unwrap();
        } else {
            fs::rename(&existing, &fixture.workspace).unwrap();
        }
        let count = crate::env::git_env::built_on_this_thread();
        assert!(
            assemble(
                fixture.inputs(),
                &|| Ok(()),
                deadline(),
                &Cancellation::default()
            )
            .is_err()
        );
        assert_eq!(crate::env::git_env::built_on_this_thread(), count);
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("keep")).unwrap(),
            "existing owner"
        );
        assert!(!fixture.workspace.join(STAMP).exists());
    }
}

#[test]
fn sh871_native_assembly_cancelled_or_expired_claim_starts_no_child_or_workspace() {
    let fixture = Fixture::new();
    for expired in [false, true] {
        let cancellation = Cancellation::default();
        if !expired {
            cancellation.cancel();
        }
        let bound = if expired { Instant::now() } else { deadline() };
        let count = crate::env::git_env::built_on_this_thread();
        assert!(assemble(fixture.inputs(), &|| Ok(()), bound, &cancellation).is_err());
        assert_eq!(crate::env::git_env::built_on_this_thread(), count);
        assert!(!fixture.workspace.exists());
    }
}

#[test]
fn sh871_native_assembly_recomputes_policy_source_and_resolution_before_commit() {
    for field in ["policy", "source", "resolution", "tree"] {
        let mut fixture = Fixture::new();
        match field {
            "policy" => fixture.plan.policy = "changed policy".into(),
            "source" => fixture.plan.files[0].ours = "a".repeat(40),
            "resolution" => fixture.plan.files[0].resolved_sha256 = "b".repeat(64),
            "tree" => fixture.plan.conflicted_tree = "c".repeat(40),
            _ => unreachable!(),
        }
        let before = snapshot(&fixture.source);
        assert!(
            assemble(
                fixture.inputs(),
                &|| Ok(()),
                deadline(),
                &Cancellation::default()
            )
            .is_err(),
            "accepted changed {field}"
        );
        assert!(
            fixture.workspace.join(STAMP).exists(),
            "failure lost reconciliation custody"
        );
        assert!(
            !fixture.workspace.join("assembly.index").exists(),
            "resolution began before proposal equality"
        );
        assert_eq!(snapshot(&fixture.source), before);
    }
}

#[test]
fn sh871_native_assembly_revoked_effect_retains_workspace_without_later_writes() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.source);
    let permit = || {
        if fixture.workspace.join("assembly.index").exists() {
            Err(refuse("fixture revoked claim"))
        } else {
            Ok(())
        }
    };
    assert!(
        assemble(
            fixture.inputs(),
            &permit,
            deadline(),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(fixture.workspace.join("assembly.index").exists());
    assert!(!fixture.workspace.join("resolved-0.blob").exists());
    assert!(fixture.workspace.join(STAMP).exists());
    assert_eq!(snapshot(&fixture.source), before);
    assert!(
        assemble(
            fixture.inputs(),
            &|| Ok(()),
            deadline(),
            &Cancellation::default()
        )
        .is_err(),
        "replayed uncertain prior effects"
    );
}

#[test]
fn sh871_native_assembly_replacement_after_stamp_never_receives_git_writes() {
    let fixture = Fixture::new();
    let replaced = Cell::new(false);
    let permit = || {
        if fixture.workspace.join(STAMP).exists() && !replaced.replace(true) {
            fs::rename(
                &fixture.workspace,
                fixture.workspace.with_file_name("original-kept"),
            )
            .unwrap();
            fs::create_dir(&fixture.workspace).unwrap();
            fs::write(fixture.workspace.join("keep"), "replacement owner").unwrap();
        }
        Ok(())
    };
    assert!(
        assemble(
            fixture.inputs(),
            &permit,
            deadline(),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(replaced.get());
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("keep")).unwrap(),
        "replacement owner"
    );
    assert!(!fixture.workspace.join("objects").exists());
    assert!(
        fixture
            .workspace
            .with_file_name("original-kept")
            .join(STAMP)
            .exists()
    );
}

#[test]
fn sh871_native_assembly_stamp_content_and_inode_are_both_required() {
    for replace_inode in [false, true] {
        let fixture = Fixture::new();
        let assembled = assemble(
            fixture.inputs(),
            &|| Ok(()),
            deadline(),
            &Cancellation::default(),
        )
        .unwrap();
        let path = fixture.workspace.join(STAMP);
        let original = fs::read(&path).unwrap();
        if replace_inode {
            fs::rename(&path, path.with_extension("original")).unwrap();
            fs::write(&path, &original).unwrap();
        } else {
            let mut changed = original;
            changed[0] = b' ';
            fs::write(&path, changed).unwrap();
        }
        assert!(assembled.validate_custody().is_err());
        assert!(fixture.workspace.exists(), "uncertain resource was removed");
    }
}

#[test]
fn sh871_native_assembly_pr_binding_preserves_host_port_and_number() {
    let original = "https://git.example.test:8443/acme/widgets/pull/7";
    assert!(
        same_pr(
            original,
            "https://GIT.example.test:8443/ACME/widgets/pull/7"
        )
        .unwrap()
    );
    for replacement in [
        "https://git.example.test/acme/widgets/pull/7",
        "https://other.example.test:8443/acme/widgets/pull/7",
        "https://git.example.test:8443/acme/other/pull/7",
        "https://git.example.test:8443/acme/widgets/pull/8",
    ] {
        assert!(!same_pr(original, replacement).unwrap());
    }
    assert!(same_pr(original, "invalid").is_err());
}

#[test]
fn sh871_native_assembly_refuses_workspace_inside_author_checkout_or_common_git() {
    for linked in [false, true] {
        let mut fixture = Fixture::new();
        let original = fixture.source.clone();
        if linked {
            let worktree = original.with_file_name("linked-author");
            git(
                &original,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    worktree.to_str().unwrap(),
                    &fixture.plan.head,
                ],
            );
            fixture.source = worktree.canonicalize().unwrap();
            fixture.submission.checkout = fixture.source.clone();
            fixture.workspace = original.join(".git/private-integration");
        } else {
            fixture.workspace = original.join("private-integration");
        }
        let before = snapshot(&original);
        assert!(
            assemble(
                fixture.inputs(),
                &|| Ok(()),
                deadline(),
                &Cancellation::default()
            )
            .is_err()
        );
        assert!(!fixture.workspace.exists());
        assert_eq!(snapshot(&original), before);
    }
}

#[test]
fn sh871_native_assembly_refuses_git_namespace_redirects_without_foreign_writes() {
    for redirect in [
        ".git",
        "commondir",
        "objects/info/alternates",
        "objects/pack",
    ] {
        let fixture = Fixture::new();
        let foreign = fixture.source.with_file_name("foreign.git");
        fs::create_dir(&foreign).unwrap();
        git(&foreign, &["init", "--bare", "--quiet", "."]);
        let before = snapshot(&foreign);
        let injected = Cell::new(false);
        let permit = || {
            if fixture.workspace.join("HEAD").exists()
                && fixture.workspace.join("objects/info").is_dir()
                && !injected.replace(true)
            {
                let target = if redirect == "objects/info/alternates" {
                    foreign.join("objects")
                } else {
                    foreign.clone()
                };
                let content = if redirect == ".git" {
                    format!("gitdir: {}\n", target.display())
                } else {
                    format!("{}\n", target.display())
                };
                if redirect == "objects/pack" {
                    fs::rename(
                        fixture.workspace.join(redirect),
                        fixture.workspace.join("original-pack"),
                    )
                    .unwrap();
                    symlink(
                        foreign.join("objects/pack"),
                        fixture.workspace.join(redirect),
                    )
                    .unwrap();
                } else {
                    fs::write(fixture.workspace.join(redirect), content).unwrap();
                }
            }
            Ok(())
        };
        assert!(
            assemble(
                fixture.inputs(),
                &permit,
                deadline(),
                &Cancellation::default()
            )
            .is_err(),
            "accepted {redirect}"
        );
        assert!(injected.get(), "did not reach private init for {redirect}");
        assert_eq!(
            snapshot(&foreign),
            before,
            "private Git wrote through {redirect}"
        );
        assert!(fixture.workspace.join(STAMP).exists());
    }
}

#[test]
fn sh871_native_assembly_rejects_substituted_staged_resolution_bytes() {
    let fixture = Fixture::new();
    let before = snapshot(&fixture.source);
    let injected = Cell::new(false);
    let permit = || {
        let staged = fixture.workspace.join("resolved-0.blob");
        if staged.exists() && !injected.replace(true) {
            fs::write(staged, "start\nsubstituted bytes\nend\n").unwrap();
        }
        Ok(())
    };
    assert!(
        assemble(
            fixture.inputs(),
            &permit,
            deadline(),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(injected.get());
    assert!(fixture.workspace.join(STAMP).exists());
    assert_eq!(snapshot(&fixture.source), before);
}

#[test]
fn sh871_native_assembly_rejects_substituted_final_index_blob_and_mode() {
    for mode_change in [false, true] {
        let fixture = Fixture::new();
        let expected_file = fixture.source.with_file_name("expected-resolution");
        fs::write(
            &expected_file,
            "start\nbase addition\nauthor addition\nend\n",
        )
        .unwrap();
        // Hash without -w: this setup must not add an object to the author.
        let expected_oid = git(
            &fixture.source,
            &[
                "hash-object",
                "--no-filters",
                expected_file.to_str().unwrap(),
            ],
        );
        let raw_oid: Vec<u8> = expected_oid
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let before = snapshot(&fixture.source);
        let injected = Cell::new(false);
        let permit = || {
            let index = fixture.workspace.join("assembly.index");
            if !fixture.workspace.join("assembly.index.lock").exists()
                && fs::read(&index).is_ok_and(|bytes| {
                    bytes
                        .windows(raw_oid.len())
                        .any(|window| window == raw_oid.as_slice())
                })
                && !injected.replace(true)
            {
                let (mode, oid) = if mode_change {
                    ("100755", expected_oid.as_str())
                } else {
                    ("100644", fixture.plan.files[0].theirs.as_str())
                };
                let mut command = crate::env::git_env::command(&fixture.workspace);
                command.env("GIT_INDEX_FILE", &index).args([
                    "update-index",
                    "--cacheinfo",
                    &format!("{mode},{oid},docs/guide.md"),
                ]);
                let changed =
                    run_captured_query_quiescent(command, deadline(), &|| false, ANSWER_LIMIT, &[])
                        .unwrap_or_else(|error| {
                            panic!("fixture index injection: {}", error.detail())
                        });
                assert!(
                    changed.status.success(),
                    "{}",
                    String::from_utf8_lossy(&changed.stderr)
                );
            }
            Ok(())
        };
        assert!(
            assemble(
                fixture.inputs(),
                &permit,
                deadline(),
                &Cancellation::default()
            )
            .is_err(),
            "accepted changed mode={mode_change}"
        );
        assert!(injected.get(), "did not reach approved private index");
        assert!(fixture.workspace.join(STAMP).exists());
        assert_eq!(snapshot(&fixture.source), before);
    }
}

#[test]
fn sh871_native_assembly_never_accepts_leader_success_with_a_live_writer() {
    let fixture = Fixture::new();
    let root = fixture.source.parent().unwrap();
    let ready = root.join("writer-ready");
    let release = root.join("writer-release");
    let finished = root.join("writer-finished");
    let allowance = storyhook_test_support::load_grace::graced_now(Duration::from_secs(5));
    let cancellation = Cancellation::default();
    let op = Operation {
        permitted: &|| Ok(()),
        deadline: Instant::now() + allowance,
        cancellation: &cancellation,
        source: PinnedDirectory::open(&fixture.source).unwrap(),
        preparing: vec![],
        custody: None,
    };
    let mut command = std::process::Command::new("sh");
    command.args(["-c",
        "(printf ready > \"$1\"; n=0; while [ ! -e \"$2\" ] && [ \"$n\" -lt \"$4\" ]; do sleep 0.02; n=$((n+1)); done; printf late > \"$3\") & while [ ! -e \"$1\" ]; do sleep 0.01; done; printf leader; exit 0",
        "assembly-writer-fixture"])
        .arg(&ready).arg(&release).arg(&finished)
        .arg((allowance.as_millis()/10+100).to_string());
    let result = op.capture(command, &[]);
    // A negative control using leader-only capture returns Ok. Release the
    // fixture's finite, sentinel-controlled writer before asserting failure;
    // do not signal a reaped PID/group or leave it writing into a removed root.
    if result.is_ok() {
        fs::write(&release, "release").unwrap();
        storyhook_test_support::load_grace::wait_for(
            storyhook_test_support::load_grace::Patience::new(Duration::from_secs(10)),
            Duration::from_millis(10),
            || "late fixture writer did not finish".into(),
            || finished.exists().then_some(()),
        );
    }
    assert!(
        ready.exists(),
        "fixture never established its writer before the deadline"
    );
    assert!(
        result.is_err(),
        "leader success was accepted while its writer remained live"
    );
}

fn assembled_fixture(fixture: &Fixture) -> NativeAssembly {
    assemble(
        fixture.inputs(),
        &|| Ok(()),
        deadline(),
        &Cancellation::default(),
    )
    .unwrap()
}

#[test]
fn sh871_live_assembly_settlement_removes_only_original_private_root() {
    let fixture = Fixture::new();
    let source = snapshot(&fixture.source);
    let assembled = assembled_fixture(&fixture);
    let sibling = fixture.workspace.with_file_name("another-owner");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("keep"), "unrelated").unwrap();
    let git_calls = crate::env::git_env::built_on_this_thread();
    assembled.settle().unwrap();
    assert!(!fixture.workspace.exists());
    assert!(fixture.workspace.parent().unwrap().is_dir());
    assert_eq!(
        fs::read_to_string(sibling.join("keep")).unwrap(),
        "unrelated"
    );
    assert_eq!(snapshot(&fixture.source), source);
    assert_eq!(crate::env::git_env::built_on_this_thread(), git_calls);
}

#[test]
fn sh871_live_assembly_settlement_refuses_replaced_administration_and_objects() {
    for relative in ["config", "HEAD", "assembly.index", STAMP, "objects"] {
        let fixture = Fixture::new();
        let assembled = assembled_fixture(&fixture);
        let path = fixture.workspace.join(relative);
        let retained = fixture
            .workspace
            .with_file_name(format!("retained-{}", relative.replace('/', "-")));
        fs::rename(&path, &retained).unwrap();
        if retained.is_dir() {
            fs::create_dir(&path).unwrap();
            fs::write(path.join("keep"), "replacement").unwrap();
        } else {
            fs::write(&path, fs::read(&retained).unwrap()).unwrap();
        }
        let before = snapshot(&fixture.workspace);
        let error = assembled.settle().unwrap_err().to_string();
        assert!(error.contains("removed 0 entries"), "{relative}: {error}");
        assert_eq!(snapshot(&fixture.workspace), before, "{relative}");
        assert!(retained.exists());
    }
}

#[test]
fn sh871_live_assembly_settlement_refuses_new_files_redirects_and_locks() {
    for relative in [
        "operator-note",
        "objects/info/alternates",
        "info/grafts",
        "assembly.index.lock",
    ] {
        let fixture = Fixture::new();
        let assembled = assembled_fixture(&fixture);
        let path = fixture.workspace.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "keep unknown ownership").unwrap();
        let before = snapshot(&fixture.workspace);
        assert!(
            assembled
                .settle()
                .unwrap_err()
                .to_string()
                .contains("removed 0 entries")
        );
        assert_eq!(snapshot(&fixture.workspace), before);
    }
}

#[test]
fn sh871_live_assembly_settlement_does_not_follow_injected_symlink() {
    let fixture = Fixture::new();
    let assembled = assembled_fixture(&fixture);
    let before = snapshot(&fixture.source);
    symlink(&fixture.source, fixture.workspace.join("foreign")).unwrap();
    assert!(
        assembled
            .settle()
            .unwrap_err()
            .to_string()
            .contains("removed 0 entries")
    );
    assert_eq!(snapshot(&fixture.source), before);
    assert!(fixture.workspace.join(STAMP).is_file());
    assert!(
        fs::symlink_metadata(fixture.workspace.join("foreign"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn sh871_live_assembly_settlement_refuses_renamed_parent_or_root() {
    for rename_parent in [false, true] {
        let fixture = Fixture::new();
        let assembled = assembled_fixture(&fixture);
        let original = if rename_parent {
            fixture.workspace.parent().unwrap().to_path_buf()
        } else {
            fixture.workspace.clone()
        };
        let retained = original.with_file_name("retained-native-root");
        fs::rename(&original, &retained).unwrap();
        fs::create_dir_all(&fixture.workspace).unwrap();
        fs::write(fixture.workspace.join("keep"), "foreign replacement").unwrap();
        assert!(
            assembled
                .settle()
                .unwrap_err()
                .to_string()
                .contains("removed 0 entries")
        );
        assert_eq!(
            fs::read_to_string(fixture.workspace.join("keep")).unwrap(),
            "foreign replacement"
        );
        let original_stamp = if rename_parent {
            retained.join("integration").join(STAMP)
        } else {
            retained.join(STAMP)
        };
        assert!(original_stamp.is_file());
    }
}

#[test]
fn sh871_live_assembly_settlement_reports_partial_failure_and_keeps_stamp() {
    let fixture = Fixture::new();
    let assembled = assembled_fixture(&fixture);
    let error = assembled
        .settle_with(|removed| {
            if removed == 1 {
                Err(refuse("fixture interruption after one exact unlink"))
            } else {
                Ok(())
            }
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("removed 1 entries"), "{error}");
    assert!(error.contains("cleanup incomplete"));
    assert!(fixture.workspace.join(STAMP).is_file());
}

#[test]
fn sh871_live_assembly_settlement_keeps_inflight_unknown_addition() {
    let fixture = Fixture::new();
    let assembled = assembled_fixture(&fixture);
    let unknown = fixture.workspace.join("keep-new-owner");
    let error = assembled
        .settle_with(|removed| {
            if removed == 0 {
                fs::write(&unknown, "new owner").map_err(storage)?;
            }
            Ok(())
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("unexpected entry appeared"), "{error}");
    assert_eq!(fs::read_to_string(unknown).unwrap(), "new owner");
    assert!(fixture.workspace.join(STAMP).is_file());
}

#[test]
fn sh871_live_assembly_drop_retains_native_workspace_without_cleanup() {
    let fixture = Fixture::new();
    let assembled = assembled_fixture(&fixture);
    let before = snapshot(&fixture.workspace);
    drop(assembled);
    assert_eq!(snapshot(&fixture.workspace), before);
}
