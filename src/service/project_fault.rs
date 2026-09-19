//! Structured project gate evidence. Human diagnostics never select recovery policy.

use serde::{Deserialize, Serialize};

/// A receipt inspection that completed successfully but did not certify the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiptRefusal {
    /// No receipt exists for the pinned tree.
    Missing,
    /// A receipt exists, but its tier cannot certify a merge.
    InsufficientTier,
}

/// A proven project fault, separate from test failure and verifier failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProjectFault {
    /// The committed project pointer cannot select a valid plain-argv gate.
    InvalidGateConfiguration {
        /// Configuration locus that identifies this fault.
        locus: String,
        /// Pinned proposed merge tree.
        tree: String,
        /// Pinned base commit.
        base: String,
        /// Pinned submitted commit.
        head: String,
        /// Tree of the submitted commit, independent of the moving merge base.
        head_tree: String,
        /// Digest of the exact committed configuration bytes.
        configuration: String,
        /// Parser diagnosis; never used for classification.
        detail: String,
    },
    /// The configured repository-local gate entry point is absent or not executable.
    MissingGateCommand {
        /// Repository-relative command path identifying the fault.
        locus: String,
        /// Pinned proposed merge tree.
        tree: String,
        /// Pinned base commit.
        base: String,
        /// Pinned submitted commit.
        head: String,
        /// Tree of the submitted commit, used to reject empty-commit repair retries.
        head_tree: String,
        /// Digest of the committed configuration bytes.
        configuration: String,
        /// Parsed plain-argv gate command.
        gate: String,
        /// Whether the path is absent or not executable.
        reason: GateEntryRefusal,
        /// Diagnostic naming the configured path and pinned tree.
        detail: String,
    },
    /// The gate completed successfully and restoration settled, but certification is absent.
    MissingCertification {
        /// Repository-relative configuration locus used for fault deduplication.
        locus: String,
        /// Exact proposed merge tree; this observation does not certify it.
        tree: String,
        /// Pinned base commit used to construct the merge.
        base: String,
        /// Pinned submitted commit used to construct the merge.
        head: String,
        /// Committed source tree of the submitted head before merge construction.
        head_tree: String,
        /// Actual plain-argv command executed for this observation.
        gate: String,
        /// Full attempt log, retained independently of the bounded diagnosis.
        log: String,
        /// Attempt-bound execution evidence produced before restoration.
        execution: String,
        /// Completed command exit status; only zero admits this variant.
        execution_status: u8,
        /// Machine-readable reason the exact tree lacks certification.
        receipt: ReceiptRefusal,
        /// Human diagnosis, never a classification input.
        detail: String,
    },
}

impl ProjectFault {
    /// Committed source identity retained independently of the proposed merge tree.
    pub fn source(&self) -> (&str, &str) {
        match self {
            Self::InvalidGateConfiguration {
                head, head_tree, ..
            }
            | Self::MissingGateCommand {
                head, head_tree, ..
            }
            | Self::MissingCertification {
                head, head_tree, ..
            } => (head, head_tree),
        }
    }
    /// Stable fault identity; changing trees and commands remain observations.
    pub fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::InvalidGateConfiguration { locus, .. } => ("invalid-gate-configuration", locus),
            Self::MissingGateCommand { locus, .. } => ("missing-gate-command", locus),
            Self::MissingCertification { locus, .. } => ("missing-certification", locus),
        }
    }

    /// Refuse malformed protocol evidence before any recovery effect is admitted.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::InvalidGateConfiguration {
            locus,
            tree,
            base,
            head,
            head_tree,
            configuration,
            detail,
        }
        | Self::MissingGateCommand {
            locus,
            tree,
            base,
            head,
            head_tree,
            configuration,
            detail,
            ..
        } = self
            && (![tree, base, head, head_tree]
                .iter()
                .all(|oid| is_pinned_oid(oid))
                || configuration.len() != 64
                || !is_pinned_oid(configuration)
                || detail.trim().is_empty()
                || locus.is_empty()
                || std::path::Path::new(locus)
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_))))
        {
            return Err("invalid pinned project configuration fault evidence".into());
        }
        match self {
            Self::InvalidGateConfiguration { locus, .. } => {
                if locus != ".storyhook.toml#verify.gate" {
                    return Err("invalid project configuration locus".into());
                }
                Ok(())
            }
            Self::MissingGateCommand { gate, .. } => {
                super::gate_command::GateCommand::parse(gate)?;
                Ok(())
            }
            Self::MissingCertification {
                locus,
                tree,
                base,
                head,
                head_tree,
                gate,
                log,
                execution,
                execution_status,
                detail,
                ..
            } => {
                for (name, oid) in [
                    ("tree", tree),
                    ("base", base),
                    ("head", head),
                    ("head_tree", head_tree),
                ] {
                    if !is_pinned_oid(oid) {
                        return Err(format!(
                            "project fault {name} must be a pinned Git object id"
                        ));
                    }
                }
                if locus != ".storyhook.toml#verify.gate" || *execution_status != 0 {
                    return Err("missing certification requires the project gate and a successful execution".into());
                }
                for (name, value) in [("log", log), ("execution", execution), ("detail", detail)] {
                    if value.trim().is_empty() {
                        return Err(format!("project fault requires nonempty {name}"));
                    }
                }
                super::gate_command::GateCommand::parse(gate)?;
                Ok(())
            }
        }
    }

    /// The retained human diagnosis associated with this structured observation.
    pub fn detail(&self) -> &str {
        match self {
            Self::MissingCertification { detail, .. }
            | Self::InvalidGateConfiguration { detail, .. }
            | Self::MissingGateCommand { detail, .. } => detail,
        }
    }
}

/// A repository-local executable cannot be launched from the proposed tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateEntryRefusal {
    /// No tracked entry names the configured executable.
    Missing,
    /// The tracked entry is not an executable file.
    NotExecutable,
}

/// Whether a protocol value names a pinned SHA-1 or SHA-256 Git object.
pub(crate) fn is_pinned_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn evidence() -> Value {
        json!({"code":"missing-certification", "locus":".storyhook.toml#verify.gate",
            "tree":"a".repeat(40), "base":"b".repeat(40), "head":"c".repeat(40),
            "head_tree":"d".repeat(40),
            "gate":"make test", "log":"/logs/attempt", "execution":"/executions/attempt.json",
            "execution_status":0, "receipt":"missing", "detail":"gate did not certify"})
    }

    #[test]
    fn missing_certification_requires_completed_exact_tree_evidence() {
        let valid: ProjectFault = serde_json::from_value(evidence()).unwrap();
        valid.validate().unwrap();
        for (key, value) in [
            ("tree", json!("HEAD")),
            ("base", json!("")),
            ("head", json!("g".repeat(40))),
            ("head_tree", json!("HEAD^{tree}")),
            ("execution_status", json!(1)),
            ("log", json!("")),
            ("execution", json!("")),
            ("gate", json!("make test && true")),
            ("locus", json!("/some/other/project")),
            ("detail", json!(" ")),
        ] {
            let mut bad = evidence();
            bad[key] = value;
            let bad: ProjectFault = serde_json::from_value(bad).unwrap();
            assert!(bad.validate().is_err(), "accepted invalid {key}");
        }
    }

    #[test]
    fn unknown_faults_and_receipt_failures_do_not_admit_project_repair() {
        for (key, value) in [("code", "infrastructure"), ("receipt", "reader-failure")] {
            let mut bad = evidence();
            bad[key] = value.into();
            assert!(serde_json::from_value::<ProjectFault>(bad).is_err());
        }
        for key in [
            "tree",
            "base",
            "head",
            "head_tree",
            "execution",
            "log",
            "receipt",
            "execution_status",
        ] {
            let mut bad = evidence();
            bad.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<ProjectFault>(bad).is_err(),
                "missing {key}"
            );
        }
    }
}
