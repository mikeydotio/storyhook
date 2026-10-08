//! Capture the existing protected merge command, without changing its policy.
use super::Repository;
use crate::error::AppError;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct MergeReply {
    result: &'static str,
    status: Option<u16>,
}

impl Repository {
    /// Keep GitHub CLI's policy and merge-queue checks and exact-head fence.
    /// There is no admin, force, auto flag or second mutation after failure.
    pub(super) fn merge_once(&self, number: u64, head: &str) -> Result<MergeReply, AppError> {
        if number == 0 || head.len() != 40 || !head.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppError::Validation("invalid merge number or head".into()));
        }
        let current = Self::resolve_with_bounds(&self.checkout, self.bounds)?;
        if current.identity != self.identity {
            return Err(AppError::Validation(
                "GitHub origin changed before merge".into(),
            ));
        }
        let args = [
            "pr".into(),
            "merge".into(),
            number.to_string(),
            "--merge".into(),
            "--match-head-commit".into(),
            head.into(),
        ];
        let output =
            super::command::capture(self.bounds.operation, &self.identity, &self.checkout, &args)?;
        if output.status.success() {
            // Includes accepted queue requests. The caller must still observe
            // MERGED and prove the exact landed tree before completing anything.
            return Ok(MergeReply {
                result: "accepted",
                status: None,
            });
        }
        // This captures only the merge subprocess, not combined shell output.
        // gh's typed HTTPError formatter names the response status and request
        // URL. Unknown formats/GraphQL 200 errors/transport errors stay fenced.
        let status = if output.stdout.is_empty() && !output.stdout_truncated {
            refusal_status(
                &output.stderr,
                &self.identity.host,
                &self.identity.owner,
                &self.identity.repo,
                number,
            )
        } else {
            None
        };
        Ok(MergeReply {
            result: if status.is_some() {
                "refused"
            } else {
                "uncertain"
            },
            status,
        })
    }
}

fn refusal_status(bytes: &[u8], host: &str, owner: &str, repo: &str, number: u64) -> Option<u16> {
    if bytes.len() >= 64 * 1024 {
        return None;
    }
    let text = std::str::from_utf8(bytes)
        .ok()?
        .trim_end_matches(['\r', '\n']);
    // Require the entire canonical one-line HTTPError, not an HTTP-looking
    // substring in a PR title, GraphQL message, stack trace or timeout detail.
    if text.contains(['\r', '\n']) {
        return None;
    }
    let rest = text.strip_prefix("HTTP ")?;
    let status = rest.get(..3)?.parse::<u16>().ok()?;
    if !(400..500).contains(&status) || status == 408 {
        return None;
    }
    let message = rest.get(3..)?;
    if !message.starts_with(": ") && !message.starts_with(" (") {
        return None;
    }
    let (api, graphql) = if host.eq_ignore_ascii_case("github.com") {
        (
            "https://api.github.com".to_owned(),
            "https://api.github.com/graphql".to_owned(),
        )
    } else {
        (
            format!("https://{host}/api/v3"),
            format!("https://{host}/api/graphql"),
        )
    };
    let merge = format!("{api}/repos/{owner}/{repo}/pulls/{number}/merge");
    if !message.ends_with(&format!("({graphql})")) && !message.ends_with(&format!("({merge})")) {
        return None;
    }
    Some(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sh842_only_causal_merge_http_errors_are_definitive() {
        for status in [400, 401, 403, 404, 405, 409, 422, 429] {
            for endpoint in [
                "https://api.github.com/graphql",
                "https://api.github.com/repos/acme/widgets/pulls/42/merge",
            ] {
                assert_eq!(
                    refusal_status(
                        format!("HTTP {status}: refused ({endpoint})\n").as_bytes(),
                        "github.com",
                        "acme",
                        "widgets",
                        42
                    ),
                    Some(status)
                );
            }
        }
        for raw in [
            "HTTP 408: timeout (https://api.github.com/graphql)",
            "HTTP 500: failed (https://api.github.com/graphql)",
            "GraphQL: HTTP 405: title (https://api.github.com/graphql)",
            "request failed: HTTP 405",
            "HTTP 405: refused (https://evil.example/graphql)",
            "HTTP 405: refused (https://api.github.com/repos/acme/widgets/pulls/43/merge)",
            "HTTP 405: refused (https://api.github.com/graphql) trailing",
            "HTTP 405: refused (https://api.github.com/graphql)\ntransport failure",
            "HTTP 405: incomplete",
        ] {
            assert_eq!(
                refusal_status(raw.as_bytes(), "github.com", "acme", "widgets", 42),
                None,
                "{raw}"
            );
        }
        assert_eq!(
            refusal_status(
                b"HTTP 405 (https://github.example/api/graphql)",
                "github.example",
                "acme",
                "widgets",
                42
            ),
            Some(405)
        );
    }
}
