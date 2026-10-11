//! Test-owned roots may be removed only after their exact Git custody settles.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use storyhook_test_support::ChildGuard;

pub struct ManagedFixture {
    root: PathBuf,
    worktree: PathBuf,
    root_identity: (u64, u64),
    git_identity: (u64, u64),
}

impl ManagedFixture {
    /// `worktree_relative` names the one initialized Git repository whose
    /// managed commands can write products anywhere inside this owned root.
    pub fn new(root: tempfile::TempDir, worktree_relative: &str) -> Self {
        let worktree = root.path().join(worktree_relative);
        let identity = |path: &Path| {
            let info = std::fs::symlink_metadata(path).expect("fixture directory identity");
            assert!(info.is_dir(), "fixture directory must not be a symlink");
            (info.dev(), info.ino())
        };
        let root_identity = identity(root.path());
        let git_identity = identity(&worktree.join(".git"));
        assert!(
            worktree
                .canonicalize()
                .unwrap()
                .starts_with(root.path().canonicalize().unwrap()),
            "managed Git namespace must belong to the fixture"
        );
        Self {
            root: root.keep(),
            worktree,
            root_identity,
            git_identity,
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for ManagedFixture {
    fn drop(&mut self) {
        // There is deliberately no TempDir left to delete on unwind. A failed
        // observer, Git identity mismatch, held lease, or unfinished journal
        // retains both the products and the evidence needed to recover them.
        let settled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut command = Command::new("bash");
            command
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/scripts/python-runtime.sh"
                ))
                .args(["--", "python3", "-B", "-c", CLEANUP])
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/scripts"))
                .arg(&self.root)
                .arg(&self.worktree)
                .arg(self.root_identity.0.to_string())
                .arg(self.root_identity.1.to_string())
                .arg(self.git_identity.0.to_string())
                .arg(self.git_identity.1.to_string());
            // Resolve the explicit fixture worktree, never an ambient Git
            // override inherited from the outer checkpoint's checkout.
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("GIT_") {
                    command.env_remove(key);
                }
            }
            let mut child = ChildGuard::spawn_with_output(&mut command)
                .expect("start managed fixture settlement observer");
            let output = child.wait_with_output_within(
                storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
                || "managed fixture settlement observer did not finish".into(),
            );
            assert!(
                output.status.success(),
                "managed fixture settlement refused: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }));
        if settled.is_err() {
            eprintln!(
                "managed fixture cleanup unproved; retained {}",
                self.root.display()
            );
            if !std::thread::panicking() {
                panic!("managed fixture custody did not settle");
            }
        }
    }
}

const CLEANUP: &str = r#"
import shutil, stat, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from build_products import ProductLease, git_path, namespace
root, worktree = map(Path, sys.argv[2:4])
root_pin = tuple(map(int, sys.argv[4:6]))
git_pin = tuple(map(int, sys.argv[6:8]))
def require_directory(path, pin):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or (info.st_dev, info.st_ino) != pin:
        raise RuntimeError('fixture directory identity changed: ' + str(path))
require_directory(root, root_pin)
require_directory(worktree / '.git', git_pin)
if not worktree.resolve().is_relative_to(root.resolve()):
    raise RuntimeError('fixture worktree escaped its owner root')
expected_git = (worktree / '.git').resolve(strict=True)
if git_path(worktree, '--absolute-git-dir') != expected_git:
    raise RuntimeError('fixture Git namespace changed')
# No recursive discovery: source/helper symlink destinations are unrelated.
# ProductLease validates every journal and keeps exclusion through deletion.
with ProductLease(namespace(worktree), reclaim=True):
    require_directory(root, root_pin)
    require_directory(worktree / '.git', git_pin)
    shutil.rmtree(root)
"#;
