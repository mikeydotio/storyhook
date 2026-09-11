//! StoryHook's authoring policy and presentation over the reusable STE checker.

use crate::{domain::StoryEvent, error::AppError, output::Response};
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use ste_lint::{Diagnostic, Format, Options, Severity};

/// One field's diagnostic, with the field kept separate from its prose message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFinding {
    /// The authored field, such as `comment` or `description`.
    pub field: String,
    /// The library's original source location and repair guidance.
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
}

/// A rejected authoring operation; transported without flattening its findings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLintReport {
    /// Additional operation context supplied by an outer layer.
    pub context: Option<String>,
    /// Story whose new text was rejected.
    pub story: String,
    /// Blocking findings and any accompanying grammar advice.
    pub findings: Vec<TextFinding>,
}

impl std::fmt::Display for TextLintReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(context) = &self.context {
            writeln!(f, "{context}\n")?;
        }
        writeln!(
            f,
            "STE text checks failed for {}. No changes were saved.",
            self.story
        )?;
        for finding in &self.findings {
            writeln!(f, "{}", display_finding(finding))?;
        }
        write!(
            f,
            "Repair the text and submit the command again. See `story help ste`."
        )
    }
}

fn findings(field: &str, text: &str) -> Vec<TextFinding> {
    ste_lint::lint(
        text,
        Options {
            format: Format::Markdown,
            sentence_limit: NonZeroUsize::new(20).expect("positive policy limit"),
        },
    )
    .into_iter()
    .map(|diagnostic| TextFinding {
        field: field.into(),
        diagnostic,
    })
    .collect()
}

fn display_finding(finding: &TextFinding) -> String {
    let d = &finding.diagnostic;
    let rule = serde_json::to_value(d.rule).expect("rule serializes");
    format!(
        "{}:{}:{} [{}] {} {}",
        finding.field,
        d.line,
        d.column,
        rule.as_str().expect("rule is a string"),
        d.message,
        d.help
    )
}

pub(crate) fn validate_events(story: &str, events: &[StoryEvent]) -> Result<(), AppError> {
    let mut all = Vec::new();
    for event in events {
        if let StoryEvent::StoryCommentAdded { text, .. } = event {
            all.extend(findings("comment", text));
        }
    }
    if all.iter().any(|f| f.diagnostic.severity == Severity::Error) {
        return Err(AppError::TextLint(TextLintReport {
            context: None,
            story: story.into(),
            findings: all,
        }));
    }
    Ok(())
}

pub(crate) fn with_advice(mut response: Response, fields: &[(&str, &str)]) -> Response {
    let advice: Vec<String> = fields
        .iter()
        .flat_map(|(field, text)| findings(field, text))
        .filter(|f| f.diagnostic.severity == Severity::Advice)
        .map(|f| display_finding(&f))
        .collect();
    if advice.is_empty() {
        return response;
    }
    match &mut response {
        Response::Story(view) => view.warnings.extend(advice),
        Response::Message(message) => {
            return Response::MessageWithWarnings(message.clone(), advice);
        }
        Response::MessageWithWarnings(_, warnings) | Response::Stories { warnings, .. } => {
            warnings.extend(advice)
        }
        _ => {}
    }
    response
}

/// Marks diagnostic output as literal evidence without changing its line content.
#[must_use]
pub fn quote_evidence(text: &str) -> String {
    text.split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
