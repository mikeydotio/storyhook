//! `story session-start` — the JSON block an agent's session hook injects.
//!
//! Answers Claude Code's SessionStart envelope:
//! `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"…"}}`,
//! or a bare `{}` when there is nothing to say. The bare form matters as much as
//! the full one: a hook that fails or prints a diagnostic pollutes the model's
//! context, so "no project here" and "the plugin is switched off" both have to
//! be silence rather than an error.

use std::collections::BTreeMap;

use crate::domain::{Priority, StorySnapshot, has_children, is_ready};
use crate::error::AppError;
use crate::help_topics;
use crate::store::{ReadOps, Store, StoryQuery};

use super::Ctx;

/// The character budget for the injected context.
///
/// Under 4000, and truncated at a character boundary rather than a byte: the
/// context is prepended to a model's window, and a project with two hundred open
/// stories should cost a fixed amount of it.
const BUDGET: usize = 3900;

/// The answer for a directory storyhook knows nothing about, and for a project
/// whose plugin has been switched off.
pub const SILENT: &str = "{}";

/// Session context over one project in one store.
pub struct SessionService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> SessionService<'ctx, S> {
    /// A session service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// The raw JSON `story session-start` prints.
    pub fn context(&self) -> Result<String, AppError> {
        if plugin_disabled(self.ctx.cwd()) {
            return Ok(SILENT.to_string());
        }

        let mut message = String::new();
        message.push_str(help_topics::compact_reference());
        message.push_str("PROJECT STATE\n");

        let project = self.ctx.project();
        // Open stories only, and the readiness question is asked *of that map*
        // — a story blocked by an archived one reads as ready, because the
        // blocker is not in the map to be found. Inherited from the legacy
        // path, which passed `load_all_open_snapshots` for the same reason and
        // whose answer `story next` agrees with.
        let stories = self.ctx.store().read(|tx| {
            Ok(tx
                .stories(project, &StoryQuery::all().archived(false))?
                .into_iter()
                .map(|row| (row.snapshot.id.clone(), row.snapshot))
                .collect::<BTreeMap<String, StorySnapshot>>())
        });
        let stories = match stories {
            Ok(stories) => stories,
            // The CLI reference alone is still worth injecting: an agent that
            // cannot read the project can at least be told how to ask.
            Err(_) => {
                message.push_str("  Unable to load project state.\n");
                return Ok(envelope(&message));
            }
        };

        let open: Vec<&StorySnapshot> = stories.values().collect();
        let ready: Vec<&StorySnapshot> = open
            .iter()
            .copied()
            .filter(|story| is_ready(story, &stories) && !has_children(story))
            .collect();
        message.push_str(&format!(
            "  {} open stories, {} ready\n",
            open.len(),
            ready.len()
        ));

        if let Some(next) = highest_priority(ready) {
            let priority = if next.priority == Priority::None {
                String::new()
            } else {
                format!(" ({})", next.priority.as_str())
            };
            message.push_str(&format!(
                "  Next: {} — {}{}\n",
                next.id, next.title, priority
            ));
        }

        Ok(envelope(&truncate(message)))
    }
}

/// The most urgent ready story, ties broken by creation time.
///
/// Inherits the legacy comparator, second-precision ties and all: two stories
/// created within one second of each other tie on both keys, and which one wins
/// depends on iteration order. Reproduced rather than corrected because
/// `story next` has the same comparator and the two must not disagree; the wave
/// that gives it a total order fixes both.
fn highest_priority(ready: Vec<&StorySnapshot>) -> Option<&StorySnapshot> {
    let mut sorted = ready;
    sorted.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    sorted.into_iter().next()
}

/// Clips the message to [`BUDGET`] characters, never mid-character.
fn truncate(mut message: String) -> String {
    if message.len() <= BUDGET {
        return message;
    }
    let mut end = BUDGET;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str("\n...(truncated)\n");
    message
}

/// Wraps `message` in the SessionStart hook envelope.
///
/// `additionalContext`, not `systemMessage`: the former is injected silently
/// into the model's context, the latter is rendered as a visible block the user
/// did not ask for.
fn envelope(message: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": message,
        }
    })
    .to_string()
}

/// Whether the repository has switched the plugin off.
///
/// The committed pointer file's `[plugin]` table first, the legacy
/// `<root>/.storyhook/plugin-config.toml` second. This is *user-authored*
/// config rather than story data, so it stays in the repository; what the flip
/// changes is which file in the repository holds it, and both are read while
/// the two storage models coexist.
fn plugin_disabled(root: &std::path::Path) -> bool {
    if let Some(table) = super::project::pointer_plugin(root) {
        return table
            .get("enabled")
            .is_some_and(|value| is_off(value.clone()));
    }
    let path = root.join(".storyhook/plugin-config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    disabled_by(&content)
}

/// Whether an `enabled` value says the plugin is off.
///
/// Tolerates `false` and `"false"`, the two shapes that have ever been written,
/// and treats anything else — including a number, a table, or a typo — as
/// "on". Failing open is the rule for every read on this path.
fn is_off(value: toml::Value) -> bool {
    match value {
        toml::Value::Boolean(flag) => !flag,
        toml::Value::String(text) => text.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// Reads `enabled` out of a plugin config, in either of the two shapes that
/// have ever been written.
///
/// Fails **open**: a malformed file leaves the plugin enabled, because a typo in
/// a config file should not silently stop an agent from being told what project
/// it is in.
fn disabled_by(content: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct PluginTable {
        enabled: Option<toml::Value>,
    }
    #[derive(serde::Deserialize)]
    struct PluginConfig {
        enabled: Option<toml::Value>,
        plugin: Option<PluginTable>,
    }

    let Ok(config) = toml::from_str::<PluginConfig>(content) else {
        return false;
    };
    if let Some(table) = config.plugin
        && let Some(value) = table.enabled
    {
        return is_off(value);
    }
    config.enabled.is_some_and(is_off)
}

/// One story's history, for the seam's undo primitive.
///
/// Answers with the events this binary understands, in order; an unknown kind is
/// omitted rather than failing the read, which is what the legacy path did by
/// being unable to represent one at all.
pub fn history<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
) -> Result<Vec<crate::domain::StoryEvent>, AppError> {
    let project = ctx.project();
    Ok(ctx.store().read(|tx| {
        let prefix = super::project_prefix(tx, project)?;
        let Ok(story_no) = crate::store::StoryNo::parse_id(&prefix, id) else {
            return Ok(Vec::new());
        };
        let stored = tx.events_for(project, story_no)?;
        Ok(crate::store::partition_known(story_no, &stored).0)
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_enabled_key_switches_the_plugin_off() {
        assert!(disabled_by("enabled = false"));
        assert!(disabled_by("enabled = \"FALSE\""));
        assert!(!disabled_by("enabled = true"));
    }

    #[test]
    fn a_nested_plugin_table_wins_over_a_bare_key() {
        assert!(disabled_by("enabled = true\n[plugin]\nenabled = false"));
        assert!(!disabled_by("enabled = false\n[plugin]\nenabled = true"));
    }

    #[test]
    fn a_malformed_config_leaves_the_plugin_enabled() {
        assert!(!disabled_by("this is not toml at all ["));
        assert!(!disabled_by(""));
    }
}
