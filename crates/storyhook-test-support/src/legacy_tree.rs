//! A direct writer for the legacy `.storyhook/` on-disk layout.
//!
//! Legacy fixtures used to be built by calling [`storyhook::app::run`] — the
//! pre-rearchitecture entry point — which meant the *deletion* of that entry
//! point was blocked by a test helper. It is not the fixture's job to keep a
//! dead code path alive, so the fixture writes the tree itself.
//!
//! # Only `init` and `new_story`
//!
//! Those two are the whole of what a fixture needs: something to migrate, and
//! something in it. A test that wants a *closed* story, a relationship, or a
//! member on a legacy tree does not exist and should not — the store is where
//! stories are edited now, and the only reasons a legacy tree still gets built
//! are `story migrate` (whose subject is a tree it imports) and the
//! unmigrated-repository guard (which only asks whether one is there).
//!
//! # It is the reader that says whether this is right
//!
//! The format is frozen and read-only, described by [`storyhook::legacy`]. This
//! module's unit tests build a tree and read it back through
//! [`storyhook::legacy::read_project`], so "the writer agrees with the reader"
//! is asserted rather than asserted-by-eye — which is the only guarantee worth
//! having, because nothing else writes this layout any more.

use std::fs;
use std::path::Path;

use storyhook::domain::{StateDef, StoryEvent, SuperState, TypeDef};
use storyhook::service::Clock;

/// The prefix `story init` used when `--prefix` was not given.
const DEFAULT_PREFIX: &str = "SH";

/// Creates a legacy project at `root` — what `story init` left behind.
///
/// Every file the reader looks for, and the directories the tree carried:
/// `project.toml`, `states.toml`, `types.toml`, an empty `members.jsonl`, a
/// `next-id` counter at 1, `open/stories`, `open/indexes` and `archive`.
///
/// `archive/archive.db` is deliberately *not* created. The reader treats a
/// missing archive as "no closed stories" (`read_archived_stories` returns
/// early), which is true of a project that has never closed one — and creating
/// an empty SQLite file here would make this module depend on rusqlite to say
/// nothing.
///
/// # Panics
///
/// If any part of the tree cannot be written. A fixture that half-exists
/// produces failures that read as bugs in the code under test.
pub fn init(root: &Path, prefix: Option<&str>) {
    let dir = root.join(".storyhook");
    for sub in ["open/stories", "open/indexes", "archive"] {
        fs::create_dir_all(dir.join(sub))
            .unwrap_or_else(|e| panic!("creating {}/{sub}: {e}", dir.display()));
    }

    let mut project = format!("schema = 1\ncreated_at = \"{}\"\n", Clock::System.now());
    if let Some(prefix) = prefix {
        project.push_str(&format!("prefix = \"{prefix}\"\n"));
    }
    write(&dir.join("project.toml"), &project);
    write(&dir.join("states.toml"), &states_toml(&default_states()));
    write(&dir.join("types.toml"), &types_toml(&default_types()));
    write(&dir.join("members.jsonl"), "");
    write(&dir.join("next-id"), "1\n");
    write(
        &dir.join(".gitignore"),
        "# Runtime files — not project data\nlock\narchive/*.db-wal\narchive/*.db-shm\n",
    );
}

/// Creates a story in the legacy tree at `root` and returns the id it minted.
///
/// The counter is read, consumed and written back, exactly as
/// `storage::next_story_id` did: a fixture that sidestepped `next-id` would
/// hand `story migrate` a tree whose counter disagrees with its stories, which
/// is a corruption no real project could reach.
///
/// # Panics
///
/// If there is no legacy project at `root`, or the tree cannot be written.
#[must_use]
pub fn new_story(root: &Path, title: &str) -> String {
    let dir = root.join(".storyhook");
    let counter = dir.join("next-id");
    let current = fs::read_to_string(&counter).unwrap_or_else(|e| {
        panic!(
            "reading {}: {e} — is there a legacy project here?",
            counter.display()
        )
    });
    let number: u64 = current
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("{} does not hold a counter: {e}", counter.display()));
    write(&counter, &format!("{}\n", number + 1));

    let id = format!("{}-{number}", prefix_at(root));
    let created = StoryEvent::StoryCreated {
        at: Clock::System.now(),
        title: title.to_string(),
        // The first OPEN state, which is what `storage::default_open_state`
        // answered and what `init` above writes first.
        state: default_states()[0].slug.clone(),
    };
    write(
        &dir.join("open/stories").join(format!("{id}.jsonl")),
        &format!(
            "{}\n",
            serde_json::to_string(&created).expect("a StoryCreated event serializes")
        ),
    );
    id
}

/// The project's configured prefix, or the default when it has none.
///
/// Parsed with a line scan rather than a TOML crate: this module writes the
/// file two functions above, the shape is three keys, and the harness has no
/// TOML dependency to add for it.
fn prefix_at(root: &Path) -> String {
    let path = root.join(".storyhook/project.toml");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    text.lines()
        .find_map(|line| line.strip_prefix("prefix = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(DEFAULT_PREFIX)
        .to_string()
}

/// `story init`'s states: `todo`, `in-progress` (the active one), `done`.
fn default_states() -> Vec<StateDef> {
    vec![
        StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "in-progress".to_string(),
            super_state: SuperState::Open,
            role: Some("active".to_string()),
            description: None,
        },
        StateDef {
            slug: "done".to_string(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ]
}

/// `story init`'s story types, in the legacy `.storyhook/` format — which
/// predates the `emoji` column (SH-157) and genuinely could hold `task`, so
/// neither is normalized away here the way `service::project::default_types`
/// now normalizes them for a fresh store.
fn default_types() -> Vec<TypeDef> {
    [
        ("story", "A user story or feature"),
        ("epic", "A large initiative containing child stories"),
        ("bug", "A defect or regression"),
        ("chore", "Maintenance or infrastructure work"),
        ("task", "A discrete unit of work"),
    ]
    .into_iter()
    .map(|(slug, description)| TypeDef {
        slug: slug.to_string(),
        description: Some(description.to_string()),
        emoji: None,
    })
    .collect()
}

/// `states.toml`, as an array of tables.
///
/// The TOML key for the superstate is `super`, not `super_state` — `StateDef`
/// renames it — and getting that wrong produces a tree the reader rejects,
/// which is what this module's round-trip test is for.
fn states_toml(states: &[StateDef]) -> String {
    let mut text = String::new();
    for state in states {
        text.push_str("[[states]]\n");
        text.push_str(&format!("slug = {}\n", quote(&state.slug)));
        text.push_str(&format!("super = {}\n", quote(state.super_state.as_str())));
        if let Some(role) = &state.role {
            text.push_str(&format!("role = {}\n", quote(role)));
        }
        if let Some(description) = &state.description {
            text.push_str(&format!("description = {}\n", quote(description)));
        }
        text.push('\n');
    }
    text
}

/// `types.toml`, as an array of tables.
fn types_toml(types: &[TypeDef]) -> String {
    let mut text = String::new();
    for story_type in types {
        text.push_str("[[types]]\n");
        text.push_str(&format!("slug = {}\n", quote(&story_type.slug)));
        if let Some(description) = &story_type.description {
            text.push_str(&format!("description = {}\n", quote(description)));
        }
        text.push('\n');
    }
    text
}

/// A TOML basic string.
///
/// TOML's escapes for the characters that can appear in a slug or a
/// description are JSON's, so `serde_json` is the encoder rather than a
/// hand-rolled one — a fixture that mangled a quote would fail somewhere far
/// from here.
fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes")
}

/// `fs::write`, naming the file it could not write.
fn write(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_dir;

    /// The writer agrees with the reader — the only claim worth making about a
    /// format nothing else writes any more.
    #[test]
    fn a_written_tree_reads_back_through_the_legacy_reader() {
        let dir = scratch_dir();
        init(dir.path(), None);
        let id = new_story(dir.path(), "Something to migrate");
        assert_eq!(id, "SH-1");

        let project = storyhook::legacy::read_project(dir.path()).expect("reading it back");
        assert_eq!(project.schema, 1);
        assert_eq!(project.prefix, None, "an unset prefix stays unset");
        assert_eq!(project.effective_prefix(), "SH");
        assert_eq!(project.next_id, 2, "creating a story consumes the counter");
        assert_eq!(project.states, default_states());
        assert_eq!(project.types, default_types());
        assert!(project.members.is_empty());

        assert_eq!(project.stories.len(), 1);
        let story = &project.stories[0];
        assert_eq!(story.id, "SH-1");
        assert!(!story.archived);
        match &story.events[0].decoded {
            Some(StoryEvent::StoryCreated { title, state, .. }) => {
                assert_eq!(title, "Something to migrate");
                assert_eq!(state, "todo");
            }
            other => panic!("expected a StoryCreated event, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_prefix_reaches_the_minted_ids_and_the_project_file() {
        let dir = scratch_dir();
        init(dir.path(), Some("TST"));
        assert_eq!(new_story(dir.path(), "first"), "TST-1");
        assert_eq!(new_story(dir.path(), "second"), "TST-2");

        let project = storyhook::legacy::read_project(dir.path()).expect("reading it back");
        assert_eq!(project.prefix.as_deref(), Some("TST"));
        assert_eq!(project.next_id, 3);
        assert_eq!(project.stories.len(), 2);
    }

    /// `LegacyPaths::exists` is the marker the unmigrated-repository guard and
    /// `story migrate` both look for, so a tree this module writes has to trip
    /// it.
    #[test]
    fn the_tree_is_findable_as_a_legacy_project() {
        let dir = scratch_dir();
        let deep = dir.path().join("src/inner");
        fs::create_dir_all(&deep).expect("creating a subdirectory");
        init(dir.path(), None);

        let found = storyhook::legacy::find_root(&deep)
            .expect("the walk up from a subdirectory must find the tree");
        assert_eq!(
            found,
            dir.path()
                .canonicalize()
                .expect("canonicalizing the fixture"),
            "and must find *this* tree"
        );
    }
}
