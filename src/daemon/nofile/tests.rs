use super::*;
use std::collections::VecDeque;
use std::os::unix::ffi::OsStrExt;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, daemon_containment, scratch_dir};

const CHILD: &str = "daemon::nofile::tests::descriptor_policy_child_probe";

struct FakeKernel {
    reads: VecDeque<std::io::Result<Limits>>,
    writes: Vec<Limits>,
    fail_write: bool,
}

impl Kernel for FakeKernel {
    fn read(&mut self) -> std::io::Result<Limits> {
        self.reads.pop_front().expect("unexpected kernel read")
    }

    fn write(&mut self, limits: Limits) -> std::io::Result<()> {
        self.writes.push(limits);
        if self.fail_write {
            Err(std::io::Error::from_raw_os_error(libc::EPERM))
        } else {
            Ok(())
        }
    }
}

fn managed(env: &Environment) -> String {
    let path =
        super::super::agent::ExecutionPath::parse(Some(OsStr::new("/usr/bin:/bin"))).unwrap();
    super::super::agent::plist(&std::env::current_exe().unwrap(), env, &path)
}

#[test]
fn descriptor_target_never_lowers_soft_or_changes_hard() {
    for (soft, hard, expected) in [
        (256, libc::RLIM_INFINITY, Some(1024)),
        (256, 512, Some(512)),
        (256, 256, None),
        (0, 0, None),
        (1024, 4096, None),
        (1_048_575, libc::RLIM_INFINITY, None),
        (libc::RLIM_INFINITY, libc::RLIM_INFINITY, None),
    ] {
        let before = Limits { soft, hard };
        assert_eq!(target(before), expected.map(|soft| Limits { soft, hard }));
    }
}

#[test]
fn descriptor_mode_accepts_inherit_and_rejects_unknown_without_echoing_it() {
    assert_eq!(automatic(None), Ok(true));
    assert_eq!(automatic(Some(OsStr::new("auto"))), Ok(true));
    assert_eq!(automatic(Some(OsStr::new("inherit"))), Ok(false));
    for value in [
        OsStr::new("private-invalid-value"),
        OsStr::from_bytes(&[0xff]),
    ] {
        let error = automatic(Some(value)).unwrap_err();
        assert!(error.contains("auto or inherit"));
        assert!(!error.contains("private-invalid-value"));
    }
}

#[test]
fn descriptor_service_proof_preserves_explicit_foreign_and_custom_definitions() {
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let text = managed(&env);
    let exe = std::env::current_exe().unwrap();
    canonical_service(&text, &exe, &env).unwrap();
    // The durable PATH comes from the plist, not the current process PATH.
    let different_path = text.replace("/usr/bin:/bin", "/opt/provider/bin:/usr/bin");
    canonical_service(&different_path, &exe, &env).unwrap();
    let special = super::super::agent::ExecutionPath::parse(Some(OsStr::new(
        "/tools & <bin>:/with spaces:/duplicate:/duplicate:/carriage\rreturn",
    )))
    .unwrap();
    let special_text = super::super::agent::plist(&exe, &env, &special);
    canonical_service(&special_text, &exe, &env).unwrap();
    for changed in [
        text.replace("    <key>RunAtLoad</key>", "    <key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>128</integer></dict>\n    <key>RunAtLoad</key>"),
        text.replace("    <key>RunAtLoad</key>", "    <key>HardResourceLimits</key><dict><key>NumberOfFiles</key><integer>256</integer></dict>\n    <key>RunAtLoad</key>"),
        text.replace("    <key>RunAtLoad</key>", "    <key>Label</key><string>duplicate</string>\n    <key>RunAtLoad</key>"),
        text.replace("    <key>RunAtLoad</key>", "    <key>WorkingDirectory</key><string>/private/tmp</string>\n    <key>RunAtLoad</key>"),
        text.replace("<key>Label</key>", "<key>UnknownLabel</key>"),
        text.replace("<key>PATH</key>", "<key>UNKNOWN_PATH</key>"),
        text.replace("<true/>", "<false/>"),
        text.replace("    <key>", "  <key>"),
        "bplist00".to_string(),
        text.replace("</plist>", ""),
    ] {
        assert!(canonical_service(&changed, &exe, &env).is_err());
    }
    let other = Environment::at(root.path().join("other-home"));
    assert!(canonical_service(&text, &exe, &other).is_err());
    assert!(canonical_service(&text, Path::new("/bin/ls"), &env).is_err());
}

#[test]
fn descriptor_service_read_refuses_missing_symlink_and_oversized_definitions() {
    let root = scratch_dir();
    let env = Environment::at(root.path());
    assert!(permitted(&env, Some("launchd"), None).is_err());
    assert_eq!(
        permitted(&env, Some("launchd"), Some(OsStr::new("inherit"))),
        Ok(false)
    );
    let path = super::super::agent::path(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let target = root.path().join("managed.plist");
    std::fs::write(&target, managed(&env)).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(permitted(&env, Some("launchd"), None).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "x".repeat(64 * 1024 + 1)).unwrap();
    assert!(permitted(&env, Some("launchd"), None).is_err());
    std::fs::write(&path, managed(&env)).unwrap();
    assert_eq!(permitted(&env, Some("launchd"), None), Ok(true));
}

#[test]
fn descriptor_syscall_failures_and_readback_mismatch_never_claim_success() {
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let before = Limits {
        soft: 256,
        hard: libc::RLIM_INFINITY,
    };
    let wanted = Limits {
        soft: 1024,
        ..before
    };
    let mut kernel = FakeKernel {
        reads: VecDeque::from([Ok(before), Ok(wanted)]),
        writes: vec![],
        fail_write: false,
    };
    assert_eq!(
        configure(&mut kernel, &env, None, None).unwrap(),
        Some(wanted)
    );
    assert_eq!(kernel.writes, vec![wanted]);

    let mut kernel = FakeKernel {
        reads: VecDeque::from([Err(std::io::Error::from_raw_os_error(libc::EINVAL))]),
        writes: vec![],
        fail_write: false,
    };
    assert!(
        configure(&mut kernel, &env, None, None)
            .unwrap_err()
            .contains("cannot read")
    );
    assert!(kernel.writes.is_empty());
    let mut kernel = FakeKernel {
        reads: VecDeque::from([Ok(before)]),
        writes: vec![],
        fail_write: true,
    };
    assert!(
        configure(&mut kernel, &env, None, None)
            .unwrap_err()
            .contains("no fallback")
    );
    assert_eq!(kernel.writes, vec![wanted]);
    let mut kernel = FakeKernel {
        reads: VecDeque::from([
            Ok(before),
            Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
        ]),
        writes: vec![],
        fail_write: false,
    };
    assert!(
        configure(&mut kernel, &env, None, None)
            .unwrap_err()
            .contains("outcome unverified")
    );
    let mut kernel = FakeKernel {
        reads: VecDeque::from([Ok(before), Ok(before)]),
        writes: vec![],
        fail_write: false,
    };
    assert!(
        configure(&mut kernel, &env, None, None)
            .unwrap_err()
            .contains("readback differs")
    );
    assert_eq!(kernel.writes.len(), 1);

    for value in ["inherit", "invalid"] {
        let mut kernel = FakeKernel {
            reads: VecDeque::from([Ok(before)]),
            writes: vec![],
            fail_write: false,
        };
        let result = configure(&mut kernel, &env, None, Some(OsStr::new(value)));
        assert_eq!(result.is_ok(), value == "inherit");
        assert!(kernel.writes.is_empty());
    }
}

fn other_limits() -> Vec<(libc::rlim_t, libc::rlim_t)> {
    [
        libc::RLIMIT_NPROC,
        libc::RLIMIT_CPU,
        libc::RLIMIT_FSIZE,
        libc::RLIMIT_DATA,
        libc::RLIMIT_STACK,
        libc::RLIMIT_CORE,
        libc::RLIMIT_MEMLOCK,
        libc::RLIMIT_AS,
    ]
    .into_iter()
    .map(|resource| {
        let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        assert_eq!(unsafe { libc::getrlimit(resource, limit.as_mut_ptr()) }, 0);
        let limit = unsafe { limit.assume_init() };
        (limit.rlim_cur, limit.rlim_max)
    })
    .collect()
}

#[test]
fn descriptor_policy_children_gain_capacity_and_preserve_operator_controls() {
    let before = Native.read().unwrap();
    let others = other_limits();
    for mode in [
        "managed", "fork", "inherit", "custom", "higher", "hard-cap", "fifo",
    ] {
        let root = scratch_dir();
        let env = Environment::at(root.path());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .envs(daemon_containment())
            .envs(env.child_vars())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("SH800_NOFILE_PROBE_MODE", mode)
            .env("SH800_NOFILE_PROBE_ROOT", root.path())
            .args(["--exact", CHILD, "--nocapture"]);
        if mode == "inherit" {
            command.env(VARIABLE, "inherit");
        }
        let output = ChildGuard::spawn_with_output(&mut command)
            .expect("spawn isolated NOFILE policy probe")
            .wait_with_output_within(
                storyhook_test_support::load_grace::graced_now(STORY_COMMAND_DEADLINE),
                || format!("NOFILE policy probe {mode} did not finish"),
            );
        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("SH800 descriptor policy observed")
        );
        assert_eq!(
            Native.read().unwrap(),
            before,
            "child changed parent NOFILE"
        );
        assert_eq!(
            other_limits(),
            others,
            "child changed parent resource policy"
        );
    }
}

#[test]
fn descriptor_policy_child_probe() {
    let Ok(mode) = std::env::var("SH800_NOFILE_PROBE_MODE") else {
        return;
    };
    let root = std::env::var_os("SH800_NOFILE_PROBE_ROOT").expect("owned probe root");
    let env = Environment::at(root);
    let original = Native.read().unwrap();
    assert!(
        original.hard == libc::RLIM_INFINITY || original.hard >= 2048,
        "probe host needs finite descriptor headroom"
    );
    let before = Limits {
        soft: if mode == "higher" { 2048 } else { 256 },
        hard: if mode == "hard-cap" {
            512
        } else {
            original.hard
        },
    };
    // Only this newly spawned test process changes its limits. The finite hard
    // case intentionally lowers its own ceiling; it is never restored/raised.
    Native.write(before).unwrap();
    let others = other_limits();
    let path = super::super::agent::path(&env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = managed(&env);
    let text = if mode == "custom" {
        text.replace("    <key>RunAtLoad</key>", "    <key>SoftResourceLimits</key><dict><key>NumberOfFiles</key><integer>256</integer></dict>\n    <key>RunAtLoad</key>")
    } else {
        text
    };
    if mode == "fifo" {
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // No writer: a blocking open in initialize would hang this probe and
        // fail the parent's ChildGuard deadline rather than the whole suite.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    } else {
        std::fs::write(path, text).unwrap();
    }
    let owner = if mode == "fork" {
        Some("fork-no-agent")
    } else {
        Some("launchd")
    };
    initialize(&env, owner);
    let expected = match mode.as_str() {
        "managed" | "fork" => 1024,
        "inherit" | "custom" | "fifo" => 256,
        "higher" => 2048,
        "hard-cap" => 512,
        _ => panic!("unexpected probe mode"),
    };
    assert_eq!(
        Native.read().unwrap(),
        Limits {
            soft: expected,
            hard: before.hard
        }
    );
    assert_eq!(
        other_limits(),
        others,
        "normalization changed another resource"
    );
    if expected > 256 {
        let descriptors: Vec<_> = (0..300)
            .map(|_| std::fs::File::open("/dev/null").unwrap())
            .collect();
        assert_eq!(
            descriptors.len(),
            300,
            "actual simultaneous capacity exceeds old ceiling"
        );
    }
    println!("SH800 descriptor policy observed");
}
