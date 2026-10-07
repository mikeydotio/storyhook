//! Real Cargo metadata and artifacts, with exact native listing and execution.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};
use storyhook::service::attribution::{CargoTarget, ProbeOutcome, RustCase, RustTarget};

const PATIENCE: Duration = Duration::from_secs(120);

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    output: PathBuf,
    metadata: Vec<u8>,
    stream: Vec<u8>,
}

fn cargo(root: &Path, output: &Path, args: &[String]) -> Output {
    let mut cmd = Command::new("cargo");
    // Preserve the host compiler admission used by this repository's own Cargo commands.
    cmd.current_dir(root)
        .args([
            "--config",
            &format!(
                "build.rustc-wrapper={:?}",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/rustc-slot.py")
            ),
        ])
        .args(args)
        .env("CARGO_TARGET_DIR", output)
        .env("CARGO_INCREMENTAL", "0");
    let out = storyhook_test_support::run_bounded(
        cmd,
        "native Cargo artifact fixture",
        storyhook_test_support::load_grace::graced_now(PATIENCE),
    );
    assert!(
        out.status.success(),
        "cargo {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn selected() -> RustCase {
    RustCase::new(
        "subject",
        RustTarget::Integration("contract".into()),
        "answer",
    )
    .unwrap()
}

fn fixture() -> Fixture {
    let directory = tempfile::Builder::new()
        .prefix("sh870-cargo-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = directory.path().join("source");
    let output = directory.path().join("build");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='subject'\nversion='0.1.0'\nedition='2021'\n[profile.dev]\ndebug=0\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = 'subject'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n#[test] fn library_answer() { assert_eq!(answer(), 42); }\n").unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() {}\n#[test] fn binary_answer() { assert_eq!(subject::answer(), 42); }\n",
    )
    .unwrap();
    fs::write(root.join("tests/contract.rs"), "#[test] fn answer() { assert_eq!(subject::answer(), 42); }\n#[test] fn sibling() { panic!(\"must not run\"); }\n").unwrap();
    let args = [
        "metadata",
        "--offline",
        "--locked",
        "--format-version",
        "1",
        "--no-deps",
    ]
    .map(str::to_string);
    let metadata = cargo(&root, &output, &args).stdout;
    let stream = cargo(&root, &output, &selected().build_arguments()).stdout;
    Fixture {
        _directory: directory,
        root,
        output,
        metadata,
        stream,
    }
}

fn deadline() -> Instant {
    Instant::now() + storyhook_test_support::load_grace::graced_now(PATIENCE)
}

#[test]
fn a_real_cargo_target_build_lists_and_runs_only_its_exact_native_case() {
    let f = fixture();
    let case = selected();
    let target = CargoTarget::resolve(&f.root, &case, &f.metadata).unwrap();
    let executable = target
        .executable(&f.output, &f.stream, false, Some(0), deadline())
        .unwrap();
    executable.verify_unchanged().unwrap();
    let listed = Command::new(executable.path())
        .args(case.list_arguments())
        .current_dir(&f.root)
        .output()
        .unwrap();
    case.validate_listing(&listed.stdout, &listed.stderr, false, listed.status.code())
        .unwrap();
    let run = Command::new(executable.path())
        .args(case.run_arguments())
        .current_dir(&f.root)
        .output()
        .unwrap();
    let observed = case.observe(&run.stdout, &run.stderr, false, run.status.code());
    assert_eq!(observed.executions, 1);
    assert_eq!(observed.outcome, ProbeOutcome::Passed);
    executable.verify_unchanged().unwrap();
}

fn metadata_change(f: &Fixture, change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(&f.metadata).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn library_and_binary_cases_resolve_their_own_native_harness() {
    let f = fixture();
    for (kind, name) in [
        (RustTarget::Library, "library_answer"),
        (RustTarget::Binary("subject".into()), "binary_answer"),
    ] {
        let case = RustCase::new("subject", kind, name).unwrap();
        let target = CargoTarget::resolve(&f.root, &case, &f.metadata).unwrap();
        let build = cargo(&f.root, &f.output, &case.build_arguments());
        let exe = target
            .executable(
                &f.output,
                &build.stdout,
                false,
                build.status.code(),
                deadline(),
            )
            .unwrap();
        let listed = Command::new(exe.path())
            .args(case.list_arguments())
            .current_dir(&f.root)
            .output()
            .unwrap();
        case.validate_listing(&listed.stdout, &listed.stderr, false, listed.status.code())
            .unwrap();
        let run = Command::new(exe.path())
            .args(case.run_arguments())
            .current_dir(&f.root)
            .output()
            .unwrap();
        assert_eq!(
            case.observe(&run.stdout, &run.stderr, false, run.status.code())
                .outcome,
            ProbeOutcome::Passed
        );
        exe.verify_unchanged().unwrap();
    }
}

#[test]
fn cargo_target_resolution_refuses_foreign_ambiguous_and_non_native_harnesses() {
    let f = fixture();
    // A valid control keeps this test red against an all-refusal stub.
    CargoTarget::resolve(&f.root, &selected(), &f.metadata).unwrap();
    for data in [
        metadata_change(&f, |m| m["version"] = json!(2)),
        metadata_change(&f, |m| m["workspace_members"] = json!([])),
        metadata_change(&f, |m| {
            m["packages"][0]["source"] = json!("registry+foreign")
        }),
        metadata_change(&f, |m| {
            let p = m["packages"][0].clone();
            m["packages"].as_array_mut().unwrap().push(p);
        }),
        metadata_change(&f, |m| {
            m["packages"][0]["manifest_path"] = json!("/tmp/foreign/Cargo.toml")
        }),
        metadata_change(&f, |m| {
            let a = m["packages"][0]["targets"].as_array_mut().unwrap();
            let t = a.iter().find(|t| t["name"] == "contract").unwrap().clone();
            a.push(t);
        }),
        metadata_change(&f, |m| {
            for t in m["packages"][0]["targets"].as_array_mut().unwrap() {
                if t["name"] == "contract" {
                    t["test"] = json!(false);
                }
            }
        }),
    ] {
        assert!(
            CargoTarget::resolve(&f.root, &selected(), &data).is_err(),
            "accepted {data:?}"
        );
    }
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    fs::create_dir(dir.path().join("tests")).unwrap();
    fs::write(dir.path().join("tests/contract.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='subject'\nversion='0.1.0'\n[[test]]\nname='contract'\nharness=false\n",
    )
    .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut m: Value = serde_json::from_slice(&f.metadata).unwrap();
    m["workspace_root"] = json!(root);
    m["packages"][0]["manifest_path"] = json!(root.join("Cargo.toml"));
    for t in m["packages"][0]["targets"].as_array_mut().unwrap() {
        if t["name"] == "contract" {
            t["src_path"] = json!(root.join("tests/contract.rs"));
        }
    }
    let error = CargoTarget::resolve(&root, &selected(), &serde_json::to_vec(&m).unwrap())
        .err()
        .expect("custom harness must be rejected");
    assert!(error.contains("custom or disabled test harness"), "{error}");
}

fn messages(f: &Fixture) -> Vec<Value> {
    String::from_utf8(f.stream.clone())
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
fn stream(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>()
        .into_bytes()
}
fn artifact(messages: &mut [Value]) -> &mut Value {
    messages
        .iter_mut()
        .find(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "contract")
        .unwrap()
}

#[test]
fn incomplete_ambiguous_or_mismatched_builds_cannot_select_an_executable() {
    let f = fixture();
    let t = CargoTarget::resolve(&f.root, &selected(), &f.metadata).unwrap();
    for (truncated, exit) in [(true, Some(0)), (false, None), (false, Some(101))] {
        assert!(
            t.executable(&f.output, &f.stream, truncated, exit, deadline())
                .is_err()
        );
    }
    for mutation in [
        "duplicate",
        "missing",
        "package",
        "source",
        "profile",
        "manifest",
        "outside",
        "unfinished",
        "failed",
        "trailing",
        "non-json",
        "warning",
        "unlisted",
        "missing-executable",
    ] {
        let mut m = messages(&f);
        match mutation {
            "duplicate" => {
                let a = artifact(&mut m).clone();
                m.insert(0, a);
            }
            "missing" => m.retain(|v| v["target"]["name"] != "contract"),
            "package" => artifact(&mut m)["package_id"] = json!("foreign"),
            "source" => artifact(&mut m)["target"]["src_path"] = json!("/tmp/other.rs"),
            "profile" => artifact(&mut m)["profile"]["test"] = json!(false),
            "manifest" => artifact(&mut m)["manifest_path"] = json!("/tmp/other/Cargo.toml"),
            "outside" => artifact(&mut m)["executable"] = json!("/bin/sh"),
            "unfinished" => {
                m.pop();
            }
            "failed" => m.last_mut().unwrap()["success"] = json!(false),
            "trailing" => m.push(json!({"reason":"build-finished","success":true})),
            "non-json" => {}
            "warning" => m.insert(
                0,
                json!({"reason":"compiler-message","message":{"level":"warning"}}),
            ),
            "unlisted" => artifact(&mut m)["filenames"] = json!([]),
            "missing-executable" => artifact(&mut m)["executable"] = Value::Null,
            _ => unreachable!(),
        }
        let mut bytes = stream(&m);
        if mutation == "non-json" {
            bytes.extend_from_slice(b"unstructured output\n");
        }
        assert!(
            t.executable(&f.output, &bytes, false, Some(0), deadline())
                .is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn an_expired_diagnosis_cannot_retain_an_executable() {
    let f = fixture();
    let t = CargoTarget::resolve(&f.root, &selected(), &f.metadata).unwrap();
    assert!(
        t.executable(&f.output, &f.stream, false, Some(0), Instant::now())
            .is_err()
    );
}

#[test]
fn artifact_recheck_detects_replacement_content_mode_and_symlink_changes() {
    let f = fixture();
    let t = CargoTarget::resolve(&f.root, &selected(), &f.metadata).unwrap();
    for change in ["contents", "mode", "replace", "link"] {
        let d = tempfile::tempdir_in("/tmp").unwrap();
        let path = d.path().join("case");
        let mut m = messages(&f);
        let source = artifact(&mut m)["executable"].as_str().unwrap().to_string();
        fs::copy(&source, &path).unwrap();
        artifact(&mut m)["executable"] = json!(path);
        artifact(&mut m)["filenames"] = json!([path]);
        let executable = t
            .executable(d.path(), &stream(&m), false, Some(0), deadline())
            .unwrap();
        executable.verify_unchanged().unwrap();
        match change {
            "contents" => fs::write(&path, b"changed").unwrap(),
            "mode" => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            "replace" => {
                fs::remove_file(&path).unwrap();
                fs::copy(&source, &path).unwrap();
            }
            "link" => {
                fs::remove_file(&path).unwrap();
                symlink(&source, &path).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(executable.verify_unchanged().is_err(), "{change}");
    }
}
