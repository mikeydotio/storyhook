//! Every `StoryRow` column is compared by `store::rebuild::diff_rebuilt`, and
//! the comparison actually fires (SH-365).
//!
//! `diff_rebuilt` is `story doctor`'s oracle: it compares a persisted `stories`
//! row against a fresh fold of that story's events, one hand-written
//! `report(...)` call per column. Nothing verified that the list was complete,
//! so a column added to [`StoryRow`] without a matching `report` line was never
//! compared and the oracle went quiet about it forever. That already happened
//! once — SH-211 added the missing lines for `hidden_at` and `draft`, having
//! noticed that a write reaching those columns directly (a raw migration, a
//! hand edit) skips both the fold and the service layer and that *nothing*
//! compared them. It added the two missing lines; it added nothing preventing
//! the third.
//!
//! # Why a test at all, when the pattern is compiler-enforced
//!
//! `diff_rebuilt` now destructures `StoryRow` with no `..` rest pattern, so a
//! new column is a compile error at the comparison site. That is the fast gate,
//! and it fires on a bare `cargo check`, long before this file runs. It is not
//! sufficient, for two reasons that were measured rather than assumed:
//!
//! - An author can silence it in one character with `field: _`. Leaving that to
//!   code review is not a control (SH-306: a gate that depends on a human
//!   noticing is not a gate).
//! - **A rest-free pattern does not see a mis-wiring.** `store::sqlite::read`'s
//!   `hydrate` is *already* a rest-free struct literal over `StoryRow` — this
//!   exact mechanism, in production, on this exact type — and it still cannot
//!   see that `raw_story_from_row` fills it **by position**, `row.get(N)`,
//!   across three `Option<String>` fields, three `bool`s and four `String`s.
//!   Swap any adjacent pair and it compiles clean: every field mentioned, every
//!   field wrong.
//!
//! That second point also decides the *form* of this file. Damage is injected
//! through SQLite, underneath the store, never against a hand-built
//! `StoryRow` — a unit test over a pure comparison function would sail straight
//! past a swapped read path, which is the fault this file most wants to catch.
//! It is the idiom `tests/store_rebuild.rs` already states in its own header:
//! none of these states can be produced through the store's own API.
//!
//! # What keeps *this* file honest
//!
//! [`CASES`] is a hand-written list, which is the shape SH-136 / SH-198 /
//! SH-258 / SH-260 all record drifting. It cannot drift here, because
//! [`every_story_row_column_has_a_damage_case_or_a_stated_exemption`] derives
//! the truth from `StoryRow`'s own definition and demands the two agree — a new
//! column fails this file by name. The parser that reads that definition
//! carries a positive control of its own
//! ([`the_field_parser_still_recognises_a_struct_it_is_shown`]), so a parser
//! that stopped recognising fields fails rather than reporting a clean tree
//! (SH-364), and [`the_fixture_diffs_clean_before_any_damage`] is the negative
//! control that stops every damage assertion passing for the wrong reason.
//!
//! Mutation-checked at authoring, in both directions: deleting the
//! `report("draft", …)` line from `diff_rebuilt` makes
//! [`every_watched_column_is_named_by_the_oracle_when_it_is_damaged`] fail
//! naming `draft`, and adding a field to `StoryRow` fails `cargo check` in
//! `rebuild.rs` first and this file's completeness test second.
//!
//! # Two boundaries this file does not claim
//!
//! - It derives from **`StoryRow`**, not from the `stories` table, so a column
//!   the read model does not expose is outside it. `priority_rank` is the live
//!   example: it is written but never read back into a row, and a schema CHECK
//!   tying it to `priority` is what guards it instead of the oracle.
//! - A *symmetric* mis-comparison — the same wrong value read on both sides of
//!   a `report` — still passes here, because both sides move together. Nothing
//!   short of review sees that one, and it is recorded rather than papered over.

mod store_support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use store_support::{append_and_fold, new_store, raw, seed_project};
use storyhook::domain::{Priority, StoryEvent};
use storyhook::store::{
    EventSeq, ExpectedSeq, ProjectId, SqliteStore, Store, StoryNo, WriteOps, diff_read_model,
};

/// The title the fixture story is created with.
///
/// Distinctive on purpose: the `snapshot` case damages the embedded document by
/// substituting this string inside it, leaving the `title` *column* alone, so
/// the divergence lands on `snapshot` and on nothing else.
const FIXTURE_TITLE: &str = "Fixture story";

/// One damage case: a write the schema permits, and the `StoryRow` fields
/// `diff_rebuilt` must name once it has landed.
struct Case {
    /// What this case is called when it fails.
    name: &'static str,
    /// The `StoryRow` fields this case damages. Every one of them must appear
    /// in the resulting divergence report; the case may name others besides.
    fields: &'static [&'static str],
    /// Why these fields share one case, for the cases that cover more than one.
    ///
    /// Only ever a schema constraint that forbids damaging them separately —
    /// an exemption with no constraint behind it is SH-364's lying fixture in a
    /// new place.
    coupled_because: Option<&'static str>,
    /// The damage, run against a second connection.
    sql: &'static str,
}

/// Every column, damaged.
///
/// Each entry writes a row that is internally consistent — the schema's CHECKs
/// are respected, never fought — but that disagrees with the story's *events*,
/// which is the fault the oracle exists to find.
const CASES: &[Case] = &[
    Case {
        name: "a stale head",
        fields: &["head_seq"],
        coupled_because: None,
        sql: "UPDATE stories SET head_seq = 0",
    },
    Case {
        name: "a stale change-feed position",
        fields: &["head_global_seq"],
        coupled_because: None,
        sql: "UPDATE stories SET head_global_seq = 0",
    },
    Case {
        name: "a retitled row",
        fields: &["title"],
        coupled_because: None,
        sql: "UPDATE stories SET title = 'tampered'",
    },
    Case {
        name: "a row moved to another open state",
        fields: &["state"],
        coupled_because: None,
        // Open-to-open, so the `(project_id, state, superstate)` foreign key
        // introduced by migration 4 still resolves and `superstate` need not
        // move with it.
        sql: "UPDATE stories SET state = 'in-progress'",
    },
    Case {
        name: "a row closed underneath the store",
        fields: &["superstate", "archived", "closed_at"],
        coupled_because: Some(
            "`CHECK (archived = (closed_at IS NOT NULL))` and SH-130's \
             `CHECK ((superstate = 'CLOSED') = archived)` make these one fact \
             told three times, and the state foreign key drags the slug along \
             with them",
        ),
        sql: "UPDATE stories SET state = 'done', superstate = 'CLOSED', \
              archived = 1, closed_at = '2026-06-01T00:00:00Z'",
    },
    Case {
        name: "a reprioritised row",
        fields: &["priority"],
        coupled_because: None,
        // `priority_rank` travels with the slug or the CHECK refuses the write.
        // It is not a `StoryRow` field, so it is the schema's to guard, not the
        // oracle's — see this file's header.
        sql: "UPDATE stories SET priority = 'low', priority_rank = 3",
    },
    Case {
        name: "a retyped row",
        fields: &["story_type"],
        coupled_because: None,
        sql: "UPDATE stories SET story_type = 'tampered'",
    },
    Case {
        name: "a reassigned row",
        fields: &["assignee"],
        coupled_because: None,
        sql: "UPDATE stories SET assignee = 'tampered'",
    },
    Case {
        name: "a row made to await something",
        fields: &["awaiting"],
        coupled_because: None,
        sql: "UPDATE stories SET awaiting = 'tampered'",
    },
    Case {
        name: "a row marked deleted",
        fields: &["deleted"],
        coupled_because: None,
        sql: "UPDATE stories SET deleted = 1",
    },
    Case {
        name: "a rewritten creation timestamp",
        fields: &["created_at"],
        coupled_because: None,
        sql: "UPDATE stories SET created_at = '2020-01-01T00:00:00Z'",
    },
    Case {
        name: "a rewritten activity timestamp",
        fields: &["updated_at"],
        coupled_because: None,
        sql: "UPDATE stories SET updated_at = '2020-01-01T00:00:00Z'",
    },
    Case {
        name: "a fabricated description",
        fields: &["description"],
        coupled_because: None,
        sql: "UPDATE stories SET description = 'tampered'",
    },
    Case {
        name: "a row hidden from the primary UI",
        fields: &["hidden_at"],
        coupled_because: None,
        // No CHECK ties `hidden_at` to `archived`/`closed_at`/`superstate`
        // (schema 10), so an open story can be given one directly — which is
        // precisely the column-only write SH-211 found nothing comparing.
        sql: "UPDATE stories SET hidden_at = '2026-06-01T00:00:00Z'",
    },
    Case {
        name: "a published row pushed back to draft",
        fields: &["draft"],
        coupled_because: None,
        sql: "UPDATE stories SET draft = 1",
    },
    Case {
        name: "a label taken off the join table",
        fields: &["labels"],
        coupled_because: None,
        sql: "DELETE FROM story_labels",
    },
    Case {
        name: "an edited snapshot whose columns still agree with the events",
        fields: &["snapshot"],
        coupled_because: None,
        // The embedded document alone, leaving every column untouched: the
        // mirror image of the column-only write, and what catches a change to a
        // field that has no column of its own.
        sql: "UPDATE stories SET snapshot = replace(snapshot, 'Fixture story', 'tampered')",
    },
];

/// Columns the oracle deliberately does not compare, and why.
///
/// One entry, and it should stay that way: anything added here is a column
/// `story doctor` has stopped watching, so the reason has to say what watches
/// it instead.
const EXEMPT: &[(&str, &str)] = &[(
    "story_no",
    "the key the row was looked up by, so comparing it against itself is \
     vacuous — a row filed under the wrong number is caught by the diff's \
     `missing_rows`/`extra_rows` halves instead",
)];

/// A story with every nullable column left null and one label set, in a project
/// with the default state catalog.
fn seed_fixture_story(store: &SqliteStore, project: ProjectId) -> StoryNo {
    let story = store
        .write(|tx| tx.allocate_story_no(project))
        .expect("allocating a story number");
    append_and_fold(
        store,
        project,
        story,
        ExpectedSeq::Exact(EventSeq::ZERO),
        &[
            StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: FIXTURE_TITLE.into(),
                state: "todo".into(),
            },
            StoryEvent::StoryPrioritySet {
                at: "2026-01-01T00:01:00Z".into(),
                priority: Priority::Critical,
            },
            StoryEvent::StoryLabelsSet {
                at: "2026-01-01T00:02:00Z".into(),
                labels: vec!["infra".into()],
            },
        ],
    )
    .expect("seeding the fixture story");
    story
}

/// The `pub` field names of one struct, read out of a source file.
///
/// Deliberately dumb: the field list it is pointed at is a flat run of
/// `pub name: Type,` lines, and a parser cleverer than that would have more
/// ways to be quietly wrong. It panics rather than returning an empty set for
/// anything it does not recognise, so a failure here reads as "the scan broke"
/// rather than as "the tree is clean".
fn fields_of(source: &str, struct_name: &str) -> BTreeSet<String> {
    let header = format!("pub struct {struct_name} {{");
    let (_, body) = source
        .split_once(&header)
        .unwrap_or_else(|| panic!("`{header}` is not in this source — the scan proved nothing"));

    let mut fields = BTreeSet::new();
    for line in body.lines() {
        let line = line.trim();
        if line == "}" {
            assert!(
                !fields.is_empty(),
                "`{struct_name}` parsed as having no fields — the scan proved nothing"
            );
            return fields;
        }
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let Some((name, _)) = rest.split_once(':') else {
            continue;
        };
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            fields.insert(name.to_string());
        }
    }
    panic!("`{struct_name}`'s body never closed — the scan lost the struct");
}

/// Where `StoryRow` is defined.
fn types_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/store/types.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn the_field_parser_still_recognises_a_struct_it_is_shown() {
    // Assembled here rather than written as one literal so that this control
    // exercises the same line shapes `StoryRow` uses — a doc comment, an
    // attribute-free field, a generic type, a trailing comma — without
    // depending on `StoryRow` itself, which is the thing under test.
    let fixture = [
        "#[derive(Clone)]",
        "pub struct Fixture {",
        "    /// A documented field.",
        "    pub alpha: String,",
        "    pub beta: Option<String>,",
        "    pub gamma: Vec<u8>,",
        "}",
    ]
    .join("\n");

    let found = fields_of(&fixture, "Fixture");

    assert_eq!(
        found,
        ["alpha", "beta", "gamma"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>(),
        "the field parser no longer reads a struct it was shown, so its verdict \
         on `StoryRow` means nothing"
    );
}

#[test]
fn every_story_row_column_has_a_damage_case_or_a_stated_exemption() {
    let declared = fields_of(&types_source(), "StoryRow");

    let watched: BTreeSet<String> = CASES
        .iter()
        .flat_map(|case| case.fields.iter())
        .chain(EXEMPT.iter().map(|(field, _)| field))
        .map(|field| (*field).to_string())
        .collect();

    let unwatched: Vec<&String> = declared.difference(&watched).collect();
    assert!(
        unwatched.is_empty(),
        "`StoryRow` has {} column(s) that no damage case covers and no \
         exemption names: {unwatched:?}.\n\
         `store::rebuild::diff_rebuilt` is `story doctor`'s only oracle over \
         these columns, and a column nothing compares is a column a raw \
         migration or a hand edit can rewrite forever in silence (SH-211, \
         SH-365). Add a case to `CASES` in this file damaging the new column \
         underneath the store, or — if a schema constraint makes damaging it \
         alone impossible — add it to an existing case's `fields` and say which \
         constraint forces the coupling.",
        unwatched.len()
    );

    let stale: Vec<&String> = watched.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "this file watches {} name(s) `StoryRow` no longer declares: {stale:?}. \
         A case pointed at a column that no longer exists proves nothing and \
         will keep passing; delete it or repoint it.",
        stale.len()
    );
}

#[test]
fn every_coupled_case_says_which_constraint_forced_the_coupling() {
    for case in CASES {
        if case.fields.len() > 1 {
            assert!(
                case.coupled_because.is_some(),
                "`{}` damages {} columns at once but does not say why they \
                 cannot be damaged separately. An unjustified coupling is a \
                 weaker assertion pretending to be a stronger one (SH-364).",
                case.name,
                case.fields.len()
            );
        } else {
            assert!(
                case.coupled_because.is_none(),
                "`{}` damages one column but claims a coupling reason",
                case.name
            );
        }
    }
}

#[test]
fn the_fixture_diffs_clean_before_any_damage() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    seed_fixture_story(&store, project);

    let diff = diff_read_model(&store, project).unwrap();

    assert!(
        diff.is_clean(),
        "the fixture must be clean before it is damaged, or every case in this \
         file passes for the wrong reason: {}",
        diff.describe()
    );
}

#[test]
fn every_watched_column_is_named_by_the_oracle_when_it_is_damaged() {
    for case in CASES {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        seed_fixture_story(&store, project);

        raw(&store)
            .execute(case.sql, [])
            .unwrap_or_else(|e| panic!("`{}` could not damage the row: {e}", case.name));

        let diff = diff_read_model(&store, project).unwrap();
        let named: BTreeSet<&str> = diff
            .divergences
            .iter()
            .map(|divergence| divergence.field.as_str())
            .collect();

        for field in case.fields {
            assert!(
                named.contains(field),
                "`{}` damaged `{field}` underneath the store and \
                 `diff_rebuilt` did not report it — so `story doctor` is blind \
                 to that column. Reported instead: {named:?}",
                case.name
            );
        }
    }
}
