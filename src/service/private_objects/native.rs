//! Controlled inspection owns both object and Git administration namespaces.
//! Capture uncertainty retains them; no destructor removes possible live roots.
use crate::{
    error::AppError,
    process::{Cancellation, Captured, run_captured_query_quiescent},
};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

const LIMIT: u64 = 8 * 1024 * 1024;

struct Pin {
    path: PathBuf,
    file: File,
    device: u64,
    inode: u64,
}
impl Pin {
    fn open(path: PathBuf) -> Result<Self, AppError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(storage)?;
        let metadata = file.metadata().map_err(storage)?;
        Ok(Self {
            path,
            file,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    fn validate(&self) -> Result<(), AppError> {
        let observed = fs::symlink_metadata(&self.path).map_err(storage)?;
        let held = self.file.metadata().map_err(storage)?;
        if !observed.is_dir()
            || observed.file_type().is_symlink()
            || observed.dev() != self.device
            || observed.ino() != self.inode
            || held.dev() != self.device
            || held.ino() != self.inode
            || self.path.canonicalize().map_err(storage)? != self.path
        {
            return Err(storage("original private namespace directory changed"));
        }
        Ok(())
    }
}

pub(super) struct NativeObjects {
    root: Option<Pin>,
    admin: Pin,
    objects: Pin,
    source: PathBuf,
    label: &'static str,
    deadline: Instant,
    cancellation: Cancellation,
    uncertain: Cell<bool>,
    reported: Cell<bool>,
}

impl NativeObjects {
    pub(super) fn open(
        checkout: &Path,
        label: &'static str,
        prefix: &str,
        deadline: Instant,
        cancellation: Cancellation,
    ) -> Result<Self, AppError> {
        let query = |args: &[&str]| -> Result<String, AppError> {
            let mut command = crate::env::git_env::command(checkout);
            command
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .env("GIT_NO_LAZY_FETCH", "1")
                .args(args);
            let answer = run_captured_query_quiescent(
                command,
                deadline,
                &|| cancellation.is_cancelled(),
                LIMIT,
                &[],
            )
            .map_err(|e| storage(format!("{label} native namespace setup: {}", e.detail())))?;
            if !answer.status.success() || answer.stdout_truncated {
                return Err(storage(format!(
                    "{label} native namespace setup refused: {}",
                    String::from_utf8_lossy(&answer.stderr)
                )));
            }
            String::from_utf8(answer.stdout)
                .map(|s| s.trim_end_matches('\n').to_owned())
                .map_err(storage)
        };
        let source = PathBuf::from(query(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?)
        .join("objects");
        let format = query(&["rev-parse", "--show-object-format"])?;
        if !matches!(format.as_str(), "sha1" | "sha256") {
            return Err(storage("unsupported native object format"));
        }
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            return Err(storage("native namespace preparation expired or cancelled"));
        }
        let directory = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(storage)?;
        // Disarm automatic deletion before the first command can borrow a path.
        let retained = directory.keep();
        let created = (|| {
            let root = retained.canonicalize().map_err(storage)?;
            fs::create_dir(root.join("admin")).map_err(storage)?;
            fs::create_dir(root.join("objects")).map_err(storage)?;
            Ok(Self {
                admin: Pin::open(root.join("admin"))?,
                objects: Pin::open(root.join("objects"))?,
                root: Some(Pin::open(root)?),
                source,
                label,
                deadline,
                cancellation,
                uncertain: Cell::new(false),
                reported: Cell::new(false),
            })
        })();
        let native = created.map_err(|e: AppError| {
            storage(format!(
                "{e}; native namespace preparation retained at {}",
                retained.display()
            ))
        })?;
        let mut init = native.command(&[]);
        init.args([
            "init",
            "--bare",
            "--quiet",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ]);
        native.capture(init, &[], None, &|| false)?;
        Ok(native)
    }

    pub(super) fn object_path(&self) -> &Path {
        &self.objects.path
    }
    pub(super) fn source(&self) -> &Path {
        &self.source
    }

    fn command(&self, env: &[(&str, &str)]) -> Command {
        let mut command = crate::env::git_env::command(&self.admin.path);
        command
            .envs(env.iter().copied())
            .env("GIT_DIR", &self.admin.path)
            .env("GIT_COMMON_DIR", &self.admin.path)
            .env("GIT_OBJECT_DIRECTORY", &self.objects.path)
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                super::super::isolated_merge::quote_alternate(&self.source),
            )
            .env("GIT_INDEX_FILE", self.admin.path.join("native.index"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .args([
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "merge.default=text",
                "-c",
                "protocol.allow=never",
            ]);
        command
    }

    fn validate(&self) -> Result<(), AppError> {
        self.root
            .as_ref()
            .ok_or_else(|| storage("native namespace already closed"))?
            .validate()?;
        self.admin.validate()?;
        self.objects.validate()
    }

    fn retained(&self, detail: impl std::fmt::Display) -> AppError {
        self.reported.set(true);
        storage(format!(
            "{}: {detail}; private object/admin namespace retained at {}",
            self.label,
            self.root.as_ref().expect("live root").path.display()
        ))
    }

    fn capture(
        &self,
        command: Command,
        answers: &'static [i32],
        shorter: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Captured, AppError> {
        if self.uncertain.get() {
            return Err(self.retained("prior capture settlement is uncertain"));
        }
        self.validate().map_err(|e| self.retained(e))?;
        let deadline = shorter.map_or(self.deadline, |d| d.min(self.deadline));
        let cancelled = || self.cancellation.is_cancelled() || cancelled();
        if cancelled() || Instant::now() >= deadline {
            return Err(self.retained("native inspection expired or cancelled before spawn"));
        }
        self.uncertain.set(true);
        let result = run_captured_query_quiescent(command, deadline, &cancelled, LIMIT, answers)
            .map_err(|e| self.retained(e.detail()))?;
        // Only a successful quiescent capture proves no owned writer remains.
        // Any capture error conservatively retains both roots, even if its
        // internal termination may have succeeded; no AppError guesses custody.
        self.uncertain.set(false);
        self.validate().map_err(|e| self.retained(e))?;
        if cancelled() || Instant::now() >= deadline {
            return Err(self.retained("native inspection expired or cancelled after capture"));
        }
        if result.stdout_truncated
            || (!result.status.success()
                && !result
                    .status
                    .code()
                    .is_some_and(|code| answers.contains(&code)))
        {
            return Err(self.retained(format!(
                "native Git answer refused: {}",
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        Ok(result)
    }

    pub(super) fn query(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        answers: &'static [i32],
        deadline: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Captured, AppError> {
        let mut command = self.command(env);
        command.args(args);
        self.capture(command, answers, deadline, cancelled)
    }

    pub(super) fn merge(
        &self,
        parents: [&str; 2],
        nul: bool,
        deadline: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Captured, AppError> {
        super::super::isolated_merge::merge_native(
            || self.command(&[]),
            parents,
            nul,
            self.label,
            |command, answers| self.capture(command, answers, deadline, cancelled),
        )
    }

    pub(super) fn close(mut self) -> Result<(), AppError> {
        if self.uncertain.get() {
            return Err(self.retained("capture settlement uncertain; cleanup refused"));
        }
        self.validate().map_err(|e| self.retained(e))?;
        let root = &self.root.as_ref().expect("live root").path;
        let mut count = 0;
        safe_tree(root, self.objects.device, 0, &mut count).map_err(|e| self.retained(e))?;
        // Both administration and objects are under the same held original
        // root. No separate merge-admin TempDir can be silently dropped.
        fs::remove_dir_all(root)
            .map_err(|e| self.retained(format!("explicit private cleanup failed: {e}")))?;
        self.root = None;
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(storage(
                "native inspection expired or cancelled after explicit cleanup",
            ));
        }
        Ok(())
    }
}

impl Drop for NativeObjects {
    fn drop(&mut self) {
        // Refusal/error paths may unwind before explicit close. Never remove
        // their possibly borrowed roots; name residue even for parse failures.
        if let Some(root) = &self.root
            && !self.reported.get()
        {
            eprintln!(
                "{}: unclosed private object/admin namespace retained at {}",
                self.label,
                root.path.display()
            );
        }
    }
}

fn safe_tree(path: &Path, device: u64, depth: usize, count: &mut usize) -> Result<(), AppError> {
    *count += 1;
    if depth > 128 || *count > 65_536 {
        return Err(storage("private cleanup inventory exceeds bound"));
    }
    let metadata = fs::symlink_metadata(path).map_err(storage)?;
    if metadata.dev() != device
        || metadata.file_type().is_symlink()
        || (!metadata.is_dir() && !metadata.is_file())
        || (metadata.is_file() && metadata.nlink() != 1)
    {
        return Err(storage(
            "private cleanup refuses a redirect, shared file or foreign namespace",
        ));
    }
    if metadata.is_dir() {
        for child in fs::read_dir(path).map_err(storage)? {
            safe_tree(&child.map_err(storage)?.path(), device, depth + 1, count)?;
        }
    }
    Ok(())
}
fn storage(error: impl std::fmt::Display) -> AppError {
    AppError::Storage(error.to_string())
}

#[cfg(test)]
mod tests;
