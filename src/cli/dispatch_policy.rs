//! CLI parsing for automatic dispatch policy. Values are validated by the service.
use super::Invocation;
use crate::domain::Complexity;
use crate::error::AppError;
use crate::store::EngineAgent;
use serde::{Deserialize, Serialize};

/// Policy operation shared with the daemon invocation protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyAction {
    /// Read all effective rows.
    Show,
    /// Preview one story's effective selection.
    Resolve {
        /// Target story ID.
        id: String,
        /// Selected provider.
        agent: EngineAgent,
    },
    /// Set or reset supplied fields; null means inherit.
    Patch {
        /// Selected provider.
        agent: EngineAgent,
        /// Selected complexity level.
        complexity: Complexity,
        /// Model and/or effort changes.
        fields: serde_json::Map<String, serde_json::Value>,
    },
}

pub(super) fn parse(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story dispatch-policy show|set|reset|resolve [<story-id>] [--global] [--agent codex|claude] [--complexity low|medium|high] [--model <id>] [--effort <id>]. For reset, --model and --effort take no value. See story help dispatch-policy";
    let fail = || AppError::Usage(usage.into());
    let verb = args.get(1).map(String::as_str).unwrap_or("show");
    if !matches!(verb, "show" | "set" | "reset" | "resolve") {
        return Err(fail());
    }
    let mut global = false;
    let mut agent = None;
    let mut complexity = None;
    let mut id = None;
    let mut fields = serde_json::Map::new();
    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--global" if !global => global = true,
            "--agent" if agent.is_none() => {
                agent = Some(EngineAgent::parse(iter.next().ok_or_else(fail)?).ok_or_else(fail)?);
            }
            "--complexity" if complexity.is_none() => {
                complexity = Some(Complexity::parse(iter.next().ok_or_else(fail)?)?);
            }
            "--model" | "--effort" if matches!(verb, "set" | "reset") => {
                let key = arg.trim_start_matches("--");
                let value = if verb == "reset" {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(
                        iter.next()
                            .filter(|v| !v.starts_with("--"))
                            .ok_or_else(fail)?
                            .clone(),
                    )
                };
                if fields.insert(key.into(), value).is_some() {
                    return Err(fail());
                }
            }
            value if verb == "resolve" && !value.starts_with('-') && id.is_none() => {
                id = Some(value.to_string())
            }
            _ => return Err(fail()),
        }
    }
    let action = match verb {
        "show" if agent.is_none() && complexity.is_none() => PolicyAction::Show,
        "resolve" if !global && complexity.is_none() => PolicyAction::Resolve {
            id: id.ok_or_else(fail)?,
            agent: agent.ok_or_else(fail)?,
        },
        "set" | "reset" => {
            if fields.is_empty() {
                if verb == "set" {
                    return Err(fail());
                }
                fields.insert("model".into(), serde_json::Value::Null);
                fields.insert("effort".into(), serde_json::Value::Null);
            }
            PolicyAction::Patch {
                agent: agent.ok_or_else(fail)?,
                complexity: complexity.ok_or_else(fail)?,
                fields,
            }
        }
        _ => return Err(fail()),
    };
    Ok(Invocation::DispatchPolicy { global, action })
}
