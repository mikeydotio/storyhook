//! Reusable, deterministic writing checks. Passing is not proof of STE compliance.

use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::ops::Range;

mod prose;
mod rules;

/// How the input marks prose and literal technical material.
#[derive(Clone, Copy, Debug)]
pub enum Format {
    /// All input is prose; titles need no final punctuation.
    Plain,
    /// CommonMark with tables; code and quoted evidence are preserved.
    Markdown,
}

/// The caller's writing policy; sentence limits differ by document type.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The source format.
    pub format: Format,
    /// Maximum words in a sentence, selected by the caller.
    pub sentence_limit: NonZeroUsize,
}

/// Whether a finding establishes a violation or asks for author review.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A deterministic violation of the selected checks.
    Error,
    /// A possible grammar issue; never proof of a violation.
    Advice,
}

/// Stable diagnostic identifiers, independent of display wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// The caller's limit, not a universal STE limit.
    SentenceLength,
    /// STE rule 4.2: expand contractions.
    Contraction,
    /// A term in the supported subset of the Issue 9 dictionary.
    Vocabulary,
    /// A possible passive construction that needs author review.
    PossiblePassive,
}

/// One finding, with a location in the original input and a repair instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable rule identifier.
    pub rule: Rule,
    /// Blocking or advisory classification.
    pub severity: Severity,
    /// Half-open UTF-8 byte range in the original source.
    pub span: Range<usize>,
    /// One-based source line.
    pub line: usize,
    /// One-based Unicode scalar column.
    pub column: usize,
    /// What the check found.
    pub message: String,
    /// What the author can do next.
    pub help: String,
}

/// Checks prose without rewriting it or performing I/O.
#[must_use]
pub fn lint(text: &str, options: Options) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for prose in prose::parse(text, options.format) {
        rules::check(text, &prose, options.sentence_limit.get(), &mut findings);
    }
    findings.sort_by_key(|finding| (finding.span.start, finding.span.end));
    findings
}
