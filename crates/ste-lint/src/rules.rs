//! Bounded, auditable checks; none claims to parse English grammar completely.

use crate::{Diagnostic, Rule, Severity, prose::Prose};
use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+(?:['’\-][\p{L}\p{N}]+)*").expect("word pattern"));
static UNIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S+").expect("unit pattern"));
static PASSIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:is|are|was|were|be|been|being)\s+(?:[a-z]+ed|written|given|done|seen|known|shown|built|sent|found|made|taken)\b").expect("passive hint pattern")
});

fn diagnostic(
    source: &str,
    prose: &Prose,
    range: Range<usize>,
    rule: Rule,
    severity: Severity,
    message: String,
    help: String,
) -> Diagnostic {
    let span = prose.source_span(range);
    let prefix = &source[..span.start];
    Diagnostic {
        rule,
        severity,
        span,
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        message,
        help,
    }
}

fn technical(unit: &str) -> bool {
    unit.contains('/')
        || unit.contains('\\')
        || unit.contains("::")
        || unit.contains('_')
        || unit.starts_with("www.")
        || (unit.contains('.') && unit.chars().any(|c| c.is_ascii_digit()))
}

fn contraction(word: &str) -> bool {
    word.ends_with("n't")
        || word.ends_with("'re")
        || word.ends_with("'ve")
        || word.ends_with("'ll")
        || word.ends_with("'d")
        || word == "i'm"
        || matches!(
            word,
            "it's"
                | "he's"
                | "she's"
                | "that's"
                | "there's"
                | "here's"
                | "what's"
                | "who's"
                | "where's"
                | "when's"
                | "why's"
                | "how's"
                | "let's"
        )
}

fn vocabulary(word: &str) -> Option<&'static str> {
    // Issue 9, dictionary pages 2-1-U4 and 2-1-C12. No contextual noun rules.
    match word {
        "utilize" => Some("use"),
        "utilizes" => Some("uses"),
        "utilized" => Some("used"),
        "utilizing" => Some("using"),
        "commence" => Some("start"),
        "commences" => Some("starts"),
        "commenced" => Some("started"),
        "commencing" => Some("starting"),
        _ => None,
    }
}

fn ends_sentence(text: &str, end: usize) -> bool {
    let prefix = &text[..end];
    if !prefix.ends_with(['.', '!', '?']) {
        return false;
    }
    let unit = prefix.split_whitespace().next_back().unwrap_or("");
    !matches!(
        unit.to_ascii_lowercase().as_str(),
        "e.g." | "i.e." | "etc." | "mr." | "mrs." | "dr."
    )
}

pub(crate) fn check(source: &str, prose: &Prose, limit: usize, findings: &mut Vec<Diagnostic>) {
    let mut count = 0;
    let mut sentence_start = 0;
    let finish = |start: usize, end: usize, count: usize, findings: &mut Vec<Diagnostic>| {
        if count > limit {
            findings.push(diagnostic(
                source,
                prose,
                start..end,
                Rule::SentenceLength,
                Severity::Error,
                format!("The sentence has {count} words; the selected limit is {limit}."),
                "Divide the sentence. Keep each action or idea clear.".into(),
            ));
        }
    };
    for unit in UNIT.find_iter(&prose.text) {
        if count == 0 {
            sentence_start = unit.start();
        }
        let text = unit.as_str();
        if technical(text) {
            count += 1;
        } else {
            for word in WORD.find_iter(text) {
                count += 1;
                let range = unit.start() + word.start()..unit.start() + word.end();
                let normalized = word.as_str().to_lowercase().replace('’', "'");
                if contraction(&normalized) {
                    findings.push(diagnostic(
                        source,
                        prose,
                        range.clone(),
                        Rule::Contraction,
                        Severity::Error,
                        format!("Expand the contraction {}.", word.as_str()),
                        "Write the words in full (STE rule 4.2).".into(),
                    ));
                }
                if let Some(replacement) = vocabulary(&normalized) {
                    findings.push(diagnostic(
                        source,
                        prose,
                        range,
                        Rule::Vocabulary,
                        Severity::Error,
                        format!(
                            "Use {replacement} instead of {} in general prose.",
                            word.as_str()
                        ),
                        "Use the suggested word. Mark a literal technical term as code.".into(),
                    ));
                }
            }
        }
        let trimmed = text.trim_end_matches(['"', '\'', '’', '”', ')', ']']);
        if ends_sentence(&prose.text, unit.start() + trimmed.len()) {
            finish(sentence_start, unit.end(), count, findings);
            count = 0;
        }
    }
    if count > 0 {
        finish(sentence_start, prose.text.len(), count, findings);
    }
    for found in PASSIVE.find_iter(&prose.text) {
        findings.push(diagnostic(source, prose, found.range(), Rule::PossiblePassive, Severity::Advice,
            "This can be passive voice.".into(), "Review the sentence. Name who or what does the action when this makes the meaning clearer.".into()));
    }
}
