//! Legacy STE diagnostic transport and literal evidence formatting.
//!
//! Public diagnostic types remain available for wire and library compatibility.
//! StoryHook does not validate authored text or produce grammar advice.

use serde::{Deserialize, Serialize};
use ste_lint::Diagnostic;

/// One field's diagnostic, with the field kept separate from its prose message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFinding {
    /// The authored field, such as `comment` or `description`.
    pub field: String,
    /// The library's original source location and repair guidance.
    #[serde(flatten)]
    pub diagnostic: Diagnostic,
}

/// A legacy rejected authoring operation, retained for transport compatibility.
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

/// Marks diagnostic output as literal evidence without changing its line content.
#[must_use]
pub fn quote_evidence(text: &str) -> String {
    text.split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
