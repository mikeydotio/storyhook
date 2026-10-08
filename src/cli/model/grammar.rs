//! Structured syntax from the literals registered beside executable handlers.
//! This is descriptive grammar: legacy parsers retain placement, duplicate,
//! normalization and service-validation rules. It never fetches dynamic values.
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Expression {
    Sequence(Vec<Expression>),
    Choice(Vec<Expression>),
    Optional(Box<Expression>),
    Repeated(Box<Expression>),
    Literal(String),
    Operand {
        name: String,
        domain: Option<Domain>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Domain {
    Static { name: String, values: Vec<String> },
    Dynamic { name: String, source: &'static str },
}

pub fn domain(name: &str) -> Result<Domain, String> {
    use crate::domain::{Complexity, Priority, SuperState};
    let values: Vec<&str> = match name {
        "priority" => [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
            Priority::None,
        ]
        .iter()
        .map(Priority::as_str)
        .collect(),
        "complexity" => Complexity::ALL.iter().map(|value| value.as_str()).collect(),
        "superstate" => [SuperState::Open, SuperState::Closed]
            .iter()
            .map(SuperState::as_str)
            .collect(),
        "audiences" => vec!["task", "operator", "internal", "all"],
        "providers" => vec!["codex", "claude"],
        "speed" => vec!["standard", "fast"],
        "context-format" => vec!["markdown", "json"],
        "active-role" => vec!["active"],
        "state-role" => vec!["active", "none"],
        "relationships" => vec![
            "relates-to",
            "related-to",
            "blocks",
            "blocked-by",
            "parent-of",
            "child-of",
            "duplicate-of",
            "obviates",
            "obviated-by",
        ],
        other => {
            let source = match other {
                "states" => "story state list",
                "types" => "story type list",
                "stories" => "story list --all",
                "labels" => "labels on project stories; new label names may also be supplied",
                "phases" => "story phase list; creation accepts a new phase number",
                "help-topics" => "story help --all",
                "project-settings" => "story project settings list",
                "provider-models" => "selected provider model catalog and dispatch policy",
                "provider-efforts" => "selected provider/model supported effort settings",
                "hook-events" => "installed hook event registry",
                _ => return Err(format!("unregistered grammar domain: {name}")),
            };
            return Ok(Domain::Dynamic {
                name: name.into(),
                source,
            });
        }
    };
    Ok(Domain::Static {
        name: name.into(),
        values: values.into_iter().map(str::to_string).collect(),
    })
}

/// Parse the small registered syntax notation into an explicit tree. Repetition
/// applies to its preceding node; repeating an optional group means zero or more.
pub fn expression(syntax: &str) -> Result<Expression, String> {
    let mut tokens = Vec::new();
    let mut tail = syntax;
    while !tail.is_empty() {
        tail = tail.trim_start();
        if tail.is_empty() {
            break;
        }
        let length = if tail.starts_with("...") {
            3
        } else if tail.starts_with('<') {
            tail.find('>').ok_or("unterminated operand")? + 1
        } else if tail.starts_with(['[', ']', '(', ')', '|']) {
            1
        } else {
            tail.find(|c: char| c.is_whitespace() || "[]()|".contains(c))
                .unwrap_or(tail.len())
        };
        tokens.push(&tail[..length]);
        tail = &tail[length..];
    }
    fn choice(tokens: &[&str], at: &mut usize) -> Result<Expression, String> {
        let mut alternatives = Vec::new();
        loop {
            let mut sequence = Vec::new();
            while let Some(token) = tokens.get(*at) {
                if matches!(*token, "]" | ")" | "|") {
                    break;
                }
                *at += 1;
                let mut item = match *token {
                    "[" | "(" => {
                        let child = choice(tokens, at)?;
                        let closing = if *token == "[" { "]" } else { ")" };
                        if tokens.get(*at) != Some(&closing) {
                            return Err(format!("missing {closing}"));
                        }
                        *at += 1;
                        if *token == "[" {
                            Expression::Optional(Box::new(child))
                        } else {
                            child
                        }
                    }
                    "..." => return Err("repetition has no preceding expression".into()),
                    value if value.starts_with('<') => {
                        let raw = &value[1..value.len() - 1];
                        let (name, source) = raw
                            .split_once(':')
                            .map_or((raw, None), |(name, source)| (name, Some(source)));
                        Expression::Operand {
                            name: name.into(),
                            domain: source.map(domain).transpose()?,
                        }
                    }
                    word => Expression::Literal(word.into()),
                };
                if tokens.get(*at) == Some(&"...") {
                    *at += 1;
                    item = Expression::Repeated(Box::new(item));
                }
                sequence.push(item);
            }
            alternatives.push(Expression::Sequence(sequence));
            if tokens.get(*at) != Some(&"|") {
                break;
            }
            *at += 1;
        }
        Ok(if alternatives.len() == 1 {
            alternatives.remove(0)
        } else {
            Expression::Choice(alternatives)
        })
    }
    let mut at = 0;
    let result = choice(&tokens, &mut at)?;
    if at != tokens.len() {
        return Err(format!("unexpected syntax token {}", tokens[at]));
    }
    Ok(result)
}
