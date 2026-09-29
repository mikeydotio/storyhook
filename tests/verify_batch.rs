//! The batch branch's GitHub side (SH-831; spec B5): `scripts/verify-batch.sh`
//! publishes a batch tip and its pull request, and retires them, through the
//! origin-pinned adapter against a local origin (the endpoint fixture) and a
//! fake `gh` that keeps pull requests as state.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

const ORIGIN_URL: &str = "https://github.com/acme/widgets.git";
const BRANCH: &str = "storyhook/verify-batch/0123456789ab";

/// A fake `gh` over `fake-gh-state/prs.json` (a list of pull requests):
/// `pr list`, `pr create` (head read from the local origin), `pr view`, and
/// `pr close`. Every call's argv is appended to `argv`. `stale-views` makes
/// that many `pr view` calls report no head first, as GitHub does while a
/// new head converges.
const FAKE_GH: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys
# The routed executor passes gh an allowlisted environment, so the fake
# finds its state beside itself rather than in a variable.
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
state = os.path.join(root, "fake-gh-state")
args = sys.argv[1:]
with open(os.path.join(state, "argv"), "a") as log:
    log.write(json.dumps(args) + "\n")
path = os.path.join(state, "prs.json")
prs = json.load(open(path)) if os.path.exists(path) else []
def save():
    json.dump(prs, open(path, "w"))
def flag(name):
    return args[args.index(name) + 1] if name in args else None
def fields(pr, names):
    return {n: pr[n] for n in names.split(",")}
if args[:2] == ["pr", "list"]:
    head = flag("--head")
    wanted = [p for p in prs if p["headRefName"] == head and p["state"] == "OPEN"]
    print(json.dumps([fields(p, flag("--json")) for p in wanted]))
elif args[:2] == ["pr", "create"]:
    head = flag("--head")
    oid = subprocess.run(["git", "-C", os.path.join(root, "origin.git"), "rev-parse", "refs/heads/" + head],
                         capture_output=True, text=True, check=True).stdout.strip()
    number = max([p["number"] for p in prs], default=40) + 1
    prs.append({"number": number, "url": "https://github.com/acme/widgets/pull/%d" % number,
                "baseRefName": flag("--base"), "headRefName": head, "headRefOid": oid,
                "isCrossRepository": False, "state": "OPEN",
                "title": flag("--title"), "body": flag("--body")})
    save()
    print("https://github.com/acme/widgets/pull/%d" % number)
elif args[:2] == ["pr", "view"]:
    pr = next(p for p in prs if str(p["number"]) == args[2])
    answer = fields(pr, flag("--json"))
    stale = os.path.join(state, "stale-views")
    if os.path.exists(stale) and "headRefOid" in answer:
        left = int(open(stale).read())
        if left > 0:
            open(stale, "w").write(str(left - 1))
            answer["headRefOid"] = "0" * 40
    print(json.dumps(answer))
elif args[:2] == ["pr", "close"]:
    pr = next(p for p in prs if str(p["number"]) == args[2])
    pr["state"] = "CLOSED"
    pr["closing_comment"] = flag("--comment")
    save()
else:
    sys.stderr.write("fake gh: unsupported %r\n" % args)
    sys.exit(64)
"#;

/// A local origin, a clone of it whose `origin` is the GitHub URL the
/// endpoint fixture maps back to the local origin, and a tip commit.
struct Fixture {
    dir: TempDir,
    tip: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = scratch_dir();
        let fixture = Self {
            dir,
            tip: String::new(),
        };
        fs::create_dir_all(fixture.bin()).unwrap();
        fs::create_dir_all(fixture.state()).unwrap();
        fixture.git(
            Path::new("."),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "dev",
                fixture.origin().to_str().unwrap(),
            ],
        );
        fixture.git(
            Path::new("."),
            &[
                "init",
                "-q",
                "-b",
                "dev",
                fixture.checkout().to_str().unwrap(),
            ],
        );
        let checkout = fixture.checkout();
        fixture.git(&checkout, &["config", "user.name", "t"]);
        fixture.git(&checkout, &["config", "user.email", "t@t"]);
        fixture.git(&checkout, &["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(&checkout, "t", "t@t");
        fixture.git(&checkout, &["commit", "-q", "--allow-empty", "-m", "base"]);
        fixture.git(&checkout, &["remote", "add", "origin", ORIGIN_URL]);
        fixture.git(
            &checkout,
            &["push", "-q", fixture.origin().to_str().unwrap(), "dev"],
        );
        fixture.git(
            &checkout,
            &["commit", "-q", "--allow-empty", "-m", "batch tip"],
        );
        let tip = fixture.git(&checkout, &["rev-parse", "HEAD"]);
        storyhook_test_support::install_git_endpoint(
            &fixture.bin(),
            &[(ORIGIN_URL, &fixture.origin())],
        );
        let gh = fixture.bin().join("gh");
        fs::write(&gh, FAKE_GH).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        Self { tip, ..fixture }
    }

    fn origin(&self) -> PathBuf {
        self.dir.path().join("origin.git")
    }

    fn checkout(&self) -> PathBuf {
        self.dir.path().join("checkout")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("fake-gh-state")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = storyhook::env::git_env::command(cwd)
            .args(args)
            .output()
            .expect("fixture: running git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn origin_branch(&self) -> Option<String> {
        let output = storyhook::env::git_env::command(&self.origin())
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{BRANCH}"),
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn prs(&self) -> Vec<Value> {
        fs::read_to_string(self.state().join("prs.json"))
            .map(|text| serde_json::from_str(&text).unwrap())
            .unwrap_or_default()
    }

    fn seed(&self, prs: &Value) {
        fs::write(self.state().join("prs.json"), prs.to_string()).unwrap();
    }

    fn gh_calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.state().join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> (Output, Value) {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut path = std::ffi::OsString::from(self.bin());
        path.push(":");
        path.push(inherited);
        let output = Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/verify-batch.sh"))
            .args(args)
            .current_dir(self.checkout())
            .env("PATH", path)
            .env("STORY_BIN", storyhook_test_support::story_binary())
            .env("STORYHOOK_GITHUB_AUTHORITY", self.checkout())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("STORYHOOK_GITHUB_EXPECTED")
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .envs(extra.iter().copied())
            .envs(storyhook_test_support::daemon_containment())
            .output()
            .expect("running verify-batch.sh");
        let answer = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "verify-batch.sh answered no JSON ({error}): stdout {} stderr {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output, answer)
    }

    fn publish(&self, extra: &[(&str, &str)]) -> (Output, Value) {
        self.run(
            &[
                "publish",
                BRANCH,
                &self.tip,
                "dev",
                "Verification batch 0123456789ab: SH-1, SH-2",
                "Members:\n1. SH-1 — #7\n2. SH-2 — #8",
            ],
            extra,
        )
    }
}

fn open_pr(number: u64, base: &str, head: &str) -> Value {
    json!({
        "number": number,
        "url": format!("https://github.com/acme/widgets/pull/{number}"),
        "baseRefName": base,
        "headRefName": BRANCH,
        "headRefOid": head,
        "isCrossRepository": false,
        "state": "OPEN",
    })
}

#[test]
fn publish_pushes_the_tip_and_opens_the_batch_pull_request() {
    let fixture = Fixture::new();
    let journal = fixture.dir.path().join("progress.ndjson");
    let (output, answer) =
        fixture.publish(&[("STORYHOOK_GATE_PROGRESS", journal.to_str().unwrap())]);

    assert!(output.status.success(), "{answer}");
    assert_eq!(answer["ok"], true);
    assert_eq!(answer["pushed"], true);
    assert_eq!(answer["adopted"], false);
    assert_eq!(answer["head_oid"], fixture.tip.as_str());
    assert_eq!(answer["base"], "dev");
    assert_eq!(answer["number"], 41);
    assert_eq!(answer["url"], "https://github.com/acme/widgets/pull/41");
    assert_eq!(
        fixture.origin_branch().as_deref(),
        Some(fixture.tip.as_str())
    );
    let prs = fixture.prs();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0]["baseRefName"], "dev");
    assert_eq!(prs[0]["headRefName"], BRANCH);
    assert_eq!(
        prs[0]["title"],
        "Verification batch 0123456789ab: SH-1, SH-2"
    );
    assert_eq!(prs[0]["body"], "Members:\n1. SH-1 — #7\n2. SH-2 — #8");
    let progress = fs::read_to_string(journal).unwrap();
    assert!(
        progress.contains(r#""path":"batch publication","status":"running""#),
        "{progress}"
    );
    assert!(
        progress.contains(r#""path":"batch publication","status":"passed""#),
        "{progress}"
    );
}

#[test]
fn publish_again_adopts_the_open_pull_request_without_pushing() {
    let fixture = Fixture::new();
    let (first, answer) = fixture.publish(&[]);
    assert!(first.status.success(), "{answer}");
    let creates = |fixture: &Fixture| {
        fixture
            .gh_calls()
            .iter()
            .filter(|call| call[..2] == ["pr", "create"])
            .count()
    };
    assert_eq!(creates(&fixture), 1);

    let (again, adopted) = fixture.publish(&[]);

    assert!(again.status.success(), "{adopted}");
    assert_eq!(adopted["adopted"], true);
    assert_eq!(adopted["pushed"], false);
    assert_eq!(adopted["number"], answer["number"]);
    assert_eq!(
        creates(&fixture),
        1,
        "an open pull request is adopted, never duplicated"
    );
}

#[test]
fn publish_refuses_a_wrong_base_or_several_open_pull_requests() {
    for (prs, reason) in [
        (
            json!([open_pr(7, "main", &"f".repeat(40))]),
            "wrong-base-pull-request",
        ),
        (
            json!([
                open_pr(7, "dev", &"f".repeat(40)),
                open_pr(8, "dev", &"f".repeat(40))
            ]),
            "multiple-pull-requests",
        ),
    ] {
        let fixture = Fixture::new();
        fixture.seed(&prs);
        let (output, answer) = fixture.publish(&[]);
        assert!(!output.status.success(), "{answer}");
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["reason"], reason, "{answer}");
        assert!(
            !fixture
                .gh_calls()
                .iter()
                .any(|call| call[..2] == ["pr", "create"]),
            "nothing is opened beside a pull request it refused"
        );
    }
}

#[test]
fn publish_never_forces_over_a_branch_origin_already_has() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.git(
        &checkout,
        &["commit", "-q", "--allow-empty", "-m", "someone else"],
    );
    let other = fixture.git(&checkout, &["rev-parse", "HEAD"]);
    fixture.git(
        &checkout,
        &[
            "push",
            "-q",
            fixture.origin().to_str().unwrap(),
            &format!("{other}:refs/heads/{BRANCH}"),
        ],
    );

    let (output, answer) = fixture.publish(&[]);

    assert!(!output.status.success(), "{answer}");
    assert_eq!(answer["reason"], "branch-exists", "{answer}");
    assert_eq!(fixture.origin_branch().as_deref(), Some(other.as_str()));
}

#[test]
fn publish_waits_for_the_head_to_converge_and_refuses_when_it_never_does() {
    let fixture = Fixture::new();
    fs::write(fixture.state().join("stale-views"), "1").unwrap();
    let (output, answer) = fixture.publish(&[]);
    assert!(output.status.success(), "{answer}");
    let views = fixture
        .gh_calls()
        .iter()
        .filter(|call| call[..2] == ["pr", "view"])
        .count();
    assert_eq!(views, 2, "one stale read, then the converged one");

    let fixture = Fixture::new();
    fs::write(fixture.state().join("stale-views"), "1000").unwrap();
    let (output, answer) = fixture.publish(&[("STORYHOOK_BATCH_CONVERGENCE_SECS", "0")]);
    assert!(!output.status.success(), "{answer}");
    assert_eq!(answer["reason"], "head-unconverged", "{answer}");
}

#[test]
fn retire_closes_the_pull_request_and_deletes_the_branch_once() {
    let fixture = Fixture::new();
    let (_, published) = fixture.publish(&[]);
    let url = published["url"].as_str().unwrap().to_owned();
    let comment = "Batch 0123456789ab ended: landing a batch is not built yet. Members SH-1 (#7) and SH-2 (#8) are verified alone.";

    let (output, answer) = fixture.run(&["retire", BRANCH, &url, comment], &[]);

    assert!(output.status.success(), "{answer}");
    assert_eq!(
        answer,
        json!({"ok": true, "closed": true, "merged": false, "deleted": true})
    );
    assert_eq!(fixture.prs()[0]["state"], "CLOSED");
    assert_eq!(fixture.prs()[0]["closing_comment"], comment);
    assert_eq!(fixture.origin_branch(), None);

    let (again, answer) = fixture.run(&["retire", BRANCH, &url, comment], &[]);
    assert!(again.status.success(), "{answer}");
    assert_eq!(
        answer,
        json!({"ok": true, "closed": false, "merged": false, "deleted": false})
    );
}

#[test]
fn retire_reports_a_merged_pull_request_and_works_without_one() {
    let fixture = Fixture::new();
    let mut merged = open_pr(9, "dev", &fixture.tip);
    merged["state"] = json!("MERGED");
    fixture.seed(&json!([merged]));
    let (output, answer) = fixture.run(
        &[
            "retire",
            BRANCH,
            "https://github.com/acme/widgets/pull/9",
            "ended",
        ],
        &[],
    );
    assert!(output.status.success(), "{answer}");
    assert_eq!(answer["merged"], true);
    assert_eq!(answer["closed"], false);

    let fixture = Fixture::new();
    fixture.git(
        &fixture.checkout(),
        &[
            "push",
            "-q",
            fixture.origin().to_str().unwrap(),
            &format!("{}:refs/heads/{BRANCH}", fixture.tip),
        ],
    );
    let (output, answer) = fixture.run(&["retire", BRANCH, "-", "ended"], &[]);
    assert!(output.status.success(), "{answer}");
    assert_eq!(
        answer,
        json!({"ok": true, "closed": false, "merged": false, "deleted": true})
    );
    assert_eq!(fixture.origin_branch(), None);
}

#[test]
fn bad_inputs_are_refused_before_any_github_call() {
    let fixture = Fixture::new();
    for args in [
        vec!["publish", "dev", fixture.tip.as_str(), "dev", "t", "b"],
        vec!["publish", BRANCH, "HEAD", "dev", "t", "b"],
        vec!["publish", BRANCH, fixture.tip.as_str(), "", "t", "b"],
        vec!["retire", "storyhook/verify-batch/../../dev", "-", "c"],
        vec!["land", BRANCH],
    ] {
        let (output, answer) = fixture.run(&args, &[]);
        assert!(!output.status.success(), "{args:?}: {answer}");
        assert_eq!(answer["ok"], false, "{args:?}");
    }
    assert!(fixture.gh_calls().is_empty());
    assert_eq!(fixture.origin_branch(), None);
}
