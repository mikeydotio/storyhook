use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::finding::Finding;

/// An integrity fault, with everything its producer knew about it.
///
/// # Why this is not a `String`
///
/// It was one, and every check that filled it already held its answer
/// structured — see [`crate::domain::finding`] for the loss that caused and
/// SH-243 for the 1.68MB regex that paid for it.
///
/// # The rendered report is *derived*, never stored
///
/// [`Display`](std::fmt::Display) joins the findings' own sentences and then
/// the advice, which is exactly what `service::integrity::detail_with_notices`
/// used to build by hand. Nothing holds a second copy of that string, so the
/// structured and prose forms cannot disagree — the same reason
/// [`WireError`] declines to transport a message it can recompute.
///
/// # Three fields, because they answer three different questions
///
/// * `context` is what [`AppError::with_context`] prepends. It is its own
///   field rather than being folded into the body, because folding it in would
///   make the derivation above a lie the first time a layer annotated an
///   integrity error — and that is reachable: `daemon::lifecycle` wraps any
///   `AppError` it decodes from a failed startup.
/// * `findings` is damage. Non-empty for every value this type can hold; see
///   [`IntegrityDetail::report`] and [`From<String>`].
/// * `advice` never decides anything. SH-185's council put an event kind this
///   build has never heard of here specifically so it could not affect a
///   health verdict or an exit code, and the separation is by *type* rather
///   than by a flag on a shared list: something that is not a [`Finding`]
///   cannot be counted as one by a producer that sets a field wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityDetail {
    /// What a layer that met this error added on the way past.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// What is wrong. Never empty.
    pub findings: Vec<Finding>,
    /// What is worth saying and is not damage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advice: Vec<String>,
}

impl IntegrityDetail {
    /// A doctor report, or `None` when there is nothing wrong.
    ///
    /// **The emptiness question is asked here and nowhere else.** It used to be
    /// asked at each raise site, which is two hand-maintained copies of one
    /// invariant and a third owed by anything added later; a caller now
    /// branches on this constructor's answer instead of deciding for itself
    /// whether it has an error to raise.
    ///
    /// `advice` is carried whether or not it is empty, because advice alone is
    /// not a fault — a project whose only anomaly is a notice is healthy, and
    /// this returns `None` for it.
    #[must_use]
    pub fn report(findings: Vec<Finding>, advice: Vec<String>) -> Option<Self> {
        (!findings.is_empty()).then_some(Self {
            context: None,
            findings,
            advice,
        })
    }

    /// The sections of the rendered report, in the order they print.
    fn sections(&self) -> Vec<String> {
        let findings = self
            .findings
            .iter()
            .map(|finding| finding.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        [findings, self.advice.join("\n")]
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect()
    }
}

/// Prose from a caller with no structure to offer, as a whole detail.
///
/// **Total on purpose.** Minting a [`FindingCode::Unstructured`](crate::domain::finding::FindingCode::Unstructured) finding rather
/// than an empty list is what makes "an integrity error carrying no findings"
/// unrepresentable at *every* raise site — a story missing a field mid-fold, a
/// refused migration, a store invariant — and not merely at the two that
/// happen to check. The rendered message is unchanged at all of them.
impl From<String> for IntegrityDetail {
    fn from(message: String) -> Self {
        Self {
            context: None,
            findings: vec![Finding::unstructured(message)],
            advice: Vec::new(),
        }
    }
}

impl From<&str> for IntegrityDetail {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl std::fmt::Display for IntegrityDetail {
    /// The report a human reads, and byte-for-byte what this error rendered
    /// before it carried structure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let body = self.sections().join("\n");
        match &self.context {
            // The same shape `with_context` gives every other variant: the
            // context, a blank line, then the original message verbatim.
            Some(context) => write!(f, "{context}\n\n{body}"),
            None => write!(f, "{body}"),
        }
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    LockTimeout(String),
    /// The calling process stopped waiting because its `--deadline` elapsed.
    ///
    /// Distinct from [`Self::Storage`] because the command may still complete
    /// in the daemon. Automation can stop repeated attempts on this condition
    /// without treating an ordinary store failure as evidence that every
    /// subsequent command will also time out.
    #[error("{0}")]
    DeadlineExceeded(String),
    #[error("{0}")]
    Integrity(IntegrityDetail),
    #[error("{0}")]
    Storage(String),
    #[error("github auth: {0}")]
    GithubAuth(String),
    #[error("github api: {0}")]
    GithubApi(String),
    #[error("state conflict: expected `{0}`, was `{1}`")]
    StateConflict(String, String), // (expected, actual)
    /// A write refused because the store is open read-only (SH-530).
    ///
    /// Its own variant, and its own exit code, rather than folding into
    /// [`Self::Storage`]: "this store is readable but not writable by this
    /// build" is a condition a caller can act on — retry after upgrading —
    /// where a generic storage failure is not. A script that reads stories
    /// from a degraded store must be able to tell that apart from a broken
    /// one without parsing prose.
    #[error("{0}")]
    ReadOnlyStore(String),
}

impl AppError {
    /// Exit codes 8 and 10 are permanently retired, not reused: they named
    /// `SyncConflict`/`SyncErrors`, constructed only by the story↔GitHub-Issues
    /// sync engine SH-408 retired. Reassigning either number to a new
    /// variant would make any script or documentation still keyed on the old
    /// meaning silently wrong instead of simply unreachable.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Validation(_) => 2,
            Self::NotFound(_) => 3,
            Self::LockTimeout(_) => 4,
            Self::Integrity(_) | Self::Storage(_) => 5,
            Self::GithubAuth(_) => 6,
            Self::GithubApi(_) => 7,
            Self::StateConflict(..) => 9,
            // 11, not 8: 8 and 10 are retired above, and 9 is taken.
            Self::ReadOnlyStore(_) => 11,
            Self::DeadlineExceeded(_) => 12,
        }
    }

    /// The same error, told where it was met.
    ///
    /// A layer that catches an error and re-raises a summary of it has thrown
    /// away the only sentence that named the actual problem. This adds without
    /// removing: the variant, and therefore the exit code, is unchanged, and
    /// the original message survives verbatim below the context.
    ///
    /// `StateConflict` is returned untouched. Its payload is two slugs a caller
    /// compares programmatically, not prose, and prepending to either would
    /// corrupt a value rather than annotate a message.
    ///
    /// `Integrity` is annotated on its own `context` field, for the same
    /// reason at a larger scale: its findings are values a caller reads, and
    /// prepending to the first one's `message` would corrupt a finding while
    /// appearing to annotate a report. The *rendered* result is identical
    /// either way — [`IntegrityDetail`]'s `Display` puts the context in
    /// exactly the position this method's `joined` would have.
    ///
    /// The context joins the variant's *detail*, so for a variant whose
    /// `Display` carries a prefix — `github auth: {0}` — the prefix stays
    /// outermost. That is the right order: it names the subsystem, and the
    /// context names the operation within it.
    #[must_use]
    pub fn with_context(self, context: &str) -> Self {
        let joined = |detail: String| format!("{context}\n\n{detail}");
        match self {
            Self::Usage(detail) => Self::Usage(joined(detail)),
            Self::Validation(detail) => Self::Validation(joined(detail)),
            Self::NotFound(detail) => Self::NotFound(joined(detail)),
            Self::LockTimeout(detail) => Self::LockTimeout(joined(detail)),
            Self::DeadlineExceeded(detail) => Self::DeadlineExceeded(joined(detail)),
            Self::Integrity(mut detail) => {
                // Annotated, never rewritten: an outer layer's sentence is
                // prepended to the report, and every finding inside it is
                // returned exactly as its producer wrote it. A second context
                // replaces the first the way a second `joined` would nest —
                // both are one annotation ahead of the body.
                detail.context = Some(match detail.context {
                    Some(inner) => format!("{context}\n\n{inner}"),
                    None => context.to_string(),
                });
                Self::Integrity(detail)
            }
            Self::Storage(detail) => Self::Storage(joined(detail)),
            Self::ReadOnlyStore(detail) => Self::ReadOnlyStore(joined(detail)),
            Self::GithubAuth(detail) => Self::GithubAuth(joined(detail)),
            Self::GithubApi(detail) => Self::GithubApi(joined(detail)),
            conflict @ Self::StateConflict(..) => conflict,
        }
    }
}

/// The wire form of [`AppError`] — a 1:1 mirror that carries the *variant*
/// and its structured payload, not a flattened string.
///
/// Why a mirror instead of `#[derive(Serialize, Deserialize)]` on `AppError`
/// itself: `AppError` is a `thiserror` type whose `Display` impls are part of
/// the CLI's user-facing contract, and whose `From` conversions pull in
/// foreign error types (`std::io`, `rusqlite`, `serde_json`) that are not and
/// should not become serializable. Deriving on it would either freeze that
/// enum's shape as a public data format or force `#[serde(skip)]` holes into
/// the error path. The mirror keeps the two concerns apart and makes the
/// conversion a place where the compiler can enforce completeness.
///
/// What survives the hop, and how it is guaranteed:
/// - **variant** — [`From<&AppError>`] is an exhaustive `match`, so a further
///   `AppError` variant stops this file compiling until it is given
///   a wire form.
/// - **fields** — each variant carries its own payload by name, so
///   `StateConflict { expected, actual }` arrives as two strings rather than
///   as prose a receiver would have to parse back apart.
/// - **message and exit code** — deliberately *not* transported. They are
///   recomputed by [`AppError::to_string`] and [`AppError::exit_code`] after
///   the round trip, which makes disagreement between a transported copy and
///   the real value structurally impossible. `tests/wire_envelope.rs` proves
///   the recomputation is exact for every variant.
///
/// The `detail` field is the variant's payload, never its rendered message:
/// `GithubAuth`'s `Display` is `github auth: {0}`, so reconstructing it from
/// the message would double the prefix on every hop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireError {
    Usage {
        detail: String,
    },
    Validation {
        detail: String,
    },
    NotFound {
        detail: String,
    },
    LockTimeout {
        detail: String,
    },
    DeadlineExceeded {
        detail: String,
    },
    /// The one variant whose payload is a document rather than a sentence.
    ///
    /// Carried field-by-field like every other, which is the whole point of the
    /// mirror: a receiver reads `findings` as data instead of parsing the
    /// rendered report back apart. The rendered report is not transported at
    /// all — [`IntegrityDetail`] recomputes it — so a transported copy cannot
    /// come to disagree with the findings it was rendered from.
    Integrity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        findings: Vec<Finding>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        advice: Vec<String>,
    },
    Storage {
        detail: String,
    },
    GithubAuth {
        detail: String,
    },
    GithubApi {
        detail: String,
    },
    StateConflict {
        expected: String,
        actual: String,
    },
    ReadOnlyStore {
        detail: String,
    },
}

impl From<&AppError> for WireError {
    fn from(error: &AppError) -> Self {
        match error {
            AppError::Usage(detail) => Self::Usage {
                detail: detail.clone(),
            },
            AppError::Validation(detail) => Self::Validation {
                detail: detail.clone(),
            },
            AppError::NotFound(detail) => Self::NotFound {
                detail: detail.clone(),
            },
            AppError::LockTimeout(detail) => Self::LockTimeout {
                detail: detail.clone(),
            },
            AppError::DeadlineExceeded(detail) => Self::DeadlineExceeded {
                detail: detail.clone(),
            },
            AppError::Integrity(detail) => Self::Integrity {
                context: detail.context.clone(),
                findings: detail.findings.clone(),
                advice: detail.advice.clone(),
            },
            AppError::Storage(detail) => Self::Storage {
                detail: detail.clone(),
            },
            AppError::ReadOnlyStore(detail) => Self::ReadOnlyStore {
                detail: detail.clone(),
            },
            AppError::GithubAuth(detail) => Self::GithubAuth {
                detail: detail.clone(),
            },
            AppError::GithubApi(detail) => Self::GithubApi {
                detail: detail.clone(),
            },
            AppError::StateConflict(expected, actual) => Self::StateConflict {
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}

impl From<AppError> for WireError {
    fn from(error: AppError) -> Self {
        Self::from(&error)
    }
}

impl From<WireError> for AppError {
    fn from(wire: WireError) -> Self {
        match wire {
            WireError::Usage { detail } => Self::Usage(detail),
            WireError::Validation { detail } => Self::Validation(detail),
            WireError::NotFound { detail } => Self::NotFound(detail),
            WireError::LockTimeout { detail } => Self::LockTimeout(detail),
            WireError::DeadlineExceeded { detail } => Self::DeadlineExceeded(detail),
            WireError::Integrity {
                context,
                findings,
                advice,
            } => Self::Integrity(IntegrityDetail {
                context,
                findings,
                advice,
            }),
            WireError::Storage { detail } => Self::Storage(detail),
            WireError::GithubAuth { detail } => Self::GithubAuth(detail),
            WireError::GithubApi { detail } => Self::GithubApi(detail),
            WireError::StateConflict { expected, actual } => Self::StateConflict(expected, actual),
            WireError::ReadOnlyStore { detail } => Self::ReadOnlyStore(detail),
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(value: std::io::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(value: serde_json::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<toml::de::Error> for AppError {
    fn from(value: toml::de::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<toml::ser::Error> for AppError {
    fn from(value: toml::ser::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

impl From<serde_yml::Error> for AppError {
    fn from(value: serde_yml::Error) -> Self {
        Self::Storage(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Context is added; nothing is taken away. The original message has to
    /// survive verbatim, because it is the only part that names the actual
    /// problem — a layer that summarised it would be the defect this method
    /// exists to prevent.
    #[test]
    fn context_is_added_without_losing_the_original_message() {
        let error = AppError::Storage("the store is damaged".to_string())
            .with_context("the daemon could not start");
        let rendered = error.to_string();

        assert!(rendered.contains("the daemon could not start"));
        assert!(rendered.contains("the store is damaged"));
    }

    /// The variant survives, and therefore so does the exit code. A caller that
    /// branches on `NotFound` must keep branching on it after a layer has
    /// annotated the message.
    #[test]
    fn every_variant_keeps_its_own_exit_code_through_a_context() {
        for error in [
            AppError::Usage("u".into()),
            AppError::Validation("v".into()),
            AppError::NotFound("n".into()),
            AppError::LockTimeout("l".into()),
            AppError::DeadlineExceeded("d".into()),
            AppError::Integrity("i".into()),
            AppError::Storage("s".into()),
            AppError::GithubAuth("ga".into()),
            AppError::GithubApi("gp".into()),
            AppError::StateConflict("todo".into(), "done".into()),
        ] {
            let before = error.exit_code();
            let after = error.with_context("while doing the thing");
            assert_eq!(
                before,
                after.exit_code(),
                "annotating an error must not change what a script sees: {after}"
            );
        }
    }

    /// `StateConflict`'s payload is two slugs a caller compares, not prose.
    /// Prepending to either would corrupt a value while appearing to annotate a
    /// message — and `story move --if-state` reads them back.
    #[test]
    fn a_state_conflicts_two_slugs_are_left_alone() {
        let error =
            AppError::StateConflict("todo".into(), "done".into()).with_context("some context");
        match error {
            AppError::StateConflict(expected, actual) => {
                assert_eq!(expected, "todo");
                assert_eq!(actual, "done");
            }
            other => panic!("the variant must survive: {other}"),
        }
    }

    /// An integrity error that carries no findings is unrepresentable.
    ///
    /// The emptiness question is asked by this constructor and nowhere else,
    /// so "healthy" is its `None` rather than a check each raise site repeats
    /// (SH-244).
    #[test]
    fn an_integrity_report_with_nothing_wrong_is_not_an_error() {
        assert!(IntegrityDetail::report(Vec::new(), Vec::new()).is_none());
        assert!(
            IntegrityDetail::report(Vec::new(), vec!["a notice".to_string()]).is_none(),
            "advice alone is not damage — SH-185's whole ruling"
        );
        assert!(
            IntegrityDetail::report(vec![Finding::unstructured("wrong")], Vec::new()).is_some()
        );
    }

    /// Prose from a caller with nothing structured to say still arrives as a
    /// finding, so *every* raise site upholds the invariant above — not only
    /// the two that construct a report.
    #[test]
    fn a_plain_string_still_becomes_a_finding() {
        let detail = IntegrityDetail::from("story SH-1 is missing state".to_string());
        assert_eq!(detail.findings.len(), 1);
        assert_eq!(
            detail.findings[0].code,
            crate::domain::finding::FindingCode::Unstructured
        );
        assert_eq!(
            detail.to_string(),
            "story SH-1 is missing state",
            "the rendered message must be exactly what the caller passed"
        );
    }

    /// The rendered report is the findings' own sentences, joined — not a
    /// second rendering that could disagree with them.
    #[test]
    fn the_rendered_report_is_its_findings_joined() {
        let detail = IntegrityDetail::report(
            vec![
                Finding::unstructured("first"),
                Finding::unstructured("second"),
            ],
            vec!["an advisory".to_string()],
        )
        .expect("two findings is a report");
        assert_eq!(detail.to_string(), "first\nsecond\nan advisory");
    }

    /// A section with no occupants leaves no blank line — the behaviour
    /// `service::integrity::join_sections` used to provide by hand.
    #[test]
    fn an_empty_advice_section_is_skipped_rather_than_printed() {
        let detail = IntegrityDetail::report(vec![Finding::unstructured("only")], Vec::new())
            .expect("a finding is a report");
        assert_eq!(detail.to_string(), "only");
    }

    /// Context is annotated onto its own field, and every finding survives
    /// untouched — the same exemption `StateConflict`'s two slugs get, for the
    /// same reason: these are values a caller reads, not prose.
    #[test]
    fn an_integrity_context_annotates_without_touching_the_findings() {
        let error = AppError::Integrity(
            IntegrityDetail::report(vec![Finding::unstructured("the row disagrees")], Vec::new())
                .expect("a report"),
        )
        .with_context("while checking the project");

        match error {
            AppError::Integrity(detail) => {
                assert_eq!(
                    detail.findings,
                    vec![Finding::unstructured("the row disagrees")],
                    "a finding is data; annotating a report must not rewrite one"
                );
                assert_eq!(
                    detail.to_string(),
                    "while checking the project\n\nthe row disagrees",
                    "the rendering must match what `joined` produces for every other variant"
                );
            }
            other => panic!("the variant must survive: {other}"),
        }
    }

    /// A variant whose `Display` carries a prefix keeps it outermost: the
    /// prefix names the subsystem, the context names the operation inside it.
    #[test]
    fn a_prefixed_variant_keeps_its_prefix_in_front() {
        let rendered = AppError::GithubAuth("no token".into())
            .with_context("while syncing")
            .to_string();
        assert!(
            rendered.starts_with("github auth: while syncing"),
            "got: {rendered}"
        );
    }
}
