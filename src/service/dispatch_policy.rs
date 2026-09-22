//! One resolution rule for CLI, web, and provider launch helpers.
use crate::domain::Complexity;
use crate::error::AppError;
use crate::store::{DispatchPolicyOverride, EngineAgent, ProjectId, ReadOps, Store, WriteOps};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Effective policy with the source of each field and the saved local override.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyEntry {
    /// Provider to launch.
    pub agent: EngineAgent,
    /// Complexity that selects this row.
    pub complexity: Complexity,
    /// Effective model identifier.
    pub model: String,
    /// Effective reasoning effort.
    pub effort: String,
    /// `project`, `installation`, or `builtin`.
    pub model_source: String,
    /// `project`, `installation`, or `builtin`.
    pub effort_source: String,
    /// Overrides saved at the requested scope.
    pub overrides: DispatchPolicyOverride,
}

/// A policy row resolved for a particular executable story.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    /// Canonical story identifier.
    pub story_id: String,
    /// Whether the story's complexity was explicitly assessed.
    pub complexity_assessed: bool,
    /// Effective selection and its origins.
    #[serde(flatten)]
    pub selection: PolicyEntry,
}

/// The settings document returned by CLI and web API.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyView {
    /// `installation` or `project`.
    pub scope: String,
    /// All six provider/complexity combinations.
    pub entries: Vec<PolicyEntry>,
    /// Present when the caller requested a story preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedPolicy>,
}

/// Validates a model for automatic selection. Explicit dispatch remains separate.
fn validate_model(agent: EngineAgent, model: &str) -> Result<(), AppError> {
    let allowed = match agent {
        EngineAgent::Codex => &["gpt-6-astra", "gpt-5.6-sol"][..],
        EngineAgent::Claude => &["opus", "fable"][..],
    };
    if allowed.contains(&model) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "invalid automatic model `{model}` for {}; use {}",
        agent.as_str(),
        allowed.join(", ")
    )))
}

/// Validates an automatic effort against the provider's supported vocabulary.
fn validate_effort(agent: EngineAgent, effort: &str) -> Result<(), AppError> {
    let allowed = match agent {
        EngineAgent::Codex => &["none", "low", "medium", "high", "xhigh", "max", "ultra"][..],
        EngineAgent::Claude => &["low", "medium", "high", "xhigh", "max"][..],
    };
    if allowed.contains(&effort) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "invalid effort `{effort}` for {}; use {}",
        agent.as_str(),
        allowed.join(", ")
    )))
}

fn entry(
    tx: &impl ReadOps,
    project: Option<ProjectId>,
    agent: EngineAgent,
    complexity: Complexity,
) -> Result<PolicyEntry, AppError> {
    let global = tx.dispatch_policy(None, agent, complexity)?;
    let local = match project {
        Some(id) => tx.dispatch_policy(Some(id), agent, complexity)?,
        None => DispatchPolicyOverride::default(),
    };
    let builtin_model = match agent {
        EngineAgent::Codex => "gpt-6-astra",
        EngineAgent::Claude => "fable",
    };
    let builtin_effort = match complexity {
        Complexity::Low => "medium",
        Complexity::Medium => "high",
        Complexity::High => "xhigh",
    };
    let choose = |local: &Option<String>, global: &Option<String>, fallback: &str| {
        if let Some(value) = local {
            (value.clone(), "project".to_string())
        } else if let Some(value) = global {
            (value.clone(), "installation".to_string())
        } else {
            (fallback.to_string(), "builtin".to_string())
        }
    };
    let (model, model_source) = choose(&local.model, &global.model, builtin_model);
    let (effort, effort_source) = choose(&local.effort, &global.effort, builtin_effort);
    validate_model(agent, &model)?;
    validate_effort(agent, &effort)?;
    Ok(PolicyEntry {
        agent,
        complexity,
        model,
        effort,
        model_source,
        effort_source,
        overrides: if project.is_some() { local } else { global },
    })
}

fn view(
    tx: &impl ReadOps,
    project: Option<ProjectId>,
    target: Option<(&str, EngineAgent)>,
) -> Result<PolicyView, AppError> {
    let mut entries = Vec::new();
    for agent in [EngineAgent::Codex, EngineAgent::Claude] {
        for complexity in Complexity::ALL {
            entries.push(entry(tx, project, agent, complexity)?);
        }
    }
    let resolved = match target {
        Some((id, agent)) => {
            let project = project.ok_or_else(|| {
                AppError::Usage("resolve requires a project; omit --global".into())
            })?;
            let prefix = super::project_prefix(tx, project)?;
            let (_, row) = super::resolve_story(tx, project, &prefix, id)?;
            Some(ResolvedPolicy {
                story_id: row.snapshot.id,
                complexity_assessed: row.snapshot.complexity_assessed,
                selection: entry(tx, Some(project), agent, row.snapshot.complexity)?,
            })
        }
        None => None,
    };
    Ok(PolicyView {
        scope: if project.is_some() {
            "project"
        } else {
            "installation"
        }
        .into(),
        entries,
        resolved,
    })
}

/// Reads effective settings and an optional story selection in one transaction.
pub fn show<S: Store>(
    store: &S,
    project: Option<ProjectId>,
    target: Option<(&str, EngineAgent)>,
) -> Result<PolicyView, AppError> {
    store.read(|tx| Ok(view(tx, project, target)))?
}

/// Changes supplied fields atomically. JSON null removes a field's override.
pub fn patch<S: Store>(
    store: &S,
    project: Option<ProjectId>,
    agent: EngineAgent,
    complexity: Complexity,
    fields: &Map<String, Value>,
) -> Result<PolicyView, AppError> {
    if fields.is_empty() {
        return Err(AppError::Usage(
            "supply model or effort; null restores inheritance".into(),
        ));
    }
    for (key, value) in fields {
        if !matches!(key.as_str(), "model" | "effort") {
            return Err(AppError::Validation(format!(
                "unknown dispatch policy field `{key}`; use model or effort"
            )));
        }
        if value.is_null() {
            continue;
        }
        let raw = value.as_str().ok_or_else(|| {
            AppError::Validation(format!("policy {key} must be a string or null"))
        })?;
        if key == "model" {
            validate_model(agent, raw)?;
        } else {
            validate_effort(agent, raw)?;
        }
    }
    store.write(|tx| {
        let mut saved = tx.dispatch_policy(project, agent, complexity)?;
        for (key, value) in fields {
            let slot = if key == "model" {
                &mut saved.model
            } else {
                &mut saved.effort
            };
            *slot = value.as_str().map(str::to_string);
        }
        tx.put_dispatch_policy(project, agent, complexity, &saved)?;
        Ok(view(tx, project, None))
    })?
}
