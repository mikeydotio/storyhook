//! Owned local fixtures only; no provider, daemon, network or ambient Git shim.
use super::*;
use std::{thread, time::Duration};

fn allowance() -> Duration {
    storyhook_test_support::load_grace::graced_now(Duration::from_secs(30))
}
fn fixture() -> (tempfile::TempDir, NativeObjects) {
    let source = storyhook_test_support::scratch_dir();
    let mut init = crate::env::git_env::command(source.path());
    init.args(["init", "--bare", "--quiet", "--template=", "."]);
    let result =
        run_captured_query_quiescent(init, Instant::now() + allowance(), &|| false, LIMIT, &[])
            .unwrap();
    assert!(result.status.success());
    let native = NativeObjects::open(
        source.path(),
        "native cleanup fixture",
        "storyhook-native-owned-test-",
        Instant::now() + allowance(),
        Cancellation::default(),
    )
    .unwrap();
    (source, native)
}

#[test]
fn sh871_controlled_native_close_removes_both_object_and_admin_namespaces() {
    let (_source, native) = fixture();
    let root = native.root.as_ref().unwrap().path.clone();
    assert!(root.join("admin/config").is_file());
    let answer = native
        .query(
            &["hash-object", "-t", "tree", "-w", "--stdin"],
            &[],
            &[],
            None,
            &|| false,
        )
        .unwrap();
    assert!(answer.status.success());
    assert!(root.join("objects").is_dir());
    native.close().unwrap();
    assert!(!root.exists());
}

#[test]
fn sh871_controlled_native_admin_replacement_refuses_explicit_cleanup() {
    let (_source, native) = fixture();
    let root = native.root.as_ref().unwrap().path.clone();
    fs::rename(root.join("admin"), root.join("retained-admin")).unwrap();
    fs::create_dir(root.join("admin")).unwrap();
    fs::write(root.join("admin/foreign-sentinel"), "retain").unwrap();
    let error = native.close().unwrap_err().to_string();
    assert!(error.contains("retained at"), "{error}");
    assert!(root.join("retained-admin/config").is_file());
    assert_eq!(
        fs::read_to_string(root.join("admin/foreign-sentinel")).unwrap(),
        "retain"
    );
    // The fixture created the replacement after all initialization captures
    // settled. It owns both trees; production refused to adopt either for removal.
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sh871_controlled_native_capture_error_retains_and_names_original_roots() {
    let (_source, native) = fixture();
    let root = native.root.as_ref().unwrap().path.clone();
    let missing = root.join("definitely-missing-executable");
    assert!(!missing.exists());
    let error = native
        .capture(Command::new(&missing), &[], None, &|| false)
        .err()
        .unwrap()
        .to_string();
    assert!(native.uncertain.get());
    assert!(error.contains(root.to_str().unwrap()), "{error}");
    assert!(
        native
            .close()
            .unwrap_err()
            .to_string()
            .contains("cleanup refused")
    );
    assert!(root.join("objects").is_dir());
    assert!(root.join("admin/config").is_file());
    // This detector knows no child could spawn: the exact absolute executable
    // did not exist. Production deliberately retains on every capture error.
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sh871_controlled_native_expired_owner_never_starts_a_new_command() {
    let (_source, mut native) = fixture();
    let root = native.root.as_ref().unwrap().path.clone();
    let marker = root.join("must-not-start");
    native.deadline = Instant::now();
    let mut command = Command::new("sh");
    command
        .args(["-c", "printf started > \"$1\"", "native-expired-fixture"])
        .arg(&marker);
    assert!(native.capture(command, &[], None, &|| false).is_err());
    assert!(!native.uncertain.get());
    assert!(!marker.exists());
    assert!(native.close().is_err()); // expired observations cannot become proofs
    assert!(!root.exists()); // known-quiescent cleanup still completed explicitly
}

#[test]
fn sh871_controlled_native_leader_exit_cannot_close_a_live_writer_namespace() {
    let (source, mut native) = fixture();
    // No destructor may remove marker paths while an injected writer might
    // still borrow them, including assertion/error unwinding in this test.
    let source = source.keep();
    let root = native.root.as_ref().unwrap().path.clone();
    let ready = source.join("ready");
    let pid_file = source.join("writer-pid");
    let release = source.join("release");
    let finished = source.join("finished");
    let bound = storyhook_test_support::load_grace::graced_now(Duration::from_secs(2));
    native.deadline = Instant::now() + bound;
    let mut command = Command::new("sh");
    command.args(["-c", "(printf ready > \"$1\"; n=0; while [ ! -e \"$2\" ] && [ \"$n\" -lt \"$5\" ]; do sleep 0.02; n=$((n+1)); done; printf late > \"$3\") & printf '%s' \"$!\" > \"$4\"; while [ ! -e \"$1\" ]; do sleep 0.01; done; printf leader; exit 0", "native-writer-fixture"])
        .arg(&ready).arg(&release).arg(&finished).arg(&pid_file)
        .arg((bound.as_millis() / 10 + 100).to_string());
    let result = native.capture(command, &[], None, &|| false);
    // A leader-only mutant returns success. Release its finite fixture writer
    // before asserting, then prove absence without signaling a reaped identity.
    fs::write(&release, "release").unwrap();
    let pid = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|pid| *pid > 0);
    let end = Instant::now() + allowance();
    let absent = loop {
        let gone = pid.is_some_and(|pid| {
            // Signal zero only observes this fixture-recorded PID. A reused or
            // inaccessible PID causes retention, never an unrelated kill.
            (unsafe { libc::kill(pid, 0) }) < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        if gone {
            break true;
        }
        if Instant::now() >= end {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let established = ready.exists();
    let settled = native.close();
    if !absent {
        panic!(
            "writer census uncertain; retain marker root {} and native root {}: {settled:?}",
            source.display(),
            root.display()
        );
    }
    let retained = root.exists();
    if retained {
        fs::remove_dir_all(&root).unwrap();
    } // exact test writer is absent
    fs::remove_dir_all(source).unwrap();
    assert!(established, "fixture did not establish its writer");
    assert!(
        result.is_err(),
        "leader-only success accepted a still-live writer"
    );
    assert!(
        settled.is_err(),
        "ambiguous capture was treated as settled cleanup"
    );
    assert!(
        retained,
        "potential writer roots were deleted before explicit reconciliation"
    );
}
