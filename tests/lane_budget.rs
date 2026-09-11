//! The store-free, informational census reports live tagged tmux windows.
//! SH-672: it never assigns a machine budget or dispatch permission. An
//! unanswered census carries its diagnostic instead of inventing a count.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use storyhook_test_support::TestEnv;
use tempfile::TempDir;

/// A fake `tmux` that answers `list-windows` from a seeded file and refuses
/// everything else — the `tests/merge_gate.rs` fake-binary shape.
struct FakeTmux {
    dir: TempDir,
}

impl FakeTmux {
    fn answering(lines: &str) -> Self {
        let dir = storyhook_test_support::scratch_dir();
        let census = dir.path().join("census");
        std::fs::write(&census, lines).unwrap();
        let script = dir.path().join("tmux");
        std::fs::write(
            &script,
            format!(
                "#!/usr/bin/env bash\nset -u\nprintf '%s\\n' \"$*\" >>\"{argv}\"\n\
                 case \" $* \" in (*' list-windows '*) cat \"{census}\"; exit 0;; esac\n\
                 echo 'fake tmux: unexpected verb' >&2; exit 1\n",
                argv = dir.path().join("argv").display(),
                census = census.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    /// A `tmux` that exits 1 with the words a real server-less tmux prints.
    fn no_server() -> Self {
        let dir = storyhook_test_support::scratch_dir();
        let script = dir.path().join("tmux");
        std::fs::write(
            &script,
            "#!/usr/bin/env bash\necho 'no server running on /tmp/tmux-501/default' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("argv")).unwrap_or_default()
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// `PATH` with the fake ahead of everything, and with no real `tmux`
/// reachable at all when `fake` is `None`.
fn path_with(fake: Option<&FakeTmux>) -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(fake) = fake {
        dirs.push(fake.path().to_path_buf());
    }
    // The system directories still hold `bash` for the fake's own shebang;
    // none of them hold `tmux` on a machine where it came from Homebrew, and
    // the `no tmux at all` case asserts on the probe's wording rather than on
    // that assumption.
    dirs.push(PathBuf::from("/usr/bin"));
    dirs.push(PathBuf::from("/bin"));
    std::env::join_paths(dirs)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn run(env: &TestEnv, fake: Option<&FakeTmux>, args: &[&str]) -> std::process::Output {
    let cwd = env.home();
    env.story(cwd)
        .args(args)
        .env("PATH", path_with(fake))
        .env_remove("TMUX")
        .output()
        .expect("running `story lane-budget`")
}

fn json_of(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

const CENSUS: &str = "storyhook:SH-655\tclaude\t0\n\
storyhook:SH-643\tcodex\t0\n\
storyhook:SH-600\tclaude\t1\n\
storyhook:zsh\t\t0\n\
storyhook-verifier:verification\t\t0\n";

#[test]
fn it_counts_only_tagged_windows_whose_pane_is_alive() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::answering(CENSUS);
    let json = json_of(&run(&env, Some(&fake), &["lane-budget", "--json"]));

    assert_eq!(json["probe"], "counted", "{json}");
    assert_eq!(json["live"], 2, "{json}");
    assert_eq!(
        json["windows"],
        serde_json::json!(["storyhook:SH-655", "storyhook:SH-643"]),
        "a dead pane and two untagged windows are not sessions: {json}"
    );
    assert!(json.get("budget").is_none(), "{json}");
    assert!(json.get("available").is_none(), "{json}");
    assert!(
        fake.argv().contains("list-windows -a -F"),
        "the census must ask every session on the server, not the current one: {}",
        fake.argv()
    );
}

#[test]
fn six_live_sessions_report_a_count_without_a_budget() {
    let env = TestEnv::isolated();
    let census: String = (0..6)
        .map(|n| format!("storyhook:SH-{n}\tclaude\t0\n"))
        .collect();
    let fake = FakeTmux::answering(&census);
    let json = json_of(&run(&env, Some(&fake), &["lane-budget", "--json"]));

    assert_eq!(json["live"], 6, "{json}");
    assert!(json.get("available").is_none(), "{json}");
    assert!(json.get("budget").is_none(), "{json}");
}

#[test]
fn an_empty_server_is_a_counted_zero_not_an_unanswered_probe() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::answering("");
    let json = json_of(&run(&env, Some(&fake), &["lane-budget", "--json"]));

    assert_eq!(json["probe"], "counted", "{json}");
    assert_eq!(json["live"], 0, "{json}");
    assert!(json.get("available").is_none(), "{json}");
}

#[test]
fn a_server_that_cannot_be_asked_is_unanswered_never_zero() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::no_server();
    let out = run(&env, Some(&fake), &["lane-budget", "--json"]);
    let json = json_of(&out);

    assert_eq!(json["probe"], "unanswered", "{json}");
    assert!(json["live"].is_null(), "no evidence is not a count: {json}");
    assert!(json["available"].is_null(), "{json}");
    assert!(
        json["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("no server running"),
        "the probe's own words travel with the verdict: {json}"
    );
    assert!(json.get("budget").is_none(), "{json}");
}

#[test]
fn a_machine_with_no_tmux_at_all_is_unanswered_and_still_exits_zero() {
    let env = TestEnv::isolated();
    let out = run(&env, None, &["lane-budget", "--json"]);
    let json = json_of(&out);

    assert_eq!(json["probe"], "unanswered", "{json}");
    assert!(
        json["detail"].as_str().unwrap_or_default().contains("tmux"),
        "{json}"
    );
}

#[test]
fn it_opens_no_store_and_starts_no_daemon() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::answering(CENSUS);
    let out = run(&env, Some(&fake), &["lane-budget", "--json"]);
    assert!(out.status.success());

    assert!(
        !env.store_path().exists(),
        "a store appeared at {}",
        env.store_path().display()
    );
    assert!(
        !env.daemon_is_live(),
        "a daemon was started to answer a question only the caller's tmux can answer"
    );
}

#[test]
fn the_human_rendering_names_the_count_and_every_live_window() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::answering(CENSUS);
    let out = run(&env, Some(&fake), &["lane-budget"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(text.contains("2 live agent sessions"), "{text}");
    assert!(!text.contains("budget"), "{text}");
    assert!(text.contains("storyhook:SH-655"), "{text}");
    assert!(text.contains("storyhook:SH-643"), "{text}");
    assert!(
        !text.contains("SH-600"),
        "a dead pane is not a session: {text}"
    );
}

#[test]
fn a_trailing_word_is_refused_by_name() {
    let env = TestEnv::isolated();
    let fake = FakeTmux::answering(CENSUS);
    let out = run(&env, Some(&fake), &["lane-budget", "extra"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("extra"), "{stderr}");
}
