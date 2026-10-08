//! Git object transport pinned to the validated HTTPS destination.

use super::{Repository, parse_origin};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(super) struct PublicationControl<'a> {
    pub deadline: Instant,
    pub cancelled: &'a dyn Fn() -> bool,
}

impl PublicationControl<'_> {
    pub(super) fn capture(
        self,
        command: Command,
    ) -> Result<crate::process::Captured, crate::process::CaptureError> {
        crate::process::run_captured_query_quiescent(
            command,
            self.deadline,
            self.cancelled,
            64 * 1024,
            &[],
        )
    }
    pub(super) fn read(self, checkout: &Path, arguments: &[&str]) -> Result<String, AppError> {
        let mut command = git_env::command(checkout);
        command.args(arguments);
        let output = self.capture(command).map_err(|e| {
            AppError::Validation(format!("publication origin read: {}", e.detail()))
        })?;
        if !output.status.success() || output.stdout_truncated || output.stdout.len() >= 64 * 1024 {
            return Err(AppError::Validation(
                "publication origin read failed or exceeded capture limit".into(),
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| AppError::Validation("publication origin read is not UTF-8".into()))
    }
}
use crate::env::{git_env, spawn_env};
use crate::error::AppError;
use crate::process::run_captured_private;

impl Repository {
    /// Runs clone, fetch, push or ls-remote for origin with gh's credential helper.
    pub fn git(&self, arguments: &[String]) -> Result<Vec<u8>, AppError> {
        self.git_with_control(arguments, None, None, None)
    }

    /// Resolve a publication origin using the same absolute, quiescent lifetime.
    pub(crate) fn resolve_publication(
        checkout: &Path,
        env: &crate::env::Environment,
        expected_repository: &str,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, AppError> {
        let control = PublicationControl {
            deadline,
            cancelled,
        };
        let repository = Self::resolve_reading(
            checkout,
            super::Bounds::for_environment(env),
            |path, args| control.read(path, args),
        )?;
        if repository.qualified() != expected_repository {
            return Err(AppError::Validation(
                "publication origin differs from immutable submission".into(),
            ));
        }
        Ok(repository)
    }

    /// Protected transport retaining normal source hooks and configuration.
    /// The optional alternate is only a native-custody-validated private object
    /// directory. This is not an arbitrary environment/configuration channel.
    pub(crate) fn git_publication(
        &self,
        arguments: &[String],
        objects: Option<&Path>,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        self.git_with_control(
            arguments,
            Some(PublicationControl {
                deadline,
                cancelled,
            }),
            objects,
            None,
        )
    }

    pub(super) fn git_private_fetch(
        &self,
        target: &super::private_fetch::PrivateFetch,
        oids: &[&str],
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        target.validate()?;
        let arguments = super::private_fetch::arguments(oids)?;
        self.git_with_control(
            &arguments,
            Some(PublicationControl {
                deadline,
                cancelled,
            }),
            None,
            Some(target),
        )
    }

    fn git_with_control(
        &self,
        arguments: &[String],
        control: Option<PublicationControl<'_>>,
        objects: Option<&Path>,
        private_fetch: Option<&super::private_fetch::PrivateFetch>,
    ) -> Result<Vec<u8>, AppError> {
        let read = |_: Duration, path: &Path, args: &[&str]| match control {
            Some(control) => control.read(path, args),
            None => super::git_read_with_bound(self.bounds.read, path, args),
        };
        let capture = |command: Command, bound: Duration| match control {
            Some(control) => control.capture(command),
            None => run_captured_private(command, bound),
        };
        let refuse = |detail: &str| {
            AppError::Validation(format!(
                "GitHub transport destination for {}: {detail}",
                self.qualified()
            ))
        };
        if Self::resolve_reading(&self.checkout, self.bounds, |path, args| {
            read(self.bounds.read, path, args)
        })?
        .identity
            != self.identity
        {
            return Err(refuse("origin changed; resolve again before retrying"));
        }
        let operation = arguments.first().map(String::as_str).unwrap_or_default();
        if !matches!(operation, "clone" | "fetch" | "push" | "ls-remote") {
            return Err(refuse("expected clone, fetch, push or ls-remote"));
        }
        let remote = arguments
            .iter()
            .position(|argument| argument == "origin")
            .ok_or_else(|| refuse("an explicit origin operand is required"))?;
        const FLAGS: &[&str] = &[
            "--quiet",
            "-q",
            "--prune",
            "--tags",
            "--heads",
            "--symref",
            "--exit-code",
            "--porcelain",
            "--no-tags",
            "--no-recurse-submodules",
        ];
        let allowed_flags = if operation == "clone" {
            &["--quiet", "-q", "--mirror", "--bare", "--no-checkout"][..]
        } else {
            FLAGS
        };
        if arguments[1..remote].iter().any(|arg| {
            !allowed_flags.contains(&arg.as_str())
                && !(control.is_some()
                    && operation == "push"
                    && matches!(arg.as_str(), "--no-follow-tags" | "--recurse-submodules=no"))
                && !(operation == "fetch" && arg == "--no-write-fetch-head")
                && !(private_fetch.is_some() && super::private_fetch::FLAGS.contains(&arg.as_str()))
        }) {
            return Err(refuse("unsupported transport option"));
        }
        let operands = &arguments[remote + 1..];
        if operation == "clone" {
            if operands.len() != 1
                || !std::path::Path::new(&operands[0]).is_absolute()
                || operands[0].chars().any(char::is_control)
            {
                return Err(refuse(
                    "clone requires exactly one absolute destination path",
                ));
            }
        } else if operands.iter().any(|arg| {
            arg.starts_with('-') || arg.contains("://") || arg.chars().any(char::is_whitespace)
        }) {
            return Err(refuse("unsupported transport refspec"));
        }
        // Check both configured push URLs and actual rewrite results before
        // pinning transport. A mismatch must not silently disappear.
        for flags in [
            vec!["remote", "get-url", "--all", "origin"],
            vec!["remote", "get-url", "--push", "--all", "origin"],
        ] {
            let answer = read(self.bounds.read, &self.checkout, &flags)?;
            let urls: Vec<_> = answer.lines().collect();
            if urls.len() != 1
                || parse_origin(urls[0])
                    .map(|repo| repo != self.identity)
                    .unwrap_or(true)
            {
                return Err(refuse("configured URL or rewrite differs from origin"));
            }
        }
        let destination = self.transport_url();
        let effective = read(
            self.bounds.read,
            &self.checkout,
            &["ls-remote", "--get-url", &destination],
        )?;
        if effective.trim() != destination {
            return Err(refuse(
                "an inherited URL rewrite changes the HTTPS destination",
            ));
        }
        let mut rewrites = git_env::command(&self.checkout);
        rewrites.args([
            "config",
            "--null",
            "--get-regexp",
            "^url\\..*\\.pushinsteadof$",
        ]);
        let rewrites = capture(rewrites, self.bounds.read).map_err(|error| {
            refuse(&format!(
                "cannot read push URL rewrites: {}",
                error.detail()
            ))
        })?;
        if !rewrites.status.success() && rewrites.status.code() != Some(1) {
            return Err(refuse("cannot read push URL rewrites"));
        }
        if rewrites.stdout.len() >= 64 * 1024 {
            return Err(refuse(
                "push URL rewrite configuration exceeds the capture limit",
            ));
        }
        for entry in rewrites
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry =
                std::str::from_utf8(entry).map_err(|_| refuse("invalid push URL rewrite"))?;
            let (_, prefix) = entry
                .split_once('\n')
                .ok_or_else(|| refuse("invalid push URL rewrite"))?;
            if destination.starts_with(prefix) {
                return Err(refuse(
                    "a pushInsteadOf rule matches the HTTPS destination; remove the conflicting rule",
                ));
            }
        }
        let mut args = arguments.to_vec();
        let mut command = git_env::command(&self.checkout);
        if operation == "push" {
            // Hooks need origin to identify outgoing history. Exact, temporary
            // mappings also work on Git 2.25, which cannot clear URL lists with
            // empty values. Validate the mapped fetch and push destinations;
            // an existing competing rewrite must never win silently.
            let configured = read(
                self.bounds.read,
                &self.checkout,
                &[
                    "config",
                    "--null",
                    "--get-regexp",
                    "^remote\\.origin\\.(url|pushurl)$",
                ],
            )?;
            if configured.len() >= 64 * 1024 {
                return Err(refuse("origin configuration exceeds the capture limit"));
            }
            let mut pin = Vec::new();
            for entry in configured.split('\0').filter(|entry| !entry.is_empty()) {
                let (_, url) = entry
                    .split_once('\n')
                    .ok_or_else(|| refuse("invalid origin URL record"))?;
                if parse_origin(url)
                    .map(|identity| identity != self.identity)
                    .unwrap_or(true)
                {
                    return Err(refuse("configured raw URL differs from origin"));
                }
                pin.push("-c".to_string());
                pin.push(format!("url.{destination}.insteadOf={url}"));
            }
            for flags in [
                vec!["remote", "get-url", "--all", "origin"],
                vec!["remote", "get-url", "--push", "--all", "origin"],
            ] {
                let mut query: Vec<_> = pin.iter().map(String::as_str).collect();
                query.extend(flags);
                let effective = read(self.bounds.read, &self.checkout, &query)?;
                if effective.lines().collect::<Vec<_>>() != [destination.as_str()] {
                    return Err(refuse(
                        "cannot pin origin to HTTPS; remove competing URL rewrites",
                    ));
                }
            }
            command.args(pin);
        } else {
            args[remote] = destination;
        }
        for name in spawn_env::GITHUB_CREDENTIAL_MAY_SEE {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("GH_HOST", &self.identity.host)
            .env("GH_REPO", self.qualified())
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .args([
                "-c",
                "core.askPass=",
                "-c",
                "credential.helper=",
                "-c",
                "http.followRedirects=false",
                "-c",
                "fetch.recurseSubmodules=false",
            ])
            .arg("-c")
            .arg(format!("credential.https://{}.helper=", self.identity.host))
            .arg("-c")
            .arg(format!(
                "credential.https://{}.helper=!gh auth git-credential",
                self.identity.host
            ))
            .args(args);
        if let Some(objects) = objects {
            if operation != "push" || control.is_none() || !valid_private_objects(objects) {
                return Err(refuse("invalid native private object directory"));
            }
            command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects);
        }
        if let Some(target) = private_fetch {
            if operation != "fetch" || objects.is_some() || control.is_none() {
                return Err(refuse("invalid private fetch capability"));
            }
            // Refuse auxiliary bundle transfer/config writes. Recheck immediately
            // before the command; do not override the user's source configuration.
            target.prepare_source(self, control.expect("checked above"))?;
            if Self::resolve_reading(&self.checkout, self.bounds, |path, args| {
                read(self.bounds.read, path, args)
            })?
            .identity
                != self.identity
            {
                return Err(refuse("origin changed during private fetch preparation"));
            }
            target.configure(&mut command)?;
        }
        let output = capture(command, self.bounds.operation).map_err(|error| {
            AppError::GithubApi(format!(
                "Git {operation} for {}: {}; operation was not retried",
                self.qualified(),
                error.detail()
            ))
        })?;
        if !output.status.success() {
            let mut detail = String::from_utf8_lossy(&output.stderr).into_owned();
            for name in spawn_env::GITHUB_CREDENTIAL_MAY_SEE {
                if name.ends_with("TOKEN")
                    && let Ok(value) = std::env::var(name)
                    && !value.is_empty()
                {
                    detail = detail.replace(&value, "[REDACTED]");
                }
            }
            return Err(AppError::GithubApi(format!(
                "Git {operation} for {} failed ({}): {}",
                self.qualified(),
                output.status,
                crate::daemon::crash::redact(&detail)
            )));
        }
        if output.stdout.len() >= 64 * 1024 {
            return Err(refuse("transport answer exceeded the capture limit"));
        }
        Ok(output.stdout)
    }
}

fn valid_private_objects(objects: &Path) -> bool {
    objects.is_absolute()
        && objects
            .to_str()
            .is_some_and(|path| !path.contains([':', '"']) && !path.chars().any(char::is_control))
        && objects
            .canonicalize()
            .is_ok_and(|canonical| canonical == objects)
        && std::fs::symlink_metadata(objects)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn sh871_publication_alternate_requires_one_canonical_directory() {
        let scratch = storyhook_test_support::scratch_dir();
        let root = scratch.path().canonicalize().unwrap();
        let objects = root.join("objects");
        std::fs::create_dir(&objects).unwrap();
        assert!(valid_private_objects(&objects));
        for name in [
            "objects:other",
            "objects\nother",
            "objects\tother",
            "objects\"other",
        ] {
            let path = root.join(name);
            std::fs::create_dir(&path).unwrap();
            assert!(!valid_private_objects(&path), "{name:?}");
        }
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&objects, &alias).unwrap();
        assert!(!valid_private_objects(&alias));
        assert!(!valid_private_objects(Path::new("objects")));
    }
    #[test]
    fn sh871_publication_control_refuses_cancel_or_expiry_before_spawn() {
        let never_spawn = || Command::new("/storyhook-fixture-command-must-not-spawn");
        let cancelled = || true;
        let control = PublicationControl {
            deadline: Instant::now() + Duration::from_secs(60),
            cancelled: &cancelled,
        };
        assert!(matches!(
            control.capture(never_spawn()),
            Err(crate::process::CaptureError::Cancelled)
        ));
        let live = || false;
        let control = PublicationControl {
            deadline: Instant::now(),
            cancelled: &live,
        };
        assert!(matches!(
            control.capture(never_spawn()),
            Err(crate::process::CaptureError::Wait(error)) if error.kind() == std::io::ErrorKind::TimedOut
        ));
    }
    #[test]
    fn sh871_publication_rejects_origin_swap_before_effect_resolution() {
        let scratch = storyhook_test_support::scratch_dir();
        let root = scratch.path().canonicalize().unwrap();
        let env = crate::env::Environment::at(&root).with_subprocess_patience();
        let deadline = Instant::now()
            + storyhook_test_support::load_grace::graced_now(Duration::from_secs(60));
        let cancelled = || false;
        let control = PublicationControl {
            deadline,
            cancelled: &cancelled,
        };
        control.read(&root, &["init", "--quiet"]).unwrap();
        control
            .read(
                &root,
                &[
                    "config",
                    "remote.origin.url",
                    "https://github.example/org/original.git",
                ],
            )
            .unwrap();
        assert!(
            Repository::resolve_publication(
                &root,
                &env,
                "github.example/org/original",
                deadline,
                &cancelled
            )
            .is_ok()
        );
        // The fresh resolver must bind the immutable identity, not merely find
        // a self-consistent replacement origin before push or PR creation.
        control
            .read(
                &root,
                &[
                    "config",
                    "remote.origin.url",
                    "https://github.example/org/replacement.git",
                ],
            )
            .unwrap();
        assert!(
            Repository::resolve_publication(
                &root,
                &env,
                "github.example/org/original",
                deadline,
                &cancelled
            )
            .is_err()
        );
        // Prove refusal is identity-specific, not a broken repository fixture.
        assert!(
            Repository::resolve_publication(
                &root,
                &env,
                "github.example/org/replacement",
                deadline,
                &cancelled
            )
            .is_ok()
        );
    }
}
