//! Fresh private object custody for read-only landed observation.
//!
//! The source retains its normal protected transport/configuration. Only object
//! storage and the unused index are redirected; explicit fetch flags exclude
//! Git's ref/FETCH_HEAD/maintenance side effects. Trusted hooks/credential
//! programs are not a security sandbox and can have their own side effects.
use super::{Repository, transport::PublicationControl};
use crate::{env::git_env, error::AppError};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

pub(super) const FLAGS: &[&str] = &[
    "--refetch",
    "--refmap=",
    "--no-write-fetch-head",
    "--no-tags",
    "--no-prune",
    "--no-prune-tags",
    "--no-recurse-submodules",
    "--no-auto-maintenance",
    "--no-write-commit-graph",
];
const LIMIT: usize = 65536;

/// Fresh native ownership only. No deserialization, adoption, Clone or Drop
/// cleanup: failures may leave a writer whose settlement is uncertain.
pub(crate) struct PrivateFetch {
    path: PathBuf,
    directories: Vec<(PathBuf, File)>,
    immutable: Vec<(PathBuf, File, Vec<u8>)>,
    format: String,
}
impl PrivateFetch {
    pub(crate) fn create(
        repository: &Repository,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, AppError> {
        let control = PublicationControl {
            deadline,
            cancelled,
        };
        let format = control.read(&repository.checkout, &["rev-parse", "--show-object-format"])?;
        Self::create_at(Path::new("/tmp"), format.trim())
    }

    fn create_at(parent: &Path, format: &str) -> Result<Self, AppError> {
        if !matches!(format, "sha1" | "sha256") {
            return Err(refuse("unsupported source object format"));
        }
        let parent = parent.canonicalize().map_err(storage)?;
        let path = parent.join(format!("storyhook-landed-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(storage)?;
        let mut result = Self {
            path,
            directories: Vec::new(),
            immutable: Vec::new(),
            format: format.into(),
        };
        // Construct a bare namespace directly, without inherited templates,
        // includes, hooks, alternates or a subprocess that might outlive init.
        let built = (|| {
            for relative in [
                "",
                "objects",
                "objects/info",
                "objects/pack",
                "refs",
                "refs/heads",
                "info",
            ] {
                let path = result.path.join(relative);
                if !relative.is_empty() {
                    fs::create_dir(&path).map_err(storage)?;
                }
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(storage)?;
                result.directories.push((path, file));
            }
            let config = if format == "sha256" {
                "[core]\nrepositoryformatversion = 1\nbare = true\n[extensions]\nobjectformat = sha256\n"
            } else {
                "[core]\nrepositoryformatversion = 0\nbare = true\n"
            };
            for (relative, bytes) in [
                ("config", config.as_bytes().to_vec()),
                ("HEAD", b"ref: refs/heads/observation\n".to_vec()),
                (
                    "storyhook-owner",
                    uuid::Uuid::new_v4().to_string().into_bytes(),
                ),
            ] {
                let path = result.path.join(relative);
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(storage)?;
                file.write_all(&bytes).map_err(storage)?;
                file.sync_all().map_err(storage)?;
                result.immutable.push((path, file, bytes));
            }
            result.validate()
        })();
        built.map_err(|e| result.residue(e))?;
        Ok(result)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Stable pins only while the exclusively owned Git writer is running.
    /// Do not traverse its transient pack files inside the cancellation poll.
    pub(crate) fn validate_live_custody(&self) -> Result<(), AppError> {
        for (path, held) in &self.directories {
            let observed = fs::symlink_metadata(path).map_err(storage)?;
            let pinned = held.metadata().map_err(storage)?;
            if !observed.is_dir()
                || observed.file_type().is_symlink()
                || observed.dev() != pinned.dev()
                || observed.ino() != pinned.ino()
                || path.canonicalize().map_err(storage)? != *path
            {
                return Err(refuse("fresh observation directory replaced"));
            }
        }
        for (path, held, bytes) in &self.immutable {
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(storage)?;
            let m = file.metadata().map_err(storage)?;
            let h = held.metadata().map_err(storage)?;
            if !m.is_file()
                || m.dev() != h.dev()
                || m.ino() != h.ino()
                || m.len() != bytes.len() as u64
                || m.nlink() != 1
            {
                return Err(refuse("fresh observation stamp/config replaced"));
            }
            let mut actual = Vec::new();
            (&mut file)
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut actual)
                .map_err(storage)?;
            if &actual != bytes {
                return Err(refuse("fresh observation stamp/config changed"));
            }
        }
        for relative in [
            ".git",
            "commondir",
            "gitdir",
            "shallow",
            "info/grafts",
            "objects/info/alternates",
            "objects/info/http-alternates",
            "refs/replace",
        ] {
            match fs::symlink_metadata(self.path.join(relative)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(storage(e)),
                Ok(_) => {
                    return Err(refuse(
                        "fresh observation acquired an object/history redirect",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Full namespace proof only before/after settled commands and consumption.
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        self.validate_live_custody()?;
        let mut pending = vec![self.path.clone()];
        let mut seen = 0;
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).map_err(storage)? {
                let entry = entry.map_err(storage)?;
                seen += 1;
                if seen > LIMIT {
                    return Err(refuse("private namespace exceeds inspection limit"));
                }
                let kind = entry.file_type().map_err(storage)?;
                if kind.is_symlink()
                    || (!kind.is_dir() && !kind.is_file())
                    || entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "promisor")
                {
                    return Err(refuse(
                        "private namespace contains redirect or incomplete objects",
                    ));
                }
                if kind.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
        Ok(())
    }

    pub(super) fn prepare_source(
        &self,
        repo: &Repository,
        control: PublicationControl<'_>,
    ) -> Result<(), AppError> {
        self.validate()?;
        if control
            .read(&repo.checkout, &["rev-parse", "--show-object-format"])?
            .trim()
            != self.format
            || control
                .read(&repo.checkout, &["rev-parse", "--is-shallow-repository"])?
                .trim()
                != "false"
        {
            return Err(refuse("source object format changed or source is shallow"));
        }
        let mut config = git_env::command(&repo.checkout);
        config.args(["config", "--null", "--get-regexp", "^(fetch\\.(bundleuri|bundlecreationtoken)|transfer\\.bundleuri|extensions\\.partialclone|remote\\..*\\.(promisor|partialclonefilter))$"]);
        let output = control.capture(config).map_err(|e| refuse(&e.detail()))?;
        // Any effective declaration refuses, including false/empty values.
        // transfer.bundleURI is a conservative opt-in refusal (clone-only on
        // currently documented Git), not a demonstrated fetch write channel.
        if output.status.code() != Some(1) || !output.stdout.is_empty() || output.stdout_truncated {
            return Err(refuse(
                "source bundle/partial-clone configuration cannot isolate this fetch",
            ));
        }
        let mut help = git_env::command(&repo.checkout);
        help.args(["fetch", "-h"]).env("LC_ALL", "C");
        let output = control.capture(help).map_err(|e| refuse(&e.detail()))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !matches!(output.status.code(), Some(0 | 129))
            || output.stdout_truncated
            || output.stderr.len() >= LIMIT
            || !capabilities(&text)
        {
            return Err(refuse("Git lacks required private-fetch controls"));
        }
        self.validate()
    }

    pub(super) fn configure(&self, command: &mut Command) -> Result<(), AppError> {
        self.validate()?;
        command
            .env("GIT_OBJECT_DIRECTORY", self.path.join("objects"))
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
            .env("GIT_INDEX_FILE", self.path.join("observation.index"));
        Ok(())
    }

    pub(crate) fn fetch(
        &self,
        repo: &Repository,
        oids: &[&str],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), AppError> {
        repo.git_private_fetch(self, oids, deadline, cancelled)
            .map_err(|e| self.residue(e))?;
        self.validate().map_err(|e| self.residue(e))
    }

    pub(crate) fn read(
        &self,
        arguments: &[&str],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        self.validate()?;
        let mut command = git_env::command(&self.path);
        command
            .env("GIT_DIR", &self.path)
            .env("GIT_COMMON_DIR", &self.path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "")
            .args(["-c", "protocol.allow=never"])
            .args(arguments);
        let output = PublicationControl {
            deadline,
            cancelled,
        }
        .capture(command)
        .map_err(|e| self.residue(refuse(&e.detail())))?;
        self.validate()?;
        if !output.status.success() || output.stdout_truncated || output.stdout.len() >= LIMIT {
            return Err(refuse(
                "fresh private Git proof failed or exceeded capture bound",
            ));
        }
        Ok(output.stdout)
    }

    /// Only a successfully completed observation may explicitly release its new
    /// private resources. Error paths retain them and identify their location.
    pub(crate) fn settle(self) -> Result<(), AppError> {
        self.validate().map_err(|e| self.residue(e))?;
        fs::remove_dir_all(&self.path).map_err(|e| self.residue(storage(e)))
    }
    pub(crate) fn residue(&self, error: AppError) -> AppError {
        refuse(&format!(
            "{error}; fresh observation residue retained at {}",
            self.path.display()
        ))
    }
}
fn capabilities(help: &str) -> bool {
    [
        "refetch",
        "refmap",
        "write-fetch-head",
        "prune-tags",
        "recurse-submodules",
        "auto-maintenance",
        "write-commit-graph",
    ]
    .iter()
    .all(|flag| help.contains(flag))
}
pub(super) fn arguments(oids: &[&str]) -> Result<Vec<String>, AppError> {
    if oids.is_empty() || oids.len() > 3 || !oids.iter().all(|oid| full_oid(oid)) {
        return Err(refuse("private fetch needs only full immutable object IDs"));
    }
    Ok(std::iter::once("fetch")
        .chain(FLAGS.iter().copied())
        .chain(std::iter::once("origin"))
        .chain(oids.iter().copied())
        .map(str::to_owned)
        .collect())
}
pub(crate) fn full_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn refuse(detail: &str) -> AppError {
    AppError::Validation(format!("landed private fetch held: {detail}"))
}
fn storage(error: impl std::fmt::Display) -> AppError {
    refuse(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, time::Duration};
    fn deadline() -> Instant {
        Instant::now() + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60))
    }
    fn control<'a>(until: Instant, cancelled: &'a dyn Fn() -> bool) -> PublicationControl<'a> {
        PublicationControl {
            deadline: until,
            cancelled,
        }
    }
    #[test]
    fn sh871_private_fetch_rejects_refs_refspecs_and_option_operands() {
        for value in [
            "main",
            "refs/heads/dev",
            "--all",
            "+abc:def",
            "abc:def",
            "https://evil/repo",
        ] {
            assert!(arguments(&[value]).is_err());
        }
        let args = arguments(&[&"a".repeat(40)]).unwrap();
        for flag in FLAGS {
            assert!(args.iter().any(|arg| arg == flag));
        }
    }
    #[test]
    fn sh871_private_fetch_refuses_replaced_custody_and_history_redirects() {
        let scratch = storyhook_test_support::scratch_dir();
        for relative in [
            "objects/info/alternates",
            "info/grafts",
            "shallow",
            "refs/replace",
        ] {
            let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
            fs::write(private.path.join(relative), b"redirect").unwrap();
            assert!(private.validate().is_err(), "{relative}");
            fs::remove_file(private.path.join(relative)).unwrap();
            private.settle().unwrap();
        }
        let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
        fs::write(private.path.join("config"), b"[include]\npath=/other\n").unwrap();
        assert!(private.validate().is_err());
        // Test owns this root; production's refusal retains it.
    }
    #[test]
    fn sh871_private_fetch_refuses_bundle_and_partial_clone_before_transfer() {
        let scratch = storyhook_test_support::scratch_dir();
        let until = deadline();
        let cancelled = || false;
        let c = control(until, &cancelled);
        c.read(scratch.path(), &["init", "-q"]).unwrap();
        c.read(
            scratch.path(),
            &[
                "config",
                "remote.origin.url",
                "https://github.example/org/repo.git",
            ],
        )
        .unwrap();
        let env = crate::env::Environment::at(scratch.path());
        let repo = Repository::resolve_publication(
            scratch.path(),
            &env,
            "github.example/org/repo",
            until,
            &cancelled,
        )
        .unwrap();
        let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
        for key in [
            "fetch.bundleURI",
            "fetch.bundleCreationToken",
            "remote.origin.promisor",
            "remote.origin.partialCloneFilter",
            "transfer.bundleURI",
        ] {
            c.read(scratch.path(), &["config", key, "true"]).unwrap();
            assert!(
                private
                    .prepare_source(&repo, c)
                    .unwrap_err()
                    .to_string()
                    .contains("bundle/partial-clone"),
                "{key}"
            );
            c.read(scratch.path(), &["config", "--unset", key]).unwrap();
        }
        private.settle().unwrap();
    }
    #[test]
    fn sh871_private_fetch_requires_every_supported_control() {
        let all = "refetch refmap write-fetch-head prune-tags recurse-submodules auto-maintenance write-commit-graph";
        assert!(capabilities(all));
        for flag in all.split_whitespace() {
            assert!(!capabilities(&all.replace(flag, "")), "{flag}");
        }
    }
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut result = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    result.insert(
                        entry.path().strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        result
    }
    #[test]
    fn sh871_private_fetch_supported_local_transfer_preserves_source_namespace() {
        // Real local Git transport proves write isolation of these flags/env.
        // Production HTTPS/auth/origin guards are separately source-reviewed;
        // this fixture does not substitute file transport into their API.
        let scratch = storyhook_test_support::scratch_dir();
        let remote = scratch.path().join("remote");
        let source = scratch.path().join("source");
        fs::create_dir(&remote).unwrap();
        fs::create_dir(&source).unwrap();
        let until = deadline();
        let cancelled = || false;
        let c = control(until, &cancelled);
        c.read(&remote, &["init", "-q"]).unwrap();
        for (key, value) in [
            ("user.name", "Landed Fixture"),
            ("user.email", "landed@example.test"),
            ("storyhookIdentity.fixture.name", "Landed Fixture"),
            ("storyhookIdentity.fixture.email", "landed@example.test"),
            ("storyhookIdentity.fixture.role", "both"),
            (
                "storyhookIdentity.fixture.reason",
                "Isolated real-Git landed observation fixture",
            ),
        ] {
            c.read(&remote, &["config", "--local", key, value]).unwrap();
        }
        c.read(
            &remote,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        )
        .unwrap();
        let head = c.read(&remote, &["rev-parse", "HEAD"]).unwrap();
        c.read(&source, &["init", "-q"]).unwrap();
        c.read(
            &source,
            &["config", "remote.origin.url", remote.to_str().unwrap()],
        )
        .unwrap();
        for (key, value) in [
            ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
            ("fetch.prune", "true"),
            ("fetch.pruneTags", "true"),
            ("fetch.writeCommitGraph", "true"),
            ("fetch.recurseSubmodules", "true"),
            ("maintenance.auto", "true"),
        ] {
            c.read(&source, &["config", key, value]).unwrap();
        }
        fs::write(source.join(".git/FETCH_HEAD"), b"sentinel\n").unwrap();
        let before = snapshot(&source.join(".git"));
        let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
        let mut command = git_env::command(&source);
        command.args(arguments(&[head.trim()]).unwrap());
        private.configure(&mut command).unwrap();
        let result = c.capture(command).unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(before, snapshot(&source.join(".git")));
        private
            .read(
                &["fsck", "--full", "--no-dangling", head.trim()],
                until,
                &cancelled,
            )
            .unwrap();
        private
            .read(&["cat-file", "-e", head.trim()], until, &cancelled)
            .unwrap();
        private.settle().unwrap();
    }
    #[test]
    fn sh871_private_fetch_live_pins_do_not_replace_settled_namespace_proof() {
        let scratch = storyhook_test_support::scratch_dir();
        let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
        // An in-flight callback must inspect stable ownership, not recurse into
        // Git's changing pack namespace. Acceptance still requires the full
        // settled scan, which refuses this deliberately substituted entry.
        let path = private.path.join("objects/pack/redirect");
        std::os::unix::fs::symlink(scratch.path(), &path).unwrap();
        private.validate_live_custody().unwrap();
        assert!(private.validate().is_err());
        fs::remove_file(path).unwrap();
        private.validate().unwrap();
        private.settle().unwrap();
    }
    #[test]
    fn sh871_private_fetch_expired_control_retains_new_resources() {
        let scratch = storyhook_test_support::scratch_dir();
        let private = PrivateFetch::create_at(scratch.path(), "sha1").unwrap();
        let path = private.path.clone();
        assert!(
            private
                .read(&["rev-parse", "HEAD"], Instant::now(), &|| false)
                .is_err()
        );
        drop(private);
        assert!(path.exists());
    }
}
