//! SH-574: real Git must refuse incorrect identities before they leave a clone.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use storyhook_test_support::{ChildGuard, EXPECT_TIMEOUT, scratch_dir};
use tempfile::TempDir;

struct Repo {
    root: TempDir,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            root: scratch_dir(),
        };
        fs::create_dir(repo.path().join("home")).unwrap();
        fs::write(
            repo.path().join("home/.gitconfig"),
            "[user]\nname = Correct Person\nemail = correct@example.test\n",
        )
        .unwrap();
        repo.git(&["init", "-q", "-b", "feature"]);
        repo.git(&["init", "-q", "--bare", "remote"]);
        repo.git(&["remote", "add", "origin", "remote"]);
        symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks"),
            repo.path().join(".githooks"),
        )
        .unwrap();
        repo.git(&["config", "core.hooksPath", ".githooks"]);
        repo
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.path().join("home"))
            .env("XDG_CONFIG_HOME", self.path().join("home/xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        cmd
    }

    fn git(&self, args: &[&str]) -> Output {
        let out = self.command("git").args(args).output().unwrap();
        ok(&out);
        out
    }

    fn commit(&self) -> Output {
        self.command("git")
            .args(["commit", "--allow-empty", "-qm", "identity proof"])
            .output()
            .unwrap()
    }

    fn push(&self) -> Output {
        self.command("git")
            .args(["push", "origin", "HEAD:refs/heads/feature"])
            .output()
            .unwrap()
    }

    fn audit(&self, args: &[&str]) -> Output {
        self.command("bash")
            .arg(helper())
            .arg("audit")
            .args(args)
            .output()
            .unwrap()
    }

    fn push_record(&self, record: &str) -> Output {
        let mut child = ChildGuard::spawn_with_output(
            self.command("bash")
                .arg(helper())
                .args(["push", "origin"])
                .stdin(Stdio::piped()),
        )
        .unwrap();
        child.stdin().unwrap().write_all(record.as_bytes()).unwrap();
        child.wait_with_output_within(EXPECT_TIMEOUT, || {
            "identity checker did not finish after receiving its ref record".to_string()
        })
    }

    fn approve(&self, role: &str, name: &str, email: &str) {
        for (field, value) in [
            ("name", name),
            ("email", email),
            ("role", role),
            ("reason", "Preserve reviewed contributor identity"),
        ] {
            self.git(&[
                "config",
                &format!("storyhookIdentity.reviewed.{field}"),
                value,
            ]);
        }
    }
}

fn helper() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/git-identity.sh")
}
fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
fn ok(out: &Output) {
    assert!(out.status.success(), "{}", text(out));
}
fn refused(out: &Output) {
    assert!(
        !out.status.success(),
        "incorrect identity accepted: {}",
        text(out)
    );
    assert!(
        text(out).contains("git-identity:"),
        "wrong failure: {}",
        text(out)
    );
}

#[path = "commit_identity/policy.rs"]
mod policy;
#[path = "commit_identity/push.rs"]
mod push;
