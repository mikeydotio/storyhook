//! Every help topic storyhook points a user at must exist (SH-93).
//!
//! `MIGRATED.txt` — the one message a user reads at the moment their story data
//! leaves directories they controlled for a store they have never seen — told
//! them to run `story help storage`, and there was no `storage` topic. The
//! pointer was dead from the commit that introduced it (`bdbb60f`).
//!
//! W8 pinned the same standard for the corruption diagnostic, on the stated
//! grounds that a message pointing somewhere empty is worse than none. This is
//! that standard, applied mechanically to every help reference in the shipped
//! source rather than to one message at a time: the defect class is "a message
//! names a help topic that does not exist", and it is checkable.
//!
//! Hermetic by construction — it reads the source tree and calls
//! [`storyhook::help_topics::get_help_topic`] directly. No fixture, no store, no
//! daemon, nothing to isolate.

use std::path::{Path, PathBuf};

use storyhook::help_topics::{get_help_topic, list_topics};

/// How this codebase writes a command reference in prose and in shipped
/// strings: inside backticks. Matching the delimiter rather than a bare word is
/// what keeps English out of the results — `docs/` contains the sentence "users
/// running story --help or story help cannot discover…", and a looser pattern
/// reads `cannot` as a topic.
const MARKER: &str = "`story help ";

fn rust_sources(at: &Path, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(at).expect("reading a source directory");
    for entry in entries {
        let path = entry.expect("reading a directory entry").path();
        if path.is_dir() {
            rust_sources(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

/// The topics named inside backticks in `text`.
///
/// Flags are not topics: `story help --all` and `story help --compact` are
/// output modes, and a leading `-` is what distinguishes them. Placeholders
/// (`story help <command>`) fall out on their own, since `<` is not a topic
/// character.
fn referenced_topics(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        rest = &rest[at + MARKER.len()..];
        let Some(end) = rest.find('`') else { break };
        let topic = &rest[..end];
        if !topic.is_empty()
            && !topic.starts_with('-')
            && topic
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            found.push(topic.to_string());
        }
    }
    found
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `story help <topic>` the shipped source names resolves to a real topic.
#[test]
fn every_help_topic_the_source_points_at_exists() {
    let mut files = Vec::new();
    rust_sources(&source_root(), &mut files);
    assert!(!files.is_empty(), "found no Rust sources to scan");

    let mut dangling = Vec::new();
    let mut checked = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file).expect("reading a source file");
        for topic in referenced_topics(&text) {
            checked += 1;
            if get_help_topic(&topic).is_none() {
                dangling.push(format!("  {}: `story help {topic}`", file.display()));
            }
        }
    }

    // Vacuity guard. A scan that matches nothing passes forever, and this
    // pattern depends on a convention (backticked command references) that a
    // later edit could quietly drop. If this fires, the scan stopped watching —
    // fix the pattern, do not lower the bound.
    assert!(
        checked > 0,
        "the scan found no `story help <topic>` references at all across {} files — \
         it has stopped testing anything",
        files.len()
    );

    assert!(
        dangling.is_empty(),
        "these messages point at help topics that do not exist:\n{}\n\nAvailable topics: {}",
        dangling.join("\n"),
        list_topics().join(", ")
    );
}

/// The reported instance, asserted on its own so a regression names itself.
#[test]
fn the_storage_topic_exists() {
    assert!(
        get_help_topic("storage").is_some(),
        "`story help storage` is what MIGRATED.txt tells a user to run after their \
         data moves into the store; available topics: {}",
        list_topics().join(", ")
    );
}

/// The topic has to carry the facts it exists to deliver.
///
/// Presence alone would satisfy [`the_storage_topic_exists`] with an empty
/// string. The reason this topic is worth adding rather than deleting the
/// promise is the restore procedure: copying a snapshot over `store.db` while
/// its `-wal` and `-shm` sidecars survive turns a recoverable machine into a
/// malformed one, which `backup_restore.rs` proves. A storage topic that
/// omitted the sidecars would be worse than no topic.
#[test]
fn the_storage_topic_documents_the_whole_restore() {
    let topic = get_help_topic("storage").expect("the storage topic exists");

    for fact in ["store.db", "-wal", "-shm", "story doctor", "story migrate"] {
        assert!(
            topic.contains(fact),
            "the storage topic must name `{fact}` — a partial restore procedure is \
             the failure mode it exists to prevent"
        );
    }
}

/// The topic omitted `$STORYHOOK_STORE_PATH` entirely (SH-95) — the lever that
/// *outranks* `$STORYHOOK_DATA_DIR`, which it did list. A downstream test suite
/// isolating itself by reading this topic set the losing variable and had its
/// writes go to whatever store `$STORYHOOK_STORE_PATH` (or `--store-path`)
/// already named — silently, since the higher lever wins without complaint.
/// Naming all four levers, and `story store new` as the supported way to get a
/// scratch store, is the preventative action for that defect class.
#[test]
fn the_storage_topic_names_every_store_naming_lever_in_precedence_order() {
    let topic = get_help_topic("storage").expect("the storage topic exists");

    for fact in [
        "--store-path",
        "STORYHOOK_STORE_PATH",
        "STORYHOOK_DATA_DIR",
        "story store new",
    ] {
        assert!(
            topic.contains(fact),
            "the storage topic must name `{fact}` — a suite that follows this topic \
             and sets only the variable it omits is not isolated, silently"
        );
    }

    let store_path_at = topic.find("STORYHOOK_STORE_PATH").unwrap();
    let data_dir_at = topic.find("STORYHOOK_DATA_DIR").unwrap();
    assert!(
        store_path_at < data_dir_at,
        "STORYHOOK_STORE_PATH outranks STORYHOOK_DATA_DIR and must be named first, \
         so the topic's order matches the precedence it documents"
    );
}
