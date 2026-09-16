//! Git object transport pinned to the validated HTTPS destination.

use super::{Repository, git_read, parse_origin};
use crate::env::{git_env, spawn_env};
use crate::error::AppError;
use crate::process::run_captured_private;
use std::time::Duration;

impl Repository {
    /// Runs clone, fetch, push or ls-remote for origin with gh's credential helper.
    pub fn git(&self, arguments: &[String]) -> Result<Vec<u8>, AppError> {
        let refuse = |detail: &str| {
            AppError::Validation(format!(
                "GitHub transport destination for {}: {detail}",
                self.qualified()
            ))
        };
        if Self::resolve(&self.checkout)?.identity != self.identity {
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
        if arguments[1..remote]
            .iter()
            .any(|arg| !allowed_flags.contains(&arg.as_str()))
        {
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
            let answer = git_read(&self.checkout, &flags)?;
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
        let effective = git_read(&self.checkout, &["ls-remote", "--get-url", &destination])?;
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
        let rewrites =
            run_captured_private(rewrites, Duration::from_secs(30)).map_err(|error| {
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
            let configured = git_read(
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
                let effective = git_read(&self.checkout, &query)?;
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
        let output = run_captured_private(command, Duration::from_secs(120)).map_err(|error| {
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
