//! Strict libtest observations. Parsed output never grants repair authority.

use super::ProbeOutcome;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

static SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\Atest result: (ok|FAILED)\. ([01]) passed; ([01]) failed; 0 ignored; 0 measured; ([0-9]+) filtered out; finished in [0-9]+\.[0-9]+s\z").expect("static summary grammar")
});
static PANIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^thread '([^'\n]+)'(?: \(([0-9]+)\))? panicked at ([^\n]+):$")
        .expect("static panic grammar")
});

/// One literal Cargo target; diagnostic selection never expands a target glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RustTarget {
    /// The selected package's library test harness.
    Library,
    /// One integration-test harness with this literal Cargo target name.
    Integration(String),
    /// One binary test harness with this literal Cargo target name.
    Binary(String),
}

/// An exact test in one package and one libtest harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustCase {
    pub(super) package: String,
    pub(super) target: RustTarget,
    pub(super) name: String,
}

/// Interpreted execution evidence; environment and cleanup require separate proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustCaseObservation {
    /// Confirmed executions of the selected test, zero when unavailable.
    pub executions: u32,
    /// The observed result, without attributing responsibility.
    pub outcome: ProbeOutcome,
}

impl RustCase {
    /// Stable exact case identity for durable failure components.
    pub(crate) fn check_identity(&self) -> Option<String> {
        let RustTarget::Integration(target) = &self.target else {
            return None;
        };
        Some(format!("rust:{}:{target}:{}", self.package, self.name))
    }

    /// Match the selected literal target and case against an original gate observation.
    pub(crate) fn matches_failure(&self, failure: &crate::store::GateFailedCase) -> bool {
        matches!(&self.target, RustTarget::Integration(target) if failure.target.as_ref() == Some(target))
            && failure.name.as_ref() == Some(&self.name)
            && !failure.path.trim().is_empty()
    }

    /// Bind an original gate log to one exact Cargo frame and native assertion failure.
    /// Unsupported or ambiguous framing is unknown, never a guessed reproduction.
    pub fn original_failure(&self, log: &[u8]) -> Result<String, String> {
        if log.len() > 16 * 1024 * 1024 {
            return Err("original gate log exceeds the native observation bound".into());
        }
        let RustTarget::Integration(target) = &self.target else {
            return Err("original gate adapter requires an integration target".into());
        };
        let text = complete_text(log, &[], false)?;
        let header = format!("     Running tests/{target}.rs (");
        let mut offset = 0;
        let mut frames = Vec::new();
        for line in text.split_inclusive('\n') {
            if line.starts_with("     Running ") {
                frames.push((offset, offset + line.len(), line));
            }
            offset += line.len();
        }
        let matching: Vec<_> = frames
            .iter()
            .enumerate()
            .filter(|(_, (_, _, line))| line.starts_with(&header) && line.ends_with(")\n"))
            .collect();
        let [(index, (_, start, _))] = matching.as_slice() else {
            return Err("original gate lacks one unambiguous exact Cargo target frame".into());
        };
        let end = frames
            .get(index + 1)
            .map_or(text.len(), |(offset, _, _)| *offset);
        let frame = &text[*start..end];
        let summaries: Vec<_> = frame.match_indices("\ntest result:").collect();
        let [(summary, _)] = summaries.as_slice() else {
            return Err("original native result is incomplete or ambiguous".into());
        };
        let summary_end = frame[*summary + 1..]
            .find('\n')
            .map(|n| *summary + 1 + n + 1)
            .ok_or("original native summary is truncated")?;
        if frame[summary_end..].lines().any(|line| {
            ["running ", "test ", "failures:", "---- ", "thread '"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        }) {
            return Err(
                "original native frame contains additional or incomplete test output".into(),
            );
        }
        let observation = self.observe(&frame.as_bytes()[..summary_end], &[], false, Some(101));
        match observation.outcome {
            ProbeOutcome::Failed { signature } if observation.executions == 1 => Ok(signature),
            _ => Err("original native frame is not the exact complete assertion failure".into()),
        }
    }

    /// Validate a literal package, target and case before any command is built.
    pub fn new(package: &str, target: RustTarget, name: &str) -> Result<Self, String> {
        if !literal_target(package)
            || match &target {
                RustTarget::Library => false,
                RustTarget::Integration(name) | RustTarget::Binary(name) => !literal_target(name),
            }
            || !name.split("::").all(|part| {
                part.starts_with(|c: char| c == '_' || c.is_alphabetic())
                    && part.chars().all(|c| c == '_' || c.is_alphanumeric())
            })
        {
            return Err(
                "Rust diagnosis requires literal package, target and exact case names".into(),
            );
        }
        Ok(Self {
            package: package.into(),
            target,
            name: name.into(),
        })
    }

    /// Cargo arguments to build just this target; execution is a separate supervised step.
    pub fn build_arguments(&self) -> Vec<String> {
        let mut args = vec!["test", "--offline", "--locked", "--package", &self.package];
        match &self.target {
            RustTarget::Library => args.push("--lib"),
            RustTarget::Integration(name) => args.extend(["--test", name]),
            RustTarget::Binary(name) => args.extend(["--bin", name]),
        }
        args.extend(["--no-run", "--message-format=json"]);
        args.into_iter().map(str::to_string).collect()
    }

    /// Native harness arguments to prove the selected test exists before execution.
    pub fn list_arguments(&self) -> Vec<String> {
        ["--list", "--exact", &self.name, "--format", "pretty"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Native harness arguments that execute this exact case once.
    pub fn run_arguments(&self) -> Vec<String> {
        [
            "--exact",
            &self.name,
            "--format",
            "pretty",
            "--color",
            "never",
            "--test-threads",
            "1",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// Require exactly one listed test and no benchmark or extra output.
    pub fn validate_listing(
        &self,
        stdout: &[u8],
        stderr: &[u8],
        truncated: bool,
        exit_code: Option<i32>,
    ) -> Result<(), String> {
        let text = complete_text(stdout, stderr, truncated)?;
        if exit_code != Some(0) || text != format!("{}: test\n\n1 test, 0 benchmarks\n", self.name)
        {
            return Err(format!(
                "libtest did not list exactly one test named {}",
                self.name
            ));
        }
        Ok(())
    }

    /// Interpret complete native output; invalid grammar or execution stays unavailable.
    pub fn observe(
        &self,
        stdout: &[u8],
        stderr: &[u8],
        truncated: bool,
        exit_code: Option<i32>,
    ) -> RustCaseObservation {
        match complete_text(stdout, stderr, truncated)
            .and_then(|text| self.interpret(text, exit_code))
        {
            Ok(outcome) => RustCaseObservation {
                executions: 1,
                outcome,
            },
            Err(detail) => RustCaseObservation {
                executions: 0,
                outcome: ProbeOutcome::Unavailable {
                    detail: format!("libtest {}: {detail}", self.name),
                },
            },
        }
    }

    fn interpret(&self, text: &str, exit_code: Option<i32>) -> Result<ProbeOutcome, String> {
        let invalid = || "exact one-case result, summary and process exit do not agree".to_string();
        let text = text.trim_matches('\n');
        // Captured test output may contain harness-like text. Ambiguity is not execution proof.
        if text.lines().filter(|l| l.starts_with("running ")).count() != 1
            || text
                .lines()
                .filter(|l| l.starts_with("test ") && l.contains(" ... "))
                .count()
                != 1
            || text
                .lines()
                .filter(|l| l.starts_with("test result:"))
                .count()
                != 1
        {
            return Err(invalid());
        }
        let (case_line, rest) = text
            .strip_prefix("running 1 test\n")
            .and_then(|t| t.split_once('\n'))
            .ok_or_else(invalid)?;
        let (middle, summary) = rest.rsplit_once("\ntest result:").ok_or_else(invalid)?;
        let summary = format!("test result:{summary}");
        let summary = SUMMARY.captures(&summary).ok_or_else(invalid)?;
        summary[4].parse::<u64>().map_err(|_| invalid())?;
        if case_line == format!("test {} ... ok", self.name)
            && middle.trim_matches('\n').is_empty()
            && &summary[1] == "ok"
            && &summary[2] == "1"
            && &summary[3] == "0"
            && exit_code == Some(0)
        {
            return Ok(ProbeOutcome::Passed);
        }
        if case_line != format!("test {} ... FAILED", self.name)
            || &summary[1] != "FAILED"
            || &summary[2] != "0"
            || &summary[3] != "1"
            || exit_code != Some(101)
            || middle.lines().filter(|l| *l == "failures:").count() != 2
        {
            return Err(invalid());
        }
        let body = middle
            .strip_prefix(&format!("\nfailures:\n\n---- {} stdout ----\n", self.name))
            .and_then(|t| t.strip_suffix(&format!("\n\nfailures:\n    {}\n", self.name)))
            .ok_or_else(invalid)?;
        self.signature(body)
            .map(|signature| ProbeOutcome::Failed { signature })
    }

    fn signature(&self, body: &str) -> Result<String, String> {
        let invalid = || "failure has no unique supported panic for the exact case".to_string();
        let mut captures = PANIC.captures_iter(body);
        let panic = captures.next().ok_or_else(invalid)?;
        if captures.next().is_some()
            || panic[1] != self.name
            || body.lines().filter(|l| l.starts_with("thread '")).count() != 1
        {
            return Err(invalid());
        }
        let mut retained = body.to_string();
        // Only the runtime's thread ID is incidental; assertion data and locations remain exact.
        if let Some(id) = panic.get(2) {
            retained.replace_range(id.range(), "<runtime-thread>");
        }
        let mut digest = Sha256::new();
        digest.update(self.name.as_bytes());
        digest.update([0]);
        digest.update(retained.as_bytes());
        Ok(format!("libtest-panic-v1:{:x}", digest.finalize()))
    }
}

fn literal_target(value: &str) -> bool {
    value.starts_with(|c: char| c == '_' || c.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

fn complete_text<'a>(stdout: &'a [u8], stderr: &[u8], truncated: bool) -> Result<&'a str, String> {
    if truncated || !stderr.is_empty() {
        return Err("truncated output or unexpected stderr cannot prove execution".into());
    }
    let text = std::str::from_utf8(stdout).map_err(|e| format!("non-UTF-8 result: {e}"))?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err("control characters in native result".into());
    }
    Ok(text)
}
