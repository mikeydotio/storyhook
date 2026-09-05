//! `story session-start` — the JSON block an agent's session hook injects.
//!
//! Answers Claude Code's SessionStart envelope:
//! `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"…"}}`,
//! or a bare `{}` when there is nothing to say. The bare form matters as much as
//! the full one: a hook that fails or prints a diagnostic pollutes the model's
//! context, so "no project here" and "the plugin is switched off" both have to
//! be silence rather than an error.

use std::collections::BTreeMap;
use std::io::Write;

use crate::domain::{
    Priority, StorySnapshot, active_state, apply_computed_epic_states, is_claimable, is_epic,
    ready_order,
};
use crate::error::AppError;
use crate::help_topics;
use crate::store::{ReadOps, Store, StoryQuery};

use super::Ctx;

/// The dispatch sentinel's own schema version.
///
/// Bumped whenever a field is added, renamed or removed — a reader that
/// understands an older version can then tell "absent" from "renamed" instead
/// of guessing.
const SENTINEL_PROTOCOL_VERSION: u32 = 1;

/// The dispatch sentinel written on every `SessionStart`, at
/// `<cwd>/.claude/dispatch-sentinel.json` — SH-231 (SH-227 R2), replacing
/// `lib/session.sh`'s screen-scraped readiness gate for `story.sh dispatch`.
///
/// # What this proves, and what it deliberately does not
///
/// Existence at that path proves *a* Claude Code SessionStart hook fired with
/// this cwd — nothing more. `story.sh` only ever polls for it inside a
/// worktree it just created itself (`git worktree add`, refused if the path
/// already exists), so the FIRST sentinel found there is guaranteed to belong
/// to the dispatch that created that worktree — but a *later* one, from an
/// unrelated session someone points at the same directory afterward, would
/// look identical. That is why this file carries no pid: a hook cannot learn
/// its own claude process's pid (confirmed against Claude Code's documented
/// `SessionStart` payload — session_id/hook_event_name/source/cwd/
/// permission_mode/model, no process identity), and `$PPID` as a proxy is
/// undocumented. `story.sh`'s own dispatcher, not this file, decides
/// liveness: it already captured the pane's pid itself
/// (`#{pane_pid}`, SH-230) and re-checks that exact pid against the exact
/// pane at verification time (SH-231's council verdict)
/// — the sentinel is the "something happened here" half of that check, the
/// re-queried pane is the "and it is still true" half.
#[derive(Debug, serde::Serialize)]
struct DispatchSentinel {
    protocol_version: u32,
    /// From the SessionStart hook payload's own `session_id` — absent if
    /// stdin was empty, unparseable, or the field was missing. Diagnostic
    /// only: nothing on the readiness path matches against it.
    session_id: Option<String>,
    /// Best-effort, derived from `cwd`'s own final path component — the
    /// dispatch worktree's directory name, which is the story id under
    /// every naming scheme this plugin ships (`resolve_wname`) unless a
    /// caller has overridden `STORY_WINDOW_NAME`. Diagnostic only, for the
    /// same reason `session_id` is: identity here comes from *which path*
    /// the dispatcher polls, not from content inside the file it finds.
    story_id: Option<String>,
    written_at: String,
}

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
        // whose answer `story next` agrees with. States ride along in the same
        // transaction so `is_claimable` can resolve the active state (SH-236)
        // against a snapshot consistent with the stories it's checked against.
        let loaded = self.ctx.store().read(|tx| {
            let mut stories = tx
                .stories(project, &StoryQuery::all())?
                .into_iter()
                .map(|row| (row.snapshot.id.clone(), row.snapshot))
                .collect::<BTreeMap<String, StorySnapshot>>();
            let states = tx.states(project)?;
            apply_computed_epic_states(&mut stories, &states);
            Ok((stories, states))
        });
        let (stories, states) = match loaded {
            Ok(pair) => pair,
            // The CLI reference alone is still worth injecting: an agent that
            // cannot read the project can at least be told how to ask.
            Err(_) => {
                message.push_str("  Unable to load project state.\n");
                return Ok(envelope(&message));
            }
        };

        let active = active_state(&states);
        let open: Vec<&StorySnapshot> = stories
            .values()
            .filter(|story| story.superstate == crate::domain::SuperState::Open)
            .collect();
        let ready: Vec<&StorySnapshot> = open
            .iter()
            .copied()
            .filter(|story| is_claimable(story, &stories, active.as_ref()) && !is_epic(story))
            .collect();
        message.push_str(&format!(
            "  {} open stories, {} ready\n",
            open.len(),
            ready.len()
        ));

        if let Some(next) = highest_priority(ready, &stories) {
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

    /// Publishes the dispatch sentinel for this session — see
    /// [`DispatchSentinel`]'s own doc comment for what it proves.
    ///
    /// # Why this never returns a `Result`
    ///
    /// `context()`'s whole job is the envelope an agent reads; this file's
    /// header says why that must degrade to silence rather than an error on
    /// failure. A sentinel-write failure (a read-only worktree, a full disk, a
    /// `.claude` that is a file rather than a directory) is a DIFFERENT
    /// failure with nothing to say to the model, so it never becomes this
    /// function's own `Result` — composing it into `context()`'s error path
    /// would mean a full disk blanking out real project state that loaded
    /// correctly.
    ///
    /// # Why it is not swallowed either (SH-544)
    ///
    /// Not returning an error is not the same as discarding one. This runs
    /// inside `Invocation::SessionStart`'s handler, which for a real dispatch
    /// executes on the daemon's own worker thread (SH-114: the daemon is the
    /// only door to the store) — so a warning logged here lands in
    /// `daemon.log`, an operational surface nobody but an investigator reads,
    /// never in the model's context and never in the hook script's own
    /// discarded stderr (`session-start.sh`'s `2>/dev/null` only discards the
    /// short-lived *client* process, a different process from the daemon).
    /// Before this, a write failure here was invisible even to a human
    /// investigator: `story.sh dispatch`'s own readiness gate polls for this
    /// exact file, so a silently-failed write here reads at the caller as
    /// "SessionStart hook missing or disabled" (`wait_ready_sentinel`'s
    /// `no-sentinel` reason) — the CLI's own diagnosis pointing at the wrong
    /// layer entirely, with nothing anywhere naming the real cause.
    pub fn publish_sentinel(&self) {
        let sentinel = DispatchSentinel {
            protocol_version: SENTINEL_PROTOCOL_VERSION,
            session_id: self.ctx.stdin().and_then(session_id_from_payload),
            story_id: story_id_from_cwd(self.ctx.cwd()),
            written_at: self.ctx.now(),
        };
        if let Err(error) = write_sentinel(self.ctx.cwd(), &sentinel) {
            eprintln!("{}", sentinel_write_failure_warning(self.ctx.cwd(), &error));
        }
    }
}

/// The `daemon.log` line [`SessionService::publish_sentinel`] emits when its
/// own write fails — a pure function so the message is testable without a
/// filesystem or a subprocess, matching the `crate::daemon::agent::warning`
/// pattern `src/daemon/lifecycle.rs`'s own `eprintln!` call sites already
/// use for daemon-observable diagnostics.
fn sentinel_write_failure_warning(cwd: &std::path::Path, error: &AppError) -> String {
    format!(
        "warning: SessionStart could not publish its dispatch sentinel at {}/.claude/\
         dispatch-sentinel.json: {error} — a `story.sh dispatch` readiness gate polling \
         this path will time out with `no-sentinel` even though this SessionStart itself \
         is proceeding normally",
        cwd.display()
    )
}

/// Pulls `session_id` out of a raw SessionStart hook payload. `None` on
/// anything short of a well-formed object carrying a non-empty string —
/// swallowed rather than surfaced, since this field is diagnostic-only (see
/// [`DispatchSentinel::session_id`]).
fn session_id_from_payload(raw: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Payload {
        session_id: Option<String>,
    }
    serde_json::from_str::<Payload>(raw)
        .ok()?
        .session_id
        .filter(|id| !id.is_empty())
}

/// The dispatch worktree's directory name — see
/// [`DispatchSentinel::story_id`] for why this is best-effort.
fn story_id_from_cwd(cwd: &std::path::Path) -> Option<String> {
    cwd.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_owned)
}

/// Atomically publishes `sentinel` at `<cwd>/.claude/dispatch-sentinel.json`
/// — create-directory, write-to-a-sibling-tempfile, `fsync`, rename, the same
/// idiom `daemon::lifecycle::write_info` uses for the portfile, so a poller
/// reading this path never observes a half-written file. The tempfile is a
/// sibling of the final path so the rename is same-filesystem and therefore
/// atomic.
fn write_sentinel(cwd: &std::path::Path, sentinel: &DispatchSentinel) -> Result<(), AppError> {
    let dir = cwd.join(".claude");
    std::fs::create_dir_all(&dir)?;
    let final_path = dir.join("dispatch-sentinel.json");
    let temp_path = final_path.with_extension("json.tmp");

    let mut file = std::fs::File::create(&temp_path)?;
    file.write_all(serde_json::to_string_pretty(sentinel)?.as_bytes())?;
    file.sync_all()?;
    drop(file);

    std::fs::rename(&temp_path, &final_path)?;
    Ok(())
}

/// The most urgent ready story — [`domain::ready_order`](crate::domain::ready_order)'s
/// first element, so the story this names is always the one `story next`
/// would offer first (SH-63).
///
/// Which is why the reserved `human-only` label is dropped here too (SH-453):
/// `story next` never offers one, so naming one on this line would make the
/// sentence above false at the very surface an agent reads before it runs any
/// command at all. The exclusion is deliberately *inside* this function
/// rather than at its call site, so it cannot reach the caller's ready
/// **count** — a `human-only` story is ready, and assumption A1 requires
/// every count to go on saying so. It is just nobody's next assignment.
fn highest_priority<'a>(
    ready: Vec<&'a StorySnapshot>,
    stories: &BTreeMap<String, StorySnapshot>,
) -> Option<&'a StorySnapshot> {
    let mut sorted: Vec<&StorySnapshot> = ready
        .into_iter()
        .filter(|story| !crate::domain::is_human_only(story))
        .collect();
    sorted.sort_by(|a, b| ready_order(a, b, stories));
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

/// What `story session-start` answers when it could not load project state at
/// all — a failure caught before, or instead of, an ordinary answer from
/// [`SessionService::context`].
///
/// # Why this is cause-agnostic
///
/// The caller here is a hook an agent harness kills on its own clock, well
/// inside the 150s a `story` command may legitimately spend starting a cold
/// daemon (`SPAWN_LOCK_DEADLINE`, 30s) and waiting for it to finish
/// (`SERVED_DEADLINE`, 120s) — see SH-182. Whatever the underlying cause —
/// spawn-lock contention, a `--deadline` this hook set expiring, a store that
/// will not open — the remedy is the same for all of them: retry from a
/// command with no external clock over it, or ask why. So this asks one
/// question rather than diagnosing the failure it was given: does this
/// checkout *claim* a project? If nothing here does, or the plugin is
/// switched off, silence stays correct — indistinguishable from "no
/// storyhook project", which is what [`SILENT`] has always meant, and
/// changing that here would be scope this fix does not need.
#[must_use]
pub fn unavailable(cwd: &std::path::Path) -> String {
    if plugin_disabled(cwd) || super::project::pointer_at_or_above(cwd).is_none() {
        return SILENT.to_string();
    }
    envelope(
        "storyhook is set up in this repository, but its project state could not be \
         loaded in time. Run `story load-context` to retry outside this hook's budget \
         — it will say why if it fails too.",
    )
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

/// One story's write history, for `story log` (SH-246).
///
/// Unlike [`history`] — the undo primitive, which answers an unknown story with
/// an empty log — this **refuses** an id it cannot resolve. The two have
/// opposite obligations: a client asking "what was there before?" about a story
/// that never existed is owed the honest empty answer, while a person typing
/// `story log SH-999` mid-incident must not be shown a blank trail that reads
/// exactly like a story nothing ever touched.
///
/// An event whose kind this binary does not understand is **listed anyway**,
/// with no detail. SH-54's rule is that an unknown kind cannot fail a read, and
/// an audit trail that silently omits the rows it cannot parse would be
/// misleading in precisely the situation it exists for.
pub fn story_log<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
) -> Result<(String, String, Vec<crate::output::LogEntry>), AppError> {
    let project = ctx.project();
    Ok(ctx.store().read(|tx| {
        let prefix = super::project_prefix(tx, project)?;
        let (story_no, row) = super::resolve_story(tx, project, &prefix, id)?;
        let stored = tx.events_for(project, story_no)?;
        let entries = stored
            .into_iter()
            .map(|event| crate::output::LogEntry {
                seq: event.seq.get(),
                at: event.at.clone(),
                kind: event.kind.clone(),
                detail: event.known().map(event_detail),
                command: event.provenance.command.clone(),
                actor: event
                    .provenance
                    .actor
                    .as_ref()
                    .map(|actor| actor.as_str().to_string()),
            })
            .collect();
        Ok((story_no.to_id(&prefix), row.snapshot.title.clone(), entries))
    })?)
}

/// What one event changed, in a phrase a person can scan.
///
/// Deliberately short and lossy — a description-set entry says that the
/// description was set, not what it was set to. `story log` is a trail to scan
/// for *when* and *by what*; `story show` is where the current values live, and
/// reproducing a multi-paragraph body here would bury the column that matters.
fn event_detail(event: &crate::domain::StoryEvent) -> String {
    use crate::domain::StoryEvent as E;
    match event {
        E::StoryCreated { title, state, .. } => format!("created in {state} — {title}"),
        E::StoryStateChanged { state, .. } => format!("state → {state}"),
        E::StoryCleanupLeaseRecorded { .. } => "cleanup lease recorded".to_string(),
        E::StoryStateCleared { .. } => "state cleared (computed from children)".to_string(),
        E::StoryClosedAndArchived { state, .. } => format!("closed and archived in {state}"),
        E::StoryDeleted { reason, .. } => format!("deleted — {reason}"),
        E::StoryCommentAdded { text, .. } => format!("comment — {}", first_line(text)),
        E::StoryCommentRetracted { text, .. } => {
            format!("comment retracted — {}", first_line(text))
        }
        E::StoryAssigned { member_id, .. } => format!("assigned to {member_id}"),
        E::StoryAssigneeCleared { .. } => "assignee cleared".to_string(),
        E::StoryAwaitingSet { awaiting, .. } => format!("awaiting — {}", first_line(awaiting)),
        E::StoryAwaitingCleared { .. } => "awaiting cleared".to_string(),
        E::StoryRelationshipAdded {
            other_id, relation, ..
        } => format!("{relation} {other_id}"),
        E::StoryRelationshipRemoved {
            other_id, relation, ..
        } => format!("no longer {relation} {other_id}"),
        E::StoryPrioritySet { priority, .. } => format!("priority → {}", priority.as_str()),
        E::StoryPriorityCleared { .. } => "priority cleared".to_string(),
        E::StoryTypeSet { story_type, .. } => format!("type → {story_type}"),
        E::StoryLabelsSet { labels, .. } if labels.is_empty() => "labels cleared".to_string(),
        E::StoryLabelsSet { labels, .. } => format!("labels → {}", labels.join(", ")),
        E::StoryTitleSet { title, .. } => format!("title → {title}"),
        E::StoryDescriptionSet { .. } => "description set".to_string(),
        E::StoryCommitLinked { sha, subject, .. } => {
            format!(
                "commit {} — {}",
                &sha[..sha.len().min(7)],
                first_line(subject)
            )
        }
        E::StoryHidden { .. } => "hidden".to_string(),
        E::StoryUnhidden { .. } => "unhidden".to_string(),
        E::StoryPrLinked { url, .. } => format!("pull request linked — {url}"),
        E::StoryPrUnlinked { url, .. } => format!("pull request unlinked — {url}"),
        E::StoryPrMerged { url, .. } => format!("pull request merged — {url}"),
        E::StoryPrClosed { url, .. } => format!("pull request closed — {url}"),
        E::StoryCreatedAsDraft { .. } => "created as a draft".to_string(),
        E::StoryPublished { .. } => "published".to_string(),
        E::StoryAttachmentAdded { name, .. } => format!("attachment added — {name}"),
        E::StoryAttachmentRemoved { id, .. } => format!("attachment {id} removed"),
    }
}

/// The first line of a possibly-multi-line value, truncated.
///
/// A trail is a column of one-line rows; a comment body carrying its own
/// newlines would break that alignment and let a comment's *content* forge
/// entries — the same shape SH-246 refuses an actor label for, arriving through
/// a field a user is genuinely allowed to write prose into.
fn first_line(text: &str) -> String {
    const MAX: usize = 60;
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(MAX).collect();
    if line.chars().count() > MAX {
        out.push('…');
    }
    out
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

    #[test]
    fn session_id_round_trips_out_of_a_real_hook_payload() {
        let payload = r#"{"session_id":"abc-123","hook_event_name":"SessionStart","source":"startup","cwd":"/tmp/x"}"#;
        assert_eq!(
            session_id_from_payload(payload),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn session_id_is_none_for_anything_short_of_a_well_formed_field() {
        assert_eq!(session_id_from_payload(""), None, "empty stdin");
        assert_eq!(session_id_from_payload("not json"), None, "malformed JSON");
        assert_eq!(session_id_from_payload("{}"), None, "field absent");
        assert_eq!(
            session_id_from_payload(r#"{"session_id":""}"#),
            None,
            "empty string is not an identity"
        );
        assert_eq!(
            session_id_from_payload(r#"{"session_id":null}"#),
            None,
            "explicit null"
        );
    }

    #[test]
    fn story_id_is_the_cwds_final_path_component() {
        assert_eq!(
            story_id_from_cwd(std::path::Path::new("/repo/.claude/worktrees/SH-231")),
            Some("SH-231".to_string())
        );
    }

    #[test]
    fn story_id_is_none_when_cwd_has_no_final_component() {
        assert_eq!(story_id_from_cwd(std::path::Path::new("/")), None);
    }

    #[test]
    fn the_sentinel_write_failure_warning_names_the_path_the_error_and_the_consequence() {
        let error = AppError::Storage("disk full".to_string());
        let warning = sentinel_write_failure_warning(std::path::Path::new("/repo/SH-231"), &error);
        assert!(
            warning.contains("/repo/SH-231/.claude/dispatch-sentinel.json"),
            "should name the exact path a readiness gate polls for: {warning}"
        );
        assert!(
            warning.contains("disk full"),
            "should carry the underlying error, not just say something failed: {warning}"
        );
        assert!(
            warning.contains("no-sentinel"),
            "should name the observable symptom (SH-544) so a daemon.log reader can \
             connect this line to a `pane-not-ready` refusal: {warning}"
        );
        assert!(
            warning.starts_with("warning:"),
            "should match this codebase's own daemon-observable warning idiom \
             (src/daemon/lifecycle.rs): {warning}"
        );
    }
}
