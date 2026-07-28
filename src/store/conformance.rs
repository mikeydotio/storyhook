//! The store conformance suite: what any `Store` implementation must do.
//!
//! The suite is a macro rather than a set of functions so that its bodies
//! expand into the *test* crate. A `macro_rules!` definition and the tiny
//! [`ConformanceFixture`] trait compile to no code at all, so nothing in this
//! file reaches a release binary — while a second implementation (the design
//! admits Postgres later) is put under the whole suite with one line:
//!
//! ```ignore
//! storyhook::store_conformance_suite!(PostgresFixture);
//! ```
//!
//! **There is no mock `Store`, and there must never be one.** A fake that
//! pretends to have transactions cannot fail the way a real one does, so a
//! service validated against it is validated against nothing — and the
//! split-brain between an event log and a read model is exactly what this
//! rearchitecture exists to end. Every case below runs against a real,
//! file-backed database.
//!
//! Deliberately *not* in the suite, because they are properties of an engine
//! rather than of the contract: schema migration and backup verification
//! (`tests/store_migrations.rs`), and damage injected beneath the store with
//! raw SQL (`tests/store_rebuild.rs`).

use crate::store::Store;

/// What the conformance suite needs from an implementation in order to test it.
///
/// `reopen` is the interesting one: durability claims are meaningless without a
/// way to close everything and look again, and an in-process fake could not
/// provide it honestly — which is another reason there is no mock.
pub trait ConformanceFixture: Sized {
    /// The implementation under test.
    type Store: Store;

    /// Builds an empty, migrated store on fresh backing storage.
    fn create() -> Self;

    /// The store under test.
    fn store(&self) -> &Self::Store;

    /// Closes this store and opens the same backing storage again.
    fn reopen(self) -> Self;
}

/// Generates the conformance suite for one [`ConformanceFixture`].
///
/// Invoke at most once per test file: the expansion defines a
/// `store_conformance` module, and two of them would collide.
#[macro_export]
macro_rules! store_conformance_suite {
    ($fixture:ty) => {
        mod store_conformance {
            #![allow(clippy::bool_assert_comparison)]

            use std::path::Path;

            use super::*;

            use $crate::domain::{
                Member, Priority, StateDef, StoryEvent, StorySnapshot, SuperState, TypeDef,
                fold_story,
            };
            use $crate::store::{
                ConformanceFixture, EventSeq, ExpectedSeq, GlobalSeq, NewProject, PathKind,
                ProjectId, ProjectSettings, RawEvent, ReadOps, Store, StoreError, StoredPayload,
                StoryNo, StoryQuery, StorySort, WriteOps, partition_known,
            };

            type Subject = <$fixture as ConformanceFixture>::Store;

            // ---------------------------------------------------------------
            // Fixture helpers
            // ---------------------------------------------------------------

            fn states() -> Vec<StateDef> {
                vec![
                    StateDef {
                        slug: "todo".into(),
                        super_state: SuperState::Open,
                        role: None,
                        description: Some("Not started".into()),
                    },
                    StateDef {
                        slug: "in-progress".into(),
                        super_state: SuperState::Open,
                        role: Some("active".into()),
                        description: None,
                    },
                    StateDef {
                        slug: "done".into(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: Some("Finished".into()),
                    },
                ]
            }

            fn types() -> Vec<TypeDef> {
                vec![
                    TypeDef {
                        slug: "feature".into(),
                        description: None,
                    },
                    TypeDef {
                        slug: "bug".into(),
                        description: Some("Something is broken".into()),
                    },
                ]
            }

            fn member() -> Member {
                Member {
                    id: "ada".into(),
                    display_name: "Ada Lovelace".into(),
                    email: Some("ada@example.com".into()),
                    github: Some("ada".into()),
                    created_at: "2026-01-01T00:00:00Z".into(),
                }
            }

            fn seed(store: &Subject, slug: &str, prefix: &str) -> ProjectId {
                store
                    .write(|tx| {
                        let project = tx.create_project(&NewProject {
                            uuid: format!("uuid-{slug}"),
                            slug: slug.to_string(),
                            name: format!("Project {slug}"),
                            prefix: prefix.to_string(),
                            created_at: "2026-01-01T00:00:00Z".into(),
                        })?;
                        tx.put_states(project, &states())?;
                        tx.put_types(project, &types())?;
                        Ok(project)
                    })
                    .expect("seeding a project")
            }

            /// Appends events and writes the folded result back, in one
            /// transaction — the pattern every caller of this store follows.
            fn apply(
                store: &Subject,
                project: ProjectId,
                story: StoryNo,
                expected: ExpectedSeq,
                events: &[StoryEvent],
            ) -> Result<EventSeq, StoreError> {
                store.write(|tx| {
                    let head = tx.append_events(project, story, expected, events)?;
                    let prefix = tx.project(project)?.expect("the project exists").prefix;
                    let state_map = tx.state_map(project)?;
                    let stored = tx.events_for(project, story)?;
                    let (known, _) = partition_known(story, &stored);
                    let snapshot = fold_story(&story.to_id(&prefix), &known, &state_map)
                        .map_err(|e| StoreError::Invariant(e.to_string()))?;
                    tx.put_story(project, &snapshot, head)?;
                    Ok(head)
                })
            }

            fn created(title: &str, at: &str) -> StoryEvent {
                StoryEvent::StoryCreated {
                    at: at.to_string(),
                    title: title.to_string(),
                    state: "todo".into(),
                }
            }

            fn new_story(store: &Subject, project: ProjectId, title: &str) -> StoryNo {
                let story = store
                    .write(|tx| tx.allocate_story_no(project))
                    .expect("allocating a story number");
                apply(
                    store,
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &[created(title, "2026-01-01T00:00:00Z")],
                )
                .expect("creating a story");
                story
            }

            fn snapshot(store: &Subject, project: ProjectId, story: StoryNo) -> StorySnapshot {
                store
                    .read(|tx| tx.story(project, story))
                    .expect("reading a story")
                    .expect("the story exists")
                    .snapshot
            }

            fn story_numbers(store: &Subject, project: ProjectId, query: &StoryQuery) -> Vec<i64> {
                store
                    .read(|tx| tx.stories(project, query))
                    .expect("querying stories")
                    .into_iter()
                    .map(|row| row.story_no.get())
                    .collect()
            }

            /// Adds both sides of an edge the way a service would: the event on
            /// each story, then the folded result of each.
            fn link(
                store: &Subject,
                project: ProjectId,
                from: StoryNo,
                relation: &str,
                to: StoryNo,
            ) -> Result<(), StoreError> {
                let inverse = $crate::domain::inverse_relation(relation).expect("a known relation");
                apply(
                    store,
                    project,
                    from,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: to.to_id("SH"),
                        relation: relation.to_string(),
                    }],
                )?;
                apply(
                    store,
                    project,
                    to,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: from.to_id("SH"),
                        relation: inverse.to_string(),
                    }],
                )?;
                Ok(())
            }

            /// Two projects that share a prefix and both have stories 1 and 2.
            ///
            /// The shape every isolation case needs: in a single global
            /// database where every repository defaults to `SH`, a query that
            /// forgets its scope returns the *other* project's story rather
            /// than failing.
            fn twin_projects(store: &Subject) -> (ProjectId, ProjectId) {
                let alpha = seed(store, "alpha", "SH");
                let beta = seed(store, "beta", "SH");
                for (project, label) in [(alpha, "alpha"), (beta, "beta")] {
                    for index in 1..=2 {
                        new_story(store, project, &format!("{label} story {index}"));
                    }
                }
                (alpha, beta)
            }

            // ===============================================================
            // Projects and identity
            // ===============================================================

            #[test]
            fn a_created_project_is_findable_by_its_uuid() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let found = f.store().read(|tx| tx.project_by_uuid("uuid-alpha")).unwrap();
                assert_eq!(found.unwrap().id, project);
            }

            #[test]
            fn a_created_project_is_findable_by_its_slug() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let found = f.store().read(|tx| tx.project_by_slug("alpha")).unwrap();
                assert_eq!(found.unwrap().id, project);
            }

            #[test]
            fn a_created_project_is_findable_by_its_id() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let found = f.store().read(|tx| tx.project(project)).unwrap().unwrap();
                assert_eq!(found.slug, "alpha");
                assert_eq!(found.name, "Project alpha");
                assert_eq!(found.prefix, "SH");
                assert_eq!(found.created_at, "2026-01-01T00:00:00Z");
            }

            #[test]
            fn projects_are_listed_in_slug_order() {
                let f = <$fixture>::create();
                seed(f.store(), "zulu", "SH");
                seed(f.store(), "alpha", "SH");
                seed(f.store(), "mike", "SH");
                let slugs: Vec<String> = f
                    .store()
                    .read(|tx| tx.projects())
                    .unwrap()
                    .into_iter()
                    .map(|p| p.slug)
                    .collect();
                assert_eq!(slugs, ["alpha", "mike", "zulu"]);
            }

            #[test]
            fn an_unknown_uuid_is_none_not_an_error() {
                let f = <$fixture>::create();
                assert!(
                    f.store()
                        .read(|tx| tx.project_by_uuid("nobody"))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn an_unknown_slug_is_none_not_an_error() {
                let f = <$fixture>::create();
                assert!(
                    f.store()
                        .read(|tx| tx.project_by_slug("nobody"))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn an_unknown_project_id_is_none_not_an_error() {
                let f = <$fixture>::create();
                assert!(
                    f.store()
                        .read(|tx| tx.project(ProjectId::new(404)))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn a_duplicate_uuid_is_rejected_by_name() {
                let f = <$fixture>::create();
                seed(f.store(), "alpha", "SH");
                let error = f
                    .store()
                    .write(|tx| {
                        tx.create_project(&NewProject {
                            uuid: "uuid-alpha".into(),
                            slug: "different".into(),
                            name: "n".into(),
                            prefix: "SH".into(),
                            created_at: "2026-01-01T00:00:00Z".into(),
                        })
                    })
                    .unwrap_err();
                assert!(error.to_string().contains("uuid"), "{error}");
            }

            #[test]
            fn a_duplicate_slug_is_rejected_by_name() {
                let f = <$fixture>::create();
                seed(f.store(), "alpha", "SH");
                let error = f
                    .store()
                    .write(|tx| {
                        tx.create_project(&NewProject {
                            uuid: "uuid-different".into(),
                            slug: "alpha".into(),
                            name: "n".into(),
                            prefix: "SH".into(),
                            created_at: "2026-01-01T00:00:00Z".into(),
                        })
                    })
                    .unwrap_err();
                assert!(error.to_string().contains("slug"), "{error}");
            }

            #[test]
            fn an_empty_prefix_is_rejected() {
                let f = <$fixture>::create();
                let error = f
                    .store()
                    .write(|tx| {
                        tx.create_project(&NewProject {
                            uuid: "u".into(),
                            slug: "s".into(),
                            name: "n".into(),
                            prefix: String::new(),
                            created_at: "2026-01-01T00:00:00Z".into(),
                        })
                    })
                    .unwrap_err();
                assert!(matches!(error, StoreError::Validation(_)), "{error}");
            }

            #[test]
            fn a_new_projects_counters_start_at_one() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let record = f.store().read(|tx| tx.project(project)).unwrap().unwrap();
                assert_eq!(record.next_story_no, 1);
                assert_eq!(record.next_global_seq, 1);
            }

            #[test]
            fn two_projects_may_share_a_prefix() {
                let f = <$fixture>::create();
                let alpha = seed(f.store(), "alpha", "SH");
                let beta = seed(f.store(), "beta", "SH");
                assert_ne!(alpha, beta);
            }

            #[test]
            fn a_rejected_project_creation_leaves_nothing_behind() {
                let f = <$fixture>::create();
                let _ = f.store().write(|tx| {
                    tx.create_project(&NewProject {
                        uuid: "u".into(),
                        slug: "s".into(),
                        name: "n".into(),
                        prefix: String::new(),
                        created_at: "2026-01-01T00:00:00Z".into(),
                    })
                });
                assert!(f.store().read(|tx| tx.projects()).unwrap().is_empty());
            }

            #[test]
            fn migrating_an_already_current_store_is_a_no_op() {
                let f = <$fixture>::create();
                assert!(f.store().migrate().unwrap().is_noop());
            }

            // ===============================================================
            // Project paths — one project, many checkouts (SH-46)
            // ===============================================================

            #[test]
            fn a_registered_path_resolves_to_its_project() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(project, Path::new("/repos/alpha"), PathKind::Main)
                    })
                    .unwrap();
                let found = f
                    .store()
                    .read(|tx| tx.project_by_path(Path::new("/repos/alpha")))
                    .unwrap();
                assert_eq!(found.unwrap().id, project);
            }

            #[test]
            fn an_unregistered_path_resolves_to_nothing() {
                let f = <$fixture>::create();
                seed(f.store(), "alpha", "SH");
                assert!(
                    f.store()
                        .read(|tx| tx.project_by_path(Path::new("/elsewhere")))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn every_checkout_of_a_repository_resolves_to_the_same_project() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(project, Path::new("/repos/alpha"), PathKind::Main)?;
                        tx.touch_project_path(
                            project,
                            Path::new("/repos/alpha/.worktrees/a"),
                            PathKind::Worktree,
                        )?;
                        tx.touch_project_path(
                            project,
                            Path::new("/repos/alpha/.worktrees/b"),
                            PathKind::Worktree,
                        )
                    })
                    .unwrap();
                // The whole of SH-46: under the old design each of these was an
                // independent, silently divergent tracker.
                for path in [
                    "/repos/alpha",
                    "/repos/alpha/.worktrees/a",
                    "/repos/alpha/.worktrees/b",
                ] {
                    let found = f
                        .store()
                        .read(|tx| tx.project_by_path(Path::new(path)))
                        .unwrap();
                    assert_eq!(found.unwrap().id, project, "{path}");
                }
            }

            #[test]
            fn touching_a_path_twice_records_it_once() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                for _ in 0..3 {
                    f.store()
                        .write(|tx| {
                            tx.touch_project_path(
                                project,
                                Path::new("/repos/alpha"),
                                PathKind::Main,
                            )
                        })
                        .unwrap();
                }
                assert_eq!(
                    f.store().read(|tx| tx.project_paths(project)).unwrap().len(),
                    1
                );
            }

            #[test]
            fn touching_a_path_can_change_its_kind() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(
                            project,
                            Path::new("/repos/alpha"),
                            PathKind::Worktree,
                        )
                    })
                    .unwrap();
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(project, Path::new("/repos/alpha"), PathKind::Main)
                    })
                    .unwrap();
                let paths = f.store().read(|tx| tx.project_paths(project)).unwrap();
                assert_eq!(paths[0].kind, PathKind::Main);
            }

            #[test]
            fn a_path_already_claimed_by_another_project_is_refused() {
                let f = <$fixture>::create();
                let alpha = seed(f.store(), "alpha", "SH");
                let beta = seed(f.store(), "beta", "SH");
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(alpha, Path::new("/repos/shared"), PathKind::Main)
                    })
                    .unwrap();
                let error = f
                    .store()
                    .write(|tx| {
                        tx.touch_project_path(beta, Path::new("/repos/shared"), PathKind::Main)
                    })
                    .unwrap_err();
                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_path(Path::new("/repos/shared")))
                        .unwrap()
                        .unwrap()
                        .id,
                    alpha
                );
            }

            #[test]
            fn project_paths_are_listed_in_path_order() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(project, Path::new("/c"), PathKind::Worktree)?;
                        tx.touch_project_path(project, Path::new("/a"), PathKind::Main)?;
                        tx.touch_project_path(project, Path::new("/b"), PathKind::Worktree)
                    })
                    .unwrap();
                let paths: Vec<String> = f
                    .store()
                    .read(|tx| tx.project_paths(project))
                    .unwrap()
                    .into_iter()
                    .map(|p| p.path)
                    .collect();
                assert_eq!(paths, ["/a", "/b", "/c"]);
            }

            #[test]
            fn a_recorded_path_carries_a_last_seen_timestamp() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| tx.touch_project_path(project, Path::new("/a"), PathKind::Main))
                    .unwrap();
                let paths = f.store().read(|tx| tx.project_paths(project)).unwrap();
                assert!(paths[0].last_seen_at.ends_with('Z'), "{:?}", paths[0]);
            }

            #[test]
            fn a_project_with_no_checkouts_has_no_paths() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert!(
                    f.store()
                        .read(|tx| tx.project_paths(project))
                        .unwrap()
                        .is_empty()
                );
            }

            // ===============================================================
            // Story number allocation
            // ===============================================================

            #[test]
            fn the_first_story_number_is_one() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(project)).unwrap(),
                    StoryNo::new(1)
                );
            }

            #[test]
            fn story_numbers_are_handed_out_in_sequence() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let numbers: Vec<i64> = (0..5)
                    .map(|_| {
                        f.store()
                            .write(|tx| tx.allocate_story_no(project))
                            .unwrap()
                            .get()
                    })
                    .collect();
                assert_eq!(numbers, [1, 2, 3, 4, 5]);
            }

            #[test]
            fn many_allocations_in_one_transaction_are_all_distinct() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let numbers = f
                    .store()
                    .write(|tx| {
                        (0..5)
                            .map(|_| tx.allocate_story_no(project).map(StoryNo::get))
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap();
                assert_eq!(numbers, [1, 2, 3, 4, 5]);
            }

            #[test]
            fn a_rolled_back_allocation_returns_the_number_to_the_pool() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let _ = f.store().write(|tx| {
                    tx.allocate_story_no(project)?;
                    Err::<(), _>(StoreError::Validation("changed my mind".into()))
                });
                // No gap: the counter moved inside the transaction that failed,
                // so it moved back with it.
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(project)).unwrap(),
                    StoryNo::new(1)
                );
            }

            #[test]
            fn allocation_is_independent_per_project() {
                let f = <$fixture>::create();
                let alpha = seed(f.store(), "alpha", "SH");
                let beta = seed(f.store(), "beta", "SH");
                f.store().write(|tx| tx.allocate_story_no(alpha)).unwrap();
                f.store().write(|tx| tx.allocate_story_no(alpha)).unwrap();
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(beta)).unwrap(),
                    StoryNo::new(1)
                );
            }

            #[test]
            fn allocating_for_an_unknown_project_says_so() {
                let f = <$fixture>::create();
                let error = f
                    .store()
                    .write(|tx| tx.allocate_story_no(ProjectId::new(404)))
                    .unwrap_err();
                assert!(matches!(error, StoreError::NotFound(_)), "{error}");
            }

            #[test]
            fn the_allocation_counter_is_visible_on_the_project_record() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store().write(|tx| tx.allocate_story_no(project)).unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project(project))
                        .unwrap()
                        .unwrap()
                        .next_story_no,
                    2
                );
            }

            /// The headline guarantee. Under the old design the counter was a
            /// file inside the repository, so two checkouts each read `49` and
            /// each wrote `50` — twice producing two different stories both
            /// called `SH-49`, and once forcing a hand-reconciled merge.
            #[test]
            fn concurrent_writers_never_receive_the_same_story_number() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let store = f.store();

                let allocated: Vec<i64> = std::thread::scope(|scope| {
                    let handles: Vec<_> = (0..8)
                        .map(|_| {
                            scope.spawn(move || {
                                (0..20)
                                    .map(|_| {
                                        store
                                            .write(|tx| tx.allocate_story_no(project))
                                            .unwrap()
                                            .get()
                                    })
                                    .collect::<Vec<_>>()
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap())
                        .collect()
                });

                let mut sorted = allocated.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(
                    sorted.len(),
                    allocated.len(),
                    "160 concurrent allocations produced a duplicate"
                );
                assert_eq!(sorted.first().copied(), Some(1));
                assert_eq!(sorted.last().copied(), Some(160), "and no gaps");
            }

            // ===============================================================
            // Appending events
            // ===============================================================

            #[test]
            fn the_first_append_lands_at_sequence_one() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let head = f
                    .store()
                    .write(|tx| {
                        tx.append_events(
                            project,
                            StoryNo::new(1),
                            ExpectedSeq::Any,
                            &[created("First", "2026-01-01T00:00:00Z")],
                        )
                    })
                    .unwrap();
                assert_eq!(head, EventSeq::new(1));
            }

            #[test]
            fn a_batch_append_assigns_consecutive_sequences() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[
                        StoryEvent::StoryTitleSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            title: "Second".into(),
                        },
                        StoryEvent::StoryTitleSet {
                            at: "2026-01-01T00:02:00Z".into(),
                            title: "Third".into(),
                        },
                    ],
                )
                .unwrap();
                let seqs: Vec<i64> = f
                    .store()
                    .read(|tx| tx.events_for(project, story))
                    .unwrap()
                    .into_iter()
                    .map(|e| e.seq.get())
                    .collect();
                assert_eq!(seqs, [1, 2, 3]);
            }

            #[test]
            fn events_come_back_in_the_order_they_were_appended() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                for index in 1..=4 {
                    apply(
                        f.store(),
                        project,
                        story,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryTitleSet {
                            at: format!("2026-01-01T00:0{index}:00Z"),
                            title: format!("Title {index}"),
                        }],
                    )
                    .unwrap();
                }
                let titles: Vec<String> = f
                    .store()
                    .read(|tx| tx.events_for(project, story))
                    .unwrap()
                    .into_iter()
                    .filter_map(|e| match e.known() {
                        Some(StoryEvent::StoryTitleSet { title, .. }) => Some(title.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(titles, ["Title 1", "Title 2", "Title 3", "Title 4"]);
            }

            #[test]
            fn an_empty_append_is_a_no_op_that_reports_the_head() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let head = f
                    .store()
                    .write(|tx| tx.append_events(project, story, ExpectedSeq::Any, &[]))
                    .unwrap();
                assert_eq!(head, EventSeq::new(1));
                assert_eq!(
                    f.store().read(|tx| tx.events_for(project, story)).unwrap().len(),
                    1
                );
            }

            #[test]
            fn a_story_with_no_events_has_head_zero() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert_eq!(
                    f.store()
                        .read(|tx| tx.head_seq(project, StoryNo::new(9)))
                        .unwrap(),
                    EventSeq::ZERO
                );
            }

            #[test]
            fn the_head_tracks_the_last_appended_sequence() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryAwaitingSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        awaiting: "review".into(),
                    }],
                )
                .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.head_seq(project, story)).unwrap(),
                    EventSeq::new(2)
                );
            }

            #[test]
            fn every_event_carries_its_kind_without_the_payload_being_understood() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                assert_eq!(events[0].kind, "StoryCreated");
            }

            #[test]
            fn every_event_carries_its_own_timestamp() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                assert_eq!(events[0].at, "2026-01-01T00:00:00Z");
            }

            #[test]
            fn an_event_payload_round_trips_exactly() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let original = StoryEvent::StoryCommentAdded {
                    at: "2026-01-01T00:01:00Z".into(),
                    text: "unicode ✓, quotes \" and \\ backslashes".into(),
                };
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    std::slice::from_ref(&original),
                )
                .unwrap();
                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                assert_eq!(events[1].known(), Some(&original));
            }

            #[test]
            fn every_event_variant_survives_the_round_trip() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let corpus = vec![
                    StoryEvent::StoryCommentAdded {
                        at: "2026-01-01T00:01:00Z".into(),
                        text: "a comment".into(),
                    },
                    StoryEvent::StoryAssigned {
                        at: "2026-01-01T00:02:00Z".into(),
                        member_id: "ada".into(),
                    },
                    StoryEvent::StoryAwaitingSet {
                        at: "2026-01-01T00:03:00Z".into(),
                        awaiting: "review".into(),
                    },
                    StoryEvent::StoryAwaitingCleared {
                        at: "2026-01-01T00:04:00Z".into(),
                    },
                    StoryEvent::StoryStateChanged {
                        at: "2026-01-01T00:05:00Z".into(),
                        state: "in-progress".into(),
                    },
                    StoryEvent::StoryPrioritySet {
                        at: "2026-01-01T00:06:00Z".into(),
                        priority: Priority::High,
                    },
                    StoryEvent::StoryTypeSet {
                        at: "2026-01-01T00:07:00Z".into(),
                        story_type: "bug".into(),
                    },
                    StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:08:00Z".into(),
                        labels: vec!["a".into(), "b".into()],
                    },
                    StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:09:00Z".into(),
                        title: "Renamed".into(),
                    },
                    StoryEvent::StoryDescriptionSet {
                        at: "2026-01-01T00:10:00Z".into(),
                        description: "Long\nmultiline\ntext".into(),
                    },
                ];
                apply(f.store(), project, story, ExpectedSeq::Any, &corpus).unwrap();
                let stored = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                let (known, unknown) = partition_known(story, &stored);
                assert!(unknown.is_empty());
                assert_eq!(&known[1..], &corpus[..]);
            }

            #[test]
            fn relationship_events_round_trip() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                let added = StoryEvent::StoryRelationshipAdded {
                    at: "2026-01-01T00:01:00Z".into(),
                    other_id: two.to_id("SH"),
                    relation: "relates-to".into(),
                };
                let removed = StoryEvent::StoryRelationshipRemoved {
                    at: "2026-01-01T00:02:00Z".into(),
                    other_id: two.to_id("SH"),
                    relation: "relates-to".into(),
                };
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[added.clone(), removed.clone()],
                )
                .unwrap();
                let stored = f.store().read(|tx| tx.events_for(project, one)).unwrap();
                assert_eq!(stored[1].known(), Some(&added));
                assert_eq!(stored[2].known(), Some(&removed));
            }

            #[test]
            fn terminal_events_round_trip() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryClosedAndArchived {
                        at: "2026-01-01T00:01:00Z".into(),
                        state: "done".into(),
                    }],
                )
                .unwrap();
                apply(
                    f.store(),
                    project,
                    two,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryDeleted {
                        at: "2026-01-01T00:01:00Z".into(),
                        reason: "duplicate".into(),
                    }],
                )
                .unwrap();
                assert_eq!(snapshot(f.store(), project, one).state, "done");
                assert_eq!(
                    snapshot(f.store(), project, two).deleted_reason.as_deref(),
                    Some("duplicate")
                );
            }

            // ===============================================================
            // The change feed
            // ===============================================================

            #[test]
            fn the_change_feed_spans_every_story_in_the_project() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                let feed = f
                    .store()
                    .read(|tx| tx.events_since(project, GlobalSeq::ZERO, 100))
                    .unwrap();
                assert_eq!(
                    feed.iter().map(|e| e.story_no.get()).collect::<Vec<_>>(),
                    [1, 2]
                );
            }

            #[test]
            fn change_feed_positions_are_strictly_increasing() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:03:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                let positions: Vec<i64> = f
                    .store()
                    .read(|tx| tx.events_since(project, GlobalSeq::ZERO, 100))
                    .unwrap()
                    .into_iter()
                    .map(|e| e.event.global_seq.get())
                    .collect();
                assert_eq!(positions, [1, 2, 3]);
            }

            #[test]
            fn the_change_feed_resumes_from_a_position() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                new_story(f.store(), project, "Third");
                let feed = f
                    .store()
                    .read(|tx| tx.events_since(project, GlobalSeq::new(1), 100))
                    .unwrap();
                assert_eq!(
                    feed.iter().map(|e| e.story_no.get()).collect::<Vec<_>>(),
                    [2, 3]
                );
            }

            #[test]
            fn the_change_feed_honours_its_limit() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                for index in 1..=5 {
                    new_story(f.store(), project, &format!("Story {index}"));
                }
                let feed = f
                    .store()
                    .read(|tx| tx.events_since(project, GlobalSeq::ZERO, 2))
                    .unwrap();
                assert_eq!(feed.len(), 2);
            }

            #[test]
            fn the_change_feed_of_a_new_project_is_empty() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert!(
                    f.store()
                        .read(|tx| tx.events_since(project, GlobalSeq::ZERO, 100))
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    f.store().read(|tx| tx.max_global_seq(project)).unwrap(),
                    GlobalSeq::ZERO
                );
            }

            #[test]
            fn the_change_feed_head_is_the_newest_position() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                assert_eq!(
                    f.store().read(|tx| tx.max_global_seq(project)).unwrap(),
                    GlobalSeq::new(2)
                );
            }

            // ===============================================================
            // Compare-and-swap
            // ===============================================================

            #[test]
            fn expecting_zero_succeeds_on_a_story_that_does_not_exist_yet() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert!(
                    f.store()
                        .write(|tx| {
                            tx.append_events(
                                project,
                                StoryNo::new(1),
                                ExpectedSeq::Exact(EventSeq::ZERO),
                                &[created("First", "2026-01-01T00:00:00Z")],
                            )
                        })
                        .is_ok()
                );
            }

            #[test]
            fn expecting_zero_conflicts_on_a_story_that_already_exists() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let error = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &[created("Again", "2026-01-01T00:01:00Z")],
                )
                .unwrap_err();
                assert!(matches!(error, StoreError::Conflict { .. }), "{error}");
            }

            #[test]
            fn a_conflict_reports_both_what_was_expected_and_what_is_there() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                let error = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::new(1)),
                    &[created("Nope", "2026-01-01T00:02:00Z")],
                )
                .unwrap_err();
                match error {
                    StoreError::Conflict { expected, actual } => {
                        assert_eq!(expected, ExpectedSeq::Exact(EventSeq::new(1)));
                        assert_eq!(actual, EventSeq::new(2));
                    }
                    other => panic!("expected a Conflict, got {other}"),
                }
            }

            #[test]
            fn expecting_the_current_head_succeeds() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let head = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::new(1)),
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                assert_eq!(head, EventSeq::new(2));
            }

            #[test]
            fn expecting_anything_never_conflicts() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                for index in 1..=3 {
                    apply(
                        f.store(),
                        project,
                        story,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryTitleSet {
                            at: format!("2026-01-01T00:0{index}:00Z"),
                            title: format!("Title {index}"),
                        }],
                    )
                    .unwrap();
                }
                assert_eq!(
                    f.store().read(|tx| tx.head_seq(project, story)).unwrap(),
                    EventSeq::new(4)
                );
            }

            #[test]
            fn a_lost_compare_and_swap_writes_nothing_at_all() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let before = f.store().read(|tx| tx.project(project)).unwrap().unwrap();

                let _ = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::new(99)),
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Never".into(),
                    }],
                );

                assert_eq!(
                    f.store().read(|tx| tx.events_for(project, story)).unwrap().len(),
                    1
                );
                let after = f.store().read(|tx| tx.project(project)).unwrap().unwrap();
                assert_eq!(
                    after.next_global_seq, before.next_global_seq,
                    "a lost claim must not consume a change-feed position"
                );
                assert_eq!(snapshot(f.store(), project, story).title, "First");
            }

            #[test]
            fn only_the_first_of_two_claims_at_the_same_head_wins() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let head = f.store().read(|tx| tx.head_seq(project, story)).unwrap();

                let first = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(head),
                    &[StoryEvent::StoryStateChanged {
                        at: "2026-01-01T00:01:00Z".into(),
                        state: "in-progress".into(),
                    }],
                );
                let second = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(head),
                    &[StoryEvent::StoryStateChanged {
                        at: "2026-01-01T00:02:00Z".into(),
                        state: "done".into(),
                    }],
                );

                assert!(first.is_ok());
                assert!(
                    matches!(second, Err(StoreError::Conflict { .. })),
                    "{second:?}"
                );
                assert_eq!(snapshot(f.store(), project, story).state, "in-progress");
            }

            #[test]
            fn a_conflict_becomes_a_state_conflict_for_the_application() {
                use $crate::error::AppError;
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let error = apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Exact(EventSeq::new(7)),
                    &[created("Nope", "2026-01-01T00:01:00Z")],
                )
                .unwrap_err();
                let app = AppError::from(error);
                // Exit code 9 and HTTP 409, the contract today's clients read.
                assert_eq!(app.exit_code(), 9);
                assert!(matches!(app, AppError::StateConflict(..)));
            }

            #[test]
            fn compare_and_swap_is_scoped_to_one_story() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                // SH-1 moved; SH-2's head did not, so a claim against it holds.
                assert!(
                    apply(
                        f.store(),
                        project,
                        two,
                        ExpectedSeq::Exact(EventSeq::new(1)),
                        &[StoryEvent::StoryTitleSet {
                            at: "2026-01-01T00:02:00Z".into(),
                            title: "Also renamed".into(),
                        }],
                    )
                    .is_ok()
                );
            }

            // ===============================================================
            // The read model
            // ===============================================================

            #[test]
            fn a_written_story_reads_back() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.story_no, story);
                assert_eq!(row.title, "First");
                assert_eq!(row.state, "todo");
                assert_eq!(row.superstate, SuperState::Open);
                assert_eq!(row.head_seq, EventSeq::new(1));
            }

            #[test]
            fn an_unwritten_story_reads_back_as_nothing() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert!(
                    f.store()
                        .read(|tx| tx.story(project, StoryNo::new(7)))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn the_snapshot_round_trips_field_for_field() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[
                        StoryEvent::StoryDescriptionSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            description: "A description\nwith newlines".into(),
                        },
                        StoryEvent::StoryAssigned {
                            at: "2026-01-01T00:02:00Z".into(),
                            member_id: "ada".into(),
                        },
                        StoryEvent::StoryAwaitingSet {
                            at: "2026-01-01T00:03:00Z".into(),
                            awaiting: "review".into(),
                        },
                        StoryEvent::StoryTypeSet {
                            at: "2026-01-01T00:04:00Z".into(),
                            story_type: "bug".into(),
                        },
                        StoryEvent::StoryPrioritySet {
                            at: "2026-01-01T00:05:00Z".into(),
                            priority: Priority::Critical,
                        },
                        StoryEvent::StoryCommentAdded {
                            at: "2026-01-01T00:06:00Z".into(),
                            text: "a comment nothing indexes".into(),
                        },
                    ],
                )
                .unwrap();

                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    row.description.as_deref(),
                    Some("A description\nwith newlines")
                );
                assert_eq!(row.assignee.as_deref(), Some("ada"));
                assert_eq!(row.awaiting.as_deref(), Some("review"));
                assert_eq!(row.story_type.as_deref(), Some("bug"));
                assert_eq!(row.priority, Priority::Critical);
                assert_eq!(row.created_at, "2026-01-01T00:00:00Z");
                assert_eq!(row.updated_at, "2026-01-01T00:06:00Z");
                // The comment has no column; it survives in the snapshot.
                assert_eq!(row.snapshot.comments.len(), 1);
                assert_eq!(row.snapshot.comments[0].text, "a comment nothing indexes");
            }

            #[test]
            fn the_columns_agree_with_the_snapshot_they_were_taken_from() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.title, row.snapshot.title);
                assert_eq!(row.state, row.snapshot.state);
                assert_eq!(row.superstate, row.snapshot.superstate);
                assert_eq!(row.priority, row.snapshot.priority);
                assert_eq!(row.created_at, row.snapshot.created_at);
                assert_eq!(row.updated_at, row.snapshot.updated_at);
            }

            #[test]
            fn writing_a_story_twice_replaces_rather_than_duplicates() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                assert_eq!(story_numbers(f.store(), project, &StoryQuery::all()), [1]);
                assert_eq!(snapshot(f.store(), project, story).title, "Renamed");
            }

            #[test]
            fn labels_are_stored_and_read_back_sorted() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        labels: vec!["zebra".into(), "apple".into(), "mango".into()],
                    }],
                )
                .unwrap();
                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.labels, ["apple", "mango", "zebra"]);
            }

            #[test]
            fn a_repeated_label_is_stored_once() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        labels: vec!["dup".into(), "dup".into()],
                    }],
                )
                .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.story(project, story))
                        .unwrap()
                        .unwrap()
                        .labels,
                    ["dup"]
                );
            }

            #[test]
            fn replacing_a_label_set_removes_the_ones_that_went() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                for labels in [vec!["a".to_string(), "b".to_string()], vec!["b".to_string()]] {
                    apply(
                        f.store(),
                        project,
                        story,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryLabelsSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            labels,
                        }],
                    )
                    .unwrap();
                }
                assert_eq!(
                    f.store()
                        .read(|tx| tx.story(project, story))
                        .unwrap()
                        .unwrap()
                        .labels,
                    ["b"]
                );
            }

            /// The `archived` flag replaces the legacy split between
            /// `open/stories/*.jsonl` and `archive/archive.db` — two storage
            /// media that could, and did, disagree.
            #[test]
            fn a_story_is_archived_exactly_when_it_has_been_closed() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                assert_eq!(
                    f.store()
                        .read(|tx| tx.story(project, story))
                        .unwrap()
                        .unwrap()
                        .archived,
                    false
                );

                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryClosedAndArchived {
                        at: "2026-01-01T00:01:00Z".into(),
                        state: "done".into(),
                    }],
                )
                .unwrap();

                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.archived, true);
                assert_eq!(row.closed_at.as_deref(), Some("2026-01-01T00:01:00Z"));
                assert_eq!(row.superstate, SuperState::Closed);
            }

            #[test]
            fn a_deleted_story_is_marked_deleted_and_archived() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryDeleted {
                        at: "2026-01-01T00:01:00Z".into(),
                        reason: "a mistake".into(),
                    }],
                )
                .unwrap();
                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.deleted, true);
                assert_eq!(row.archived, true);
                assert_eq!(row.superstate, SuperState::Closed);
            }

            #[test]
            fn a_story_row_records_the_event_it_was_folded_from() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[
                        StoryEvent::StoryTitleSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            title: "Two".into(),
                        },
                        StoryEvent::StoryTitleSet {
                            at: "2026-01-01T00:02:00Z".into(),
                            title: "Three".into(),
                        },
                    ],
                )
                .unwrap();
                let row = f
                    .store()
                    .read(|tx| tx.story(project, story))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.head_seq, EventSeq::new(3));
                assert_eq!(
                    row.head_seq,
                    f.store().read(|tx| tx.head_seq(project, story)).unwrap()
                );
            }

            #[test]
            fn a_snapshot_whose_id_belongs_to_another_prefix_is_refused() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let mut foreign = snapshot(f.store(), project, story);
                foreign.id = "OTHER-1".into();
                let error = f
                    .store()
                    .write(|tx| tx.put_story(project, &foreign, EventSeq::new(1)))
                    .unwrap_err();
                assert!(matches!(error, StoreError::Validation(_)), "{error}");
            }

            /// Everything a write did is visible to reads *within that same
            /// transaction*. Without it, "fold the events you just appended"
            /// would be reading a world one step out of date, which is how a
            /// read model and its events drift apart.
            #[test]
            fn a_write_sees_its_own_effects_before_it_commits() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| {
                        let story = tx.allocate_story_no(project)?;
                        let head = tx.append_events(
                            project,
                            story,
                            ExpectedSeq::Exact(EventSeq::ZERO),
                            &[created("First", "2026-01-01T00:00:00Z")],
                        )?;
                        assert_eq!(tx.head_seq(project, story)?, head);
                        assert_eq!(tx.events_for(project, story)?.len(), 1);

                        let state_map = tx.state_map(project)?;
                        let stored = tx.events_for(project, story)?;
                        let (known, _) = partition_known(story, &stored);
                        let snap = fold_story(&story.to_id("SH"), &known, &state_map)
                            .map_err(|e| StoreError::Invariant(e.to_string()))?;
                        tx.put_story(project, &snap, head)?;

                        assert_eq!(tx.story(project, story)?.unwrap().title, "First");
                        assert_eq!(tx.stories(project, &StoryQuery::all())?.len(), 1);
                        Ok(())
                    })
                    .unwrap();
            }

            #[test]
            fn a_failed_write_leaves_neither_the_events_nor_the_row() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let result = f.store().write(|tx| {
                    let story = tx.allocate_story_no(project)?;
                    let head = tx.append_events(
                        project,
                        story,
                        ExpectedSeq::Exact(EventSeq::ZERO),
                        &[created("First", "2026-01-01T00:00:00Z")],
                    )?;
                    let state_map = tx.state_map(project)?;
                    let stored = tx.events_for(project, story)?;
                    let (known, _) = partition_known(story, &stored);
                    let snap = fold_story(&story.to_id("SH"), &known, &state_map)
                        .map_err(|e| StoreError::Invariant(e.to_string()))?;
                    tx.put_story(project, &snap, head)?;
                    Err::<(), _>(StoreError::Validation("second thoughts".into()))
                });

                assert!(result.is_err());
                assert!(
                    f.store()
                        .read(|tx| tx.events_for(project, StoryNo::new(1)))
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    f.store()
                        .read(|tx| tx.stories(project, &StoryQuery::all()))
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project(project))
                        .unwrap()
                        .unwrap()
                        .next_story_no,
                    1
                );
            }

            // ===============================================================
            // Queries
            // ===============================================================

            /// Six stories spanning every filterable field.
            fn query_fixture(store: &Subject) -> ProjectId {
                let project = seed(store, "alpha", "SH");
                let specs: [(&str, &str, Priority, Option<&str>, Option<&str>, &[&str]); 6] = [
                    (
                        "Alpha",
                        "todo",
                        Priority::Low,
                        Some("ada"),
                        Some("bug"),
                        &["red"],
                    ),
                    (
                        "Bravo",
                        "todo",
                        Priority::Critical,
                        None,
                        Some("feature"),
                        &["red", "blue"],
                    ),
                    (
                        "Charlie",
                        "in-progress",
                        Priority::Medium,
                        Some("ada"),
                        None,
                        &[],
                    ),
                    (
                        "Delta",
                        "in-progress",
                        Priority::High,
                        Some("bob"),
                        Some("bug"),
                        &["blue"],
                    ),
                    ("Echo", "done", Priority::None, None, None, &[]),
                    (
                        "Foxtrot",
                        "done",
                        Priority::High,
                        Some("bob"),
                        Some("feature"),
                        &["red"],
                    ),
                ];
                for (index, (title, state, priority, assignee, story_type, labels)) in
                    specs.into_iter().enumerate()
                {
                    let story = new_story(store, project, title);
                    let minute = index + 1;
                    let mut events = vec![
                        StoryEvent::StoryStateChanged {
                            at: format!("2026-01-01T00:0{minute}:00Z"),
                            state: state.to_string(),
                        },
                        StoryEvent::StoryPrioritySet {
                            at: format!("2026-01-01T00:0{minute}:01Z"),
                            priority,
                        },
                        StoryEvent::StoryLabelsSet {
                            at: format!("2026-01-01T00:0{minute}:02Z"),
                            labels: labels.iter().map(|l| (*l).to_string()).collect(),
                        },
                    ];
                    if let Some(assignee) = assignee {
                        events.push(StoryEvent::StoryAssigned {
                            at: format!("2026-01-01T00:0{minute}:03Z"),
                            member_id: assignee.to_string(),
                        });
                    }
                    if let Some(story_type) = story_type {
                        events.push(StoryEvent::StoryTypeSet {
                            at: format!("2026-01-01T00:0{minute}:04Z"),
                            story_type: story_type.to_string(),
                        });
                    }
                    if state == "done" {
                        events.push(StoryEvent::StoryClosedAndArchived {
                            at: format!("2026-01-01T00:0{minute}:05Z"),
                            state: "done".into(),
                        });
                    }
                    apply(store, project, story, ExpectedSeq::Any, &events).unwrap();
                }
                project
            }

            #[test]
            fn an_unfiltered_query_returns_every_story_in_numeric_order() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all()),
                    [1, 2, 3, 4, 5, 6]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_superstate() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().superstate(SuperState::Open)
                    ),
                    [1, 2, 3, 4]
                );
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().superstate(SuperState::Closed)
                    ),
                    [5, 6]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_state() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().state("in-progress")),
                    [3, 4]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_priority() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().priority(Priority::High)
                    ),
                    [4, 6]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_assignee() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().assignee("ada")),
                    [1, 3]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_type() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().story_type("bug")),
                    [1, 4]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_label() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().label("red")),
                    [1, 2, 6]
                );
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().label("blue")),
                    [2, 4]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_archived() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().archived(false)),
                    [1, 2, 3, 4]
                );
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().archived(true)),
                    [5, 6]
                );
            }

            #[test]
            fn stories_can_be_filtered_by_deleted() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                apply(
                    f.store(),
                    project,
                    StoryNo::new(3),
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryDeleted {
                        at: "2026-01-01T01:00:00Z".into(),
                        reason: "gone".into(),
                    }],
                )
                .unwrap();
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().deleted(true)),
                    [3]
                );
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().deleted(false)),
                    [1, 2, 4, 5, 6]
                );
            }

            #[test]
            fn filters_combine() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all()
                            .superstate(SuperState::Open)
                            .assignee("ada")
                            .label("red")
                    ),
                    [1]
                );
            }

            #[test]
            fn a_query_that_matches_nothing_returns_nothing() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert!(
                    story_numbers(f.store(), project, &StoryQuery::all().assignee("nobody"))
                        .is_empty()
                );
            }

            #[test]
            fn stories_can_be_sorted_by_priority() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().sort(StorySort::Priority)
                    ),
                    [2, 4, 6, 3, 1, 5]
                );
            }

            /// The priority order is *total*. The legacy comparator was
            /// `priority ASC, created_at ASC`, and `created_at` has one-second
            /// precision — so stories created in the same second tied on both
            /// keys and the result depended on the order files happened to be
            /// read in. Asking twice could give two different answers.
            #[test]
            fn the_priority_order_is_total_so_identical_input_gives_identical_output() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                for index in 1..=6 {
                    let story = new_story(f.store(), project, &format!("Story {index}"));
                    apply(
                        f.store(),
                        project,
                        story,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryPrioritySet {
                            // Every story created and prioritised in the same
                            // second, which is exactly the tie the old
                            // comparator could not break.
                            at: "2026-01-01T00:00:00Z".into(),
                            priority: Priority::High,
                        }],
                    )
                    .unwrap();
                }
                let query = StoryQuery::all().sort(StorySort::Priority);
                let first = story_numbers(f.store(), project, &query);
                assert_eq!(first, [1, 2, 3, 4, 5, 6]);
                for _ in 0..5 {
                    assert_eq!(story_numbers(f.store(), project, &query), first);
                }
            }

            #[test]
            fn stories_can_be_sorted_by_recency() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().sort(StorySort::UpdatedAt)
                    ),
                    [6, 5, 4, 3, 2, 1]
                );
            }

            #[test]
            fn a_query_can_be_limited() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                assert_eq!(
                    story_numbers(f.store(), project, &StoryQuery::all().limit(2)),
                    [1, 2]
                );
                assert_eq!(
                    story_numbers(
                        f.store(),
                        project,
                        &StoryQuery::all().sort(StorySort::Priority).limit(3)
                    ),
                    [2, 4, 6]
                );
            }

            #[test]
            fn a_queried_row_carries_its_labels() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                let rows = f
                    .store()
                    .read(|tx| tx.stories(project, &StoryQuery::all()))
                    .unwrap();
                assert_eq!(rows[1].labels, ["blue", "red"]);
                assert!(rows[2].labels.is_empty());
            }

            #[test]
            fn a_queried_row_carries_its_snapshot() {
                let f = <$fixture>::create();
                let project = query_fixture(f.store());
                let rows = f
                    .store()
                    .read(|tx| tx.stories(project, &StoryQuery::all()))
                    .unwrap();
                assert_eq!(rows[0].snapshot.id, "SH-1");
                assert_eq!(rows[0].snapshot.title, "Alpha");
            }

            // ===============================================================
            // Relations
            // ===============================================================

            #[test]
            fn writing_one_side_of_an_edge_materializes_the_other() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: two.to_id("SH"),
                        relation: "blocks".into(),
                    }],
                )
                .unwrap();

                let outbound = f
                    .store()
                    .read(|tx| tx.relations_from(project, two))
                    .unwrap();
                assert_eq!(outbound.len(), 1);
                assert_eq!(outbound[0].relation, "blocked-by");
                assert_eq!(outbound[0].other_no, one);
            }

            #[test]
            fn every_relation_type_mirrors_to_its_inverse() {
                for (relation, inverse) in [
                    ("relates-to", "relates-to"),
                    ("blocks", "blocked-by"),
                    ("blocked-by", "blocks"),
                    ("parent-of", "child-of"),
                    ("child-of", "parent-of"),
                    ("duplicate-of", "duplicate-of"),
                    ("obviates", "obviated-by"),
                    ("obviated-by", "obviates"),
                ] {
                    let f = <$fixture>::create();
                    let project = seed(f.store(), "alpha", "SH");
                    let one = new_story(f.store(), project, "First");
                    let two = new_story(f.store(), project, "Second");
                    link(f.store(), project, one, relation, two).unwrap();

                    let outbound = f
                        .store()
                        .read(|tx| tx.relations_from(project, two))
                        .unwrap();
                    assert_eq!(outbound.len(), 1, "{relation}");
                    assert_eq!(outbound[0].relation, inverse, "{relation}");
                }
            }

            #[test]
            fn inbound_edges_are_queryable_without_a_scan() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let parent = new_story(f.store(), project, "Parent");
                let a = new_story(f.store(), project, "A");
                let b = new_story(f.store(), project, "B");
                link(f.store(), project, parent, "parent-of", a).unwrap();
                link(f.store(), project, parent, "parent-of", b).unwrap();

                let children = f
                    .store()
                    .read(|tx| tx.relations_from(project, parent))
                    .unwrap();
                assert_eq!(children.len(), 2);
                let inbound = f.store().read(|tx| tx.relations_to(project, a)).unwrap();
                assert_eq!(inbound.len(), 1);
                assert_eq!(inbound[0].story_no, parent);
            }

            #[test]
            fn removing_an_edge_removes_both_of_its_directions() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                link(f.store(), project, one, "blocks", two).unwrap();

                for (story, other, relation) in [(one, two, "blocks"), (two, one, "blocked-by")] {
                    apply(
                        f.store(),
                        project,
                        story,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryRelationshipRemoved {
                            at: "2026-01-01T00:11:00Z".into(),
                            other_id: other.to_id("SH"),
                            relation: relation.to_string(),
                        }],
                    )
                    .unwrap();
                }

                assert!(
                    f.store()
                        .read(|tx| tx.relations_from(project, one))
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    f.store()
                        .read(|tx| tx.relations_from(project, two))
                        .unwrap()
                        .is_empty()
                );
            }

            /// The second family of SH-60: stories with two parents, which
            /// survived indefinitely because nothing rejected the write that
            /// created them.
            #[test]
            fn a_story_cannot_be_given_a_second_parent() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let first_parent = new_story(f.store(), project, "First parent");
                let second_parent = new_story(f.store(), project, "Second parent");
                let child = new_story(f.store(), project, "Child");
                link(f.store(), project, first_parent, "parent-of", child).unwrap();

                let error = link(f.store(), project, second_parent, "parent-of", child)
                    .expect_err("a second parent must be refused");

                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
                assert!(error.to_string().contains("one parent"), "{error}");
                let parents = f.store().read(|tx| tx.relations_to(project, child)).unwrap();
                assert_eq!(parents.len(), 1);
                assert_eq!(parents[0].story_no, first_parent);
            }

            #[test]
            fn a_parent_may_have_many_children() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let parent = new_story(f.store(), project, "Parent");
                for index in 1..=4 {
                    let child = new_story(f.store(), project, &format!("Child {index}"));
                    link(f.store(), project, parent, "parent-of", child).unwrap();
                }
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_from(project, parent))
                        .unwrap()
                        .len(),
                    4
                );
            }

            #[test]
            fn a_story_cannot_relate_to_itself() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let error = apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: one.to_id("SH"),
                        relation: "blocks".into(),
                    }],
                )
                .unwrap_err();
                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
            }

            #[test]
            fn a_relation_type_storyhook_does_not_know_is_refused() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                let error = apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: two.to_id("SH"),
                        relation: "supersedes".into(),
                    }],
                )
                .unwrap_err();
                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
            }

            #[test]
            fn a_relation_to_a_story_that_does_not_exist_is_refused() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let error = apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: "SH-99".into(),
                        relation: "blocks".into(),
                    }],
                )
                .unwrap_err();
                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
            }

            #[test]
            fn a_relation_naming_another_projects_story_is_refused() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                let error = apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:10:00Z".into(),
                        other_id: "OTHER-2".into(),
                        relation: "blocks".into(),
                    }],
                )
                .unwrap_err();
                assert!(matches!(error, StoreError::Validation(_)), "{error}");
            }

            #[test]
            fn rewriting_a_story_does_not_duplicate_its_edges() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                link(f.store(), project, one, "blocks", two).unwrap();
                for index in 1..=3 {
                    apply(
                        f.store(),
                        project,
                        one,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryTitleSet {
                            at: format!("2026-01-01T00:2{index}:00Z"),
                            title: format!("Renamed {index}"),
                        }],
                    )
                    .unwrap();
                }
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_from(project, one))
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_from(project, two))
                        .unwrap()
                        .len(),
                    1
                );
            }

            #[test]
            fn edges_are_returned_in_a_stable_order() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                let three = new_story(f.store(), project, "Third");
                link(f.store(), project, one, "relates-to", three).unwrap();
                link(f.store(), project, one, "blocks", two).unwrap();
                let edges: Vec<String> = f
                    .store()
                    .read(|tx| tx.relations_from(project, one))
                    .unwrap()
                    .into_iter()
                    .map(|e| format!("{}:{}", e.relation, e.other_no))
                    .collect();
                assert_eq!(edges, ["blocks:2", "relates-to:3"]);
            }

            // ===============================================================
            // Catalog and settings — every field, individually
            // ===============================================================

            #[test]
            fn states_round_trip_in_configured_order() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert_eq!(f.store().read(|tx| tx.states(project)).unwrap(), states());
            }

            #[test]
            fn a_states_slug_round_trips() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let slugs: Vec<String> = f
                    .store()
                    .read(|tx| tx.states(project))
                    .unwrap()
                    .into_iter()
                    .map(|s| s.slug)
                    .collect();
                assert_eq!(slugs, ["todo", "in-progress", "done"]);
            }

            #[test]
            fn a_states_superstate_round_trips() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let read = f.store().read(|tx| tx.states(project)).unwrap();
                assert_eq!(read[0].super_state, SuperState::Open);
                assert_eq!(read[2].super_state, SuperState::Closed);
            }

            #[test]
            fn a_states_role_round_trips_including_its_absence() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let read = f.store().read(|tx| tx.states(project)).unwrap();
                assert_eq!(read[0].role, None);
                assert_eq!(read[1].role.as_deref(), Some("active"));
            }

            /// SH-49 in one assertion: `save_states` rewrote the whole file
            /// from a struct that did not carry `description`, so every edit
            /// destroyed it.
            #[test]
            fn a_states_description_round_trips_including_its_absence() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let read = f.store().read(|tx| tx.states(project)).unwrap();
                assert_eq!(read[0].description.as_deref(), Some("Not started"));
                assert_eq!(read[1].description, None);
                assert_eq!(read[2].description.as_deref(), Some("Finished"));
            }

            #[test]
            fn editing_one_state_does_not_disturb_its_siblings_fields() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let mut edited = states();
                edited[1].description = Some("Being worked on".into());
                f.store()
                    .write(|tx| tx.put_states(project, &edited))
                    .unwrap();

                let read = f.store().read(|tx| tx.states(project)).unwrap();
                assert_eq!(read, edited);
                assert_eq!(read[0].description.as_deref(), Some("Not started"));
                assert_eq!(read[2].description.as_deref(), Some("Finished"));
                assert_eq!(read[1].role.as_deref(), Some("active"));
            }

            #[test]
            fn replacing_the_state_set_reorders_and_removes() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let mut reordered = states();
                reordered.reverse();
                reordered.pop();
                f.store()
                    .write(|tx| tx.put_states(project, &reordered))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.states(project)).unwrap(), reordered);
            }

            #[test]
            fn a_duplicated_state_slug_is_refused() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let mut duplicated = states();
                duplicated.push(states()[0].clone());
                let error = f
                    .store()
                    .write(|tx| tx.put_states(project, &duplicated))
                    .unwrap_err();
                assert!(matches!(error, StoreError::Invariant(_)), "{error}");
                assert_eq!(f.store().read(|tx| tx.states(project)).unwrap(), states());
            }

            #[test]
            fn types_round_trip_in_configured_order() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert_eq!(f.store().read(|tx| tx.types(project)).unwrap(), types());
            }

            #[test]
            fn a_types_description_round_trips_including_its_absence() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let read = f.store().read(|tx| tx.types(project)).unwrap();
                assert_eq!(read[0].description, None);
                assert_eq!(read[1].description.as_deref(), Some("Something is broken"));
            }

            #[test]
            fn replacing_the_type_set_reorders_and_removes() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let replacement = vec![TypeDef {
                    slug: "chore".into(),
                    description: None,
                }];
                f.store()
                    .write(|tx| tx.put_types(project, &replacement))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.types(project)).unwrap(), replacement);
            }

            #[test]
            fn a_project_starts_with_no_members() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert!(f.store().read(|tx| tx.members(project)).unwrap().is_empty());
            }

            #[test]
            fn a_member_round_trips_field_for_field() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| tx.put_member(project, &member()))
                    .unwrap();
                let read = f.store().read(|tx| tx.members(project)).unwrap();
                assert_eq!(read, vec![member()]);
                assert_eq!(read[0].id, "ada");
                assert_eq!(read[0].display_name, "Ada Lovelace");
                assert_eq!(read[0].email.as_deref(), Some("ada@example.com"));
                assert_eq!(read[0].github.as_deref(), Some("ada"));
                assert_eq!(read[0].created_at, "2026-01-01T00:00:00Z");
            }

            #[test]
            fn a_members_optional_fields_round_trip_when_absent() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let sparse = Member {
                    id: "bob".into(),
                    display_name: "Bob".into(),
                    email: None,
                    github: None,
                    created_at: "2026-01-01T00:00:00Z".into(),
                };
                f.store()
                    .write(|tx| tx.put_member(project, &sparse))
                    .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.members(project)).unwrap(),
                    vec![sparse]
                );
            }

            #[test]
            fn writing_a_member_twice_updates_rather_than_duplicates() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| tx.put_member(project, &member()))
                    .unwrap();
                let mut renamed = member();
                renamed.display_name = "Augusta Ada King".into();
                f.store()
                    .write(|tx| tx.put_member(project, &renamed))
                    .unwrap();
                let read = f.store().read(|tx| tx.members(project)).unwrap();
                assert_eq!(read.len(), 1);
                assert_eq!(read[0].display_name, "Augusta Ada King");
            }

            #[test]
            fn members_are_listed_in_id_order() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                for id in ["zoe", "ada", "mike"] {
                    let mut m = member();
                    m.id = id.into();
                    f.store().write(|tx| tx.put_member(project, &m)).unwrap();
                }
                let ids: Vec<String> = f
                    .store()
                    .read(|tx| tx.members(project))
                    .unwrap()
                    .into_iter()
                    .map(|m| m.id)
                    .collect();
                assert_eq!(ids, ["ada", "mike", "zoe"]);
            }

            #[test]
            fn removing_a_member_reports_whether_there_was_one() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| tx.put_member(project, &member()))
                    .unwrap();
                assert_eq!(
                    f.store()
                        .write(|tx| tx.remove_member(project, "ada"))
                        .unwrap(),
                    true
                );
                assert_eq!(
                    f.store()
                        .write(|tx| tx.remove_member(project, "ada"))
                        .unwrap(),
                    false
                );
                assert!(f.store().read(|tx| tx.members(project)).unwrap().is_empty());
            }

            #[test]
            fn a_project_with_no_settings_reads_back_as_defaults() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                assert_eq!(
                    f.store().read(|tx| tx.settings(project)).unwrap(),
                    ProjectSettings::default()
                );
            }

            #[test]
            fn the_auto_transition_setting_round_trips_in_all_three_states() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                for value in [Some(true), Some(false), None] {
                    let settings = ProjectSettings {
                        sync_auto_transition: value,
                        ..ProjectSettings::default()
                    };
                    f.store()
                        .write(|tx| tx.put_settings(project, &settings))
                        .unwrap();
                    assert_eq!(
                        f.store()
                            .read(|tx| tx.settings(project))
                            .unwrap()
                            .sync_auto_transition,
                        value
                    );
                }
            }

            #[test]
            fn the_stale_threshold_setting_round_trips() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let settings = ProjectSettings {
                    doctor_stale_threshold: Some("21d".into()),
                    ..ProjectSettings::default()
                };
                f.store()
                    .write(|tx| tx.put_settings(project, &settings))
                    .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.settings(project))
                        .unwrap()
                        .doctor_stale_threshold
                        .as_deref(),
                    Some("21d")
                );
            }

            #[test]
            fn the_github_sync_document_round_trips_unchanged() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let document = serde_json::json!({
                    "github": {"owner": "mikeydotio", "repo": "storyhook"},
                    "sync": {"mode": "auto", "last_sync_at": "2026-01-01T00:00:00Z"},
                    "etags": {"issues": "W/\"abc\""},
                    "mappings": [{"story_id": "SH-1", "issue_number": 7,
                                  "last_synced_at": "2026-01-01T00:00:00Z"}]
                });
                let settings = ProjectSettings {
                    github_sync: Some(document.clone()),
                    ..ProjectSettings::default()
                };
                f.store()
                    .write(|tx| tx.put_settings(project, &settings))
                    .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.settings(project))
                        .unwrap()
                        .github_sync,
                    Some(document)
                );
            }

            /// Every setting written every time, from the caller's value. The
            /// pattern this rules out is read-modify-write of a serialized
            /// document, which is how SH-49 destroyed a field the struct in
            /// memory did not know about.
            #[test]
            fn writing_settings_carries_every_field_at_once() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let full = ProjectSettings {
                    sync_auto_transition: Some(true),
                    doctor_stale_threshold: Some("14d".into()),
                    github_sync: Some(serde_json::json!({"sync": {"mode": "manual"}})),
                };
                f.store()
                    .write(|tx| tx.put_settings(project, &full))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.settings(project)).unwrap(), full);

                let cleared = ProjectSettings::default();
                f.store()
                    .write(|tx| tx.put_settings(project, &cleared))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.settings(project)).unwrap(), cleared);
            }

            #[test]
            fn a_github_base_round_trips() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let base = snapshot(f.store(), project, story);
                f.store()
                    .write(|tx| tx.put_github_base(project, story, &base))
                    .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.github_base(project, story)).unwrap(),
                    Some(base)
                );
            }

            #[test]
            fn a_story_with_no_github_base_reads_back_as_nothing() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                assert!(
                    f.store()
                        .read(|tx| tx.github_base(project, story))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn writing_a_github_base_twice_replaces_it() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let first = snapshot(f.store(), project, story);
                f.store()
                    .write(|tx| tx.put_github_base(project, story, &first))
                    .unwrap();
                let mut second = first.clone();
                second.title = "Renamed".into();
                f.store()
                    .write(|tx| tx.put_github_base(project, story, &second))
                    .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.github_base(project, story))
                        .unwrap()
                        .unwrap()
                        .title,
                    "Renamed"
                );
            }

            // ===============================================================
            // Unknown event kinds (SH-54)
            // ===============================================================

            fn teleported() -> RawEvent {
                RawEvent {
                    kind: "StoryTeleported".into(),
                    at: "2026-01-01T00:05:00Z".into(),
                    payload: "{\"kind\":\"StoryTeleported\",\"at\":\"2026-01-01T00:05:00Z\",\
                              \"destination\":\"mars\"}"
                        .into(),
                }
            }

            #[test]
            fn an_event_kind_this_binary_does_not_know_can_still_be_read() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                f.store()
                    .write(|tx| {
                        tx.append_raw_events(project, story, ExpectedSeq::Any, &[teleported()])
                    })
                    .unwrap();

                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                assert_eq!(events.len(), 2);
                assert_eq!(events[1].kind, "StoryTeleported");
                assert_eq!(events[1].at, "2026-01-01T00:05:00Z");
            }

            #[test]
            fn an_unknown_payload_is_retained_byte_for_byte() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                f.store()
                    .write(|tx| {
                        tx.append_raw_events(project, story, ExpectedSeq::Any, &[teleported()])
                    })
                    .unwrap();
                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                match &events[1].payload {
                    StoredPayload::Unknown { kind, json } => {
                        assert_eq!(kind, "StoryTeleported");
                        assert_eq!(*json, teleported().payload);
                    }
                    other => panic!("expected an unknown payload, got {other:?}"),
                }
            }

            #[test]
            fn an_unknown_kind_is_skipped_by_the_fold_and_reported() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                f.store()
                    .write(|tx| {
                        tx.append_raw_events(project, story, ExpectedSeq::Any, &[teleported()])
                    })
                    .unwrap();
                let events = f.store().read(|tx| tx.events_for(project, story)).unwrap();
                let (known, unknown) = partition_known(story, &events);
                assert_eq!(known.len(), 1);
                assert_eq!(unknown.len(), 1);
                assert_eq!(unknown[0].kind, "StoryTeleported");
                assert_eq!(unknown[0].story_no, story);
                assert_eq!(unknown[0].seq, EventSeq::new(2));
            }

            #[test]
            fn known_events_around_an_unknown_one_still_decode() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                f.store()
                    .write(|tx| {
                        tx.append_raw_events(project, story, ExpectedSeq::Any, &[teleported()])
                    })
                    .unwrap();
                apply(
                    f.store(),
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:06:00Z".into(),
                        title: "After the unknown".into(),
                    }],
                )
                .unwrap();
                assert_eq!(
                    snapshot(f.store(), project, story).title,
                    "After the unknown"
                );
            }

            #[test]
            fn an_unknown_kind_still_advances_the_head_and_the_change_feed() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                f.store()
                    .write(|tx| {
                        tx.append_raw_events(project, story, ExpectedSeq::Any, &[teleported()])
                    })
                    .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.head_seq(project, story)).unwrap(),
                    EventSeq::new(2)
                );
                assert_eq!(
                    f.store().read(|tx| tx.max_global_seq(project)).unwrap(),
                    GlobalSeq::new(2)
                );
            }

            #[test]
            fn a_raw_append_obeys_the_same_compare_and_swap() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let error = f
                    .store()
                    .write(|tx| {
                        tx.append_raw_events(
                            project,
                            story,
                            ExpectedSeq::Exact(EventSeq::ZERO),
                            &[teleported()],
                        )
                    })
                    .unwrap_err();
                assert!(matches!(error, StoreError::Conflict { .. }), "{error}");
            }

            // ===============================================================
            // Cross-project isolation
            //
            // A single global database in which every repository defaults to
            // the prefix `SH` makes this an entirely new risk class: a query
            // that forgets its scope does not fail, it returns the wrong
            // project's story. Every read path gets a case.
            // ===============================================================

            #[test]
            fn isolation_project_lookup_by_uuid() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_uuid("uuid-alpha"))
                        .unwrap()
                        .unwrap()
                        .id,
                    alpha
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_uuid("uuid-beta"))
                        .unwrap()
                        .unwrap()
                        .id,
                    beta
                );
            }

            #[test]
            fn isolation_project_lookup_by_slug() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_slug("alpha"))
                        .unwrap()
                        .unwrap()
                        .id,
                    alpha
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_slug("beta"))
                        .unwrap()
                        .unwrap()
                        .id,
                    beta
                );
            }

            #[test]
            fn isolation_project_lookup_by_path() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(alpha, Path::new("/repos/alpha"), PathKind::Main)?;
                        tx.touch_project_path(beta, Path::new("/repos/beta"), PathKind::Main)
                    })
                    .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project_by_path(Path::new("/repos/beta")))
                        .unwrap()
                        .unwrap()
                        .id,
                    beta
                );
            }

            #[test]
            fn isolation_project_paths() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                f.store()
                    .write(|tx| {
                        tx.touch_project_path(alpha, Path::new("/repos/alpha"), PathKind::Main)?;
                        tx.touch_project_path(beta, Path::new("/repos/beta"), PathKind::Main)
                    })
                    .unwrap();
                let paths = f.store().read(|tx| tx.project_paths(alpha)).unwrap();
                assert_eq!(paths.len(), 1);
                assert_eq!(paths[0].path, "/repos/alpha");
            }

            #[test]
            fn isolation_states() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let replacement = vec![
                    StateDef {
                        slug: "spike".into(),
                        super_state: SuperState::Open,
                        role: None,
                        description: None,
                    },
                    StateDef {
                        slug: "shipped".into(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: None,
                    },
                ];
                f.store()
                    .write(|tx| tx.put_states(beta, &replacement))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.states(alpha)).unwrap(), states());
                assert_eq!(f.store().read(|tx| tx.states(beta)).unwrap(), replacement);
            }

            #[test]
            fn isolation_types() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let replacement = vec![TypeDef {
                    slug: "chore".into(),
                    description: None,
                }];
                f.store()
                    .write(|tx| tx.put_types(beta, &replacement))
                    .unwrap();
                assert_eq!(f.store().read(|tx| tx.types(alpha)).unwrap(), types());
                assert_eq!(f.store().read(|tx| tx.types(beta)).unwrap(), replacement);
            }

            #[test]
            fn isolation_members() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                f.store().write(|tx| tx.put_member(beta, &member())).unwrap();
                assert!(f.store().read(|tx| tx.members(alpha)).unwrap().is_empty());
                assert_eq!(f.store().read(|tx| tx.members(beta)).unwrap().len(), 1);
            }

            #[test]
            fn isolation_settings() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let settings = ProjectSettings {
                    doctor_stale_threshold: Some("30d".into()),
                    ..ProjectSettings::default()
                };
                f.store()
                    .write(|tx| tx.put_settings(beta, &settings))
                    .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.settings(alpha)).unwrap(),
                    ProjectSettings::default()
                );
                assert_eq!(f.store().read(|tx| tx.settings(beta)).unwrap(), settings);
            }

            #[test]
            fn isolation_events_for_a_story_number_both_projects_have() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let alpha_events = f
                    .store()
                    .read(|tx| tx.events_for(alpha, StoryNo::new(1)))
                    .unwrap();
                let beta_events = f
                    .store()
                    .read(|tx| tx.events_for(beta, StoryNo::new(1)))
                    .unwrap();
                assert_eq!(alpha_events.len(), 1);
                assert_eq!(beta_events.len(), 1);
                assert!(matches!(
                    alpha_events[0].known(),
                    Some(StoryEvent::StoryCreated { title, .. }) if title == "alpha story 1"
                ));
                assert!(matches!(
                    beta_events[0].known(),
                    Some(StoryEvent::StoryCreated { title, .. }) if title == "beta story 1"
                ));
            }

            #[test]
            fn isolation_head_seq() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                apply(
                    f.store(),
                    alpha,
                    StoryNo::new(1),
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.head_seq(alpha, StoryNo::new(1)))
                        .unwrap(),
                    EventSeq::new(2)
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.head_seq(beta, StoryNo::new(1)))
                        .unwrap(),
                    EventSeq::new(1)
                );
            }

            #[test]
            fn isolation_change_feed() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let alpha_feed = f
                    .store()
                    .read(|tx| tx.events_since(alpha, GlobalSeq::ZERO, 100))
                    .unwrap();
                assert_eq!(alpha_feed.len(), 2);
                // Each project has its own positions, starting at 1 — a shared
                // counter would leak one project's write rate to the other.
                assert_eq!(
                    alpha_feed
                        .iter()
                        .map(|e| e.event.global_seq.get())
                        .collect::<Vec<_>>(),
                    [1, 2]
                );
                let beta_feed = f
                    .store()
                    .read(|tx| tx.events_since(beta, GlobalSeq::ZERO, 100))
                    .unwrap();
                assert_eq!(
                    beta_feed
                        .iter()
                        .map(|e| e.event.global_seq.get())
                        .collect::<Vec<_>>(),
                    [1, 2]
                );
            }

            #[test]
            fn isolation_change_feed_head() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                apply(
                    f.store(),
                    alpha,
                    StoryNo::new(1),
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryTitleSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        title: "Renamed".into(),
                    }],
                )
                .unwrap();
                assert_eq!(
                    f.store().read(|tx| tx.max_global_seq(alpha)).unwrap(),
                    GlobalSeq::new(3)
                );
                assert_eq!(
                    f.store().read(|tx| tx.max_global_seq(beta)).unwrap(),
                    GlobalSeq::new(2)
                );
            }

            #[test]
            fn isolation_single_story_read() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                assert_eq!(
                    f.store()
                        .read(|tx| tx.story(alpha, StoryNo::new(2)))
                        .unwrap()
                        .unwrap()
                        .title,
                    "alpha story 2"
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.story(beta, StoryNo::new(2)))
                        .unwrap()
                        .unwrap()
                        .title,
                    "beta story 2"
                );
            }

            #[test]
            fn isolation_story_query() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let alpha_titles: Vec<String> = f
                    .store()
                    .read(|tx| tx.stories(alpha, &StoryQuery::all()))
                    .unwrap()
                    .into_iter()
                    .map(|r| r.title)
                    .collect();
                assert_eq!(alpha_titles, ["alpha story 1", "alpha story 2"]);
                let beta_titles: Vec<String> = f
                    .store()
                    .read(|tx| tx.stories(beta, &StoryQuery::all()))
                    .unwrap()
                    .into_iter()
                    .map(|r| r.title)
                    .collect();
                assert_eq!(beta_titles, ["beta story 1", "beta story 2"]);
            }

            #[test]
            fn isolation_label_query() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                for project in [alpha, beta] {
                    apply(
                        f.store(),
                        project,
                        StoryNo::new(1),
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryLabelsSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            labels: vec!["shared".into()],
                        }],
                    )
                    .unwrap();
                }
                assert_eq!(
                    story_numbers(f.store(), alpha, &StoryQuery::all().label("shared")),
                    [1]
                );
                assert_eq!(
                    story_numbers(f.store(), beta, &StoryQuery::all().label("shared")),
                    [1]
                );
            }

            #[test]
            fn isolation_outbound_relations() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                link(f.store(), alpha, StoryNo::new(1), "blocks", StoryNo::new(2)).unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_from(alpha, StoryNo::new(1)))
                        .unwrap()
                        .len(),
                    1
                );
                assert!(
                    f.store()
                        .read(|tx| tx.relations_from(beta, StoryNo::new(1)))
                        .unwrap()
                        .is_empty()
                );
            }

            #[test]
            fn isolation_inbound_relations() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                link(f.store(), alpha, StoryNo::new(1), "blocks", StoryNo::new(2)).unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_to(alpha, StoryNo::new(2)))
                        .unwrap()
                        .len(),
                    1
                );
                assert!(
                    f.store()
                        .read(|tx| tx.relations_to(beta, StoryNo::new(2)))
                        .unwrap()
                        .is_empty()
                );
            }

            #[test]
            fn isolation_single_parent_is_enforced_per_project() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                link(
                    f.store(),
                    alpha,
                    StoryNo::new(1),
                    "parent-of",
                    StoryNo::new(2),
                )
                .unwrap();
                // beta's SH-2 has no parent, so the same edge must be allowed
                // there — the constraint is scoped, not global.
                link(
                    f.store(),
                    beta,
                    StoryNo::new(1),
                    "parent-of",
                    StoryNo::new(2),
                )
                .unwrap();
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_to(beta, StoryNo::new(2)))
                        .unwrap()
                        .len(),
                    1
                );
            }

            #[test]
            fn isolation_github_bases() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let base = snapshot(f.store(), alpha, StoryNo::new(1));
                f.store()
                    .write(|tx| tx.put_github_base(alpha, StoryNo::new(1), &base))
                    .unwrap();
                assert!(
                    f.store()
                        .read(|tx| tx.github_base(alpha, StoryNo::new(1)))
                        .unwrap()
                        .is_some()
                );
                assert!(
                    f.store()
                        .read(|tx| tx.github_base(beta, StoryNo::new(1)))
                        .unwrap()
                        .is_none()
                );
            }

            #[test]
            fn isolation_story_numbers_are_allocated_independently() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(alpha)).unwrap(),
                    StoryNo::new(3)
                );
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(beta)).unwrap(),
                    StoryNo::new(3)
                );
            }

            #[test]
            fn isolation_writing_one_project_does_not_touch_the_other() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                let before = f
                    .store()
                    .read(|tx| tx.stories(beta, &StoryQuery::all()))
                    .unwrap();
                for index in 1..=2 {
                    apply(
                        f.store(),
                        alpha,
                        StoryNo::new(index),
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryClosedAndArchived {
                            at: "2026-01-01T00:01:00Z".into(),
                            state: "done".into(),
                        }],
                    )
                    .unwrap();
                }
                assert_eq!(
                    f.store()
                        .read(|tx| tx.stories(beta, &StoryQuery::all()))
                        .unwrap(),
                    before
                );
            }

            #[test]
            fn isolation_a_story_number_absent_in_one_project_stays_absent() {
                let f = <$fixture>::create();
                let (alpha, beta) = twin_projects(f.store());
                new_story(f.store(), alpha, "alpha story 3");
                assert!(
                    f.store()
                        .read(|tx| tx.story(alpha, StoryNo::new(3)))
                        .unwrap()
                        .is_some()
                );
                assert!(
                    f.store()
                        .read(|tx| tx.story(beta, StoryNo::new(3)))
                        .unwrap()
                        .is_none()
                );
                assert!(
                    f.store()
                        .read(|tx| tx.events_for(beta, StoryNo::new(3)))
                        .unwrap()
                        .is_empty()
                );
            }

            // ===============================================================
            // Durability
            // ===============================================================

            #[test]
            fn committed_projects_survive_a_reopen() {
                let f = <$fixture>::create();
                seed(f.store(), "alpha", "SH");
                let f = f.reopen();
                let projects = f.store().read(|tx| tx.projects()).unwrap();
                assert_eq!(projects.len(), 1);
                assert_eq!(projects[0].slug, "alpha");
            }

            #[test]
            fn committed_events_survive_a_reopen() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let story = new_story(f.store(), project, "First");
                let f = f.reopen();
                assert_eq!(
                    f.store().read(|tx| tx.events_for(project, story)).unwrap().len(),
                    1
                );
                assert_eq!(
                    f.store().read(|tx| tx.head_seq(project, story)).unwrap(),
                    EventSeq::new(1)
                );
            }

            #[test]
            fn the_committed_read_model_survives_a_reopen() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let one = new_story(f.store(), project, "First");
                let two = new_story(f.store(), project, "Second");
                link(f.store(), project, one, "blocks", two).unwrap();
                apply(
                    f.store(),
                    project,
                    one,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:11:00Z".into(),
                        labels: vec!["kept".into()],
                    }],
                )
                .unwrap();

                let f = f.reopen();
                let row = f
                    .store()
                    .read(|tx| tx.story(project, one))
                    .unwrap()
                    .unwrap();
                assert_eq!(row.title, "First");
                assert_eq!(row.labels, ["kept"]);
                assert_eq!(
                    f.store()
                        .read(|tx| tx.relations_from(project, two))
                        .unwrap()
                        .len(),
                    1
                );
            }

            #[test]
            fn the_allocation_counter_survives_a_reopen() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                new_story(f.store(), project, "First");
                new_story(f.store(), project, "Second");
                let f = f.reopen();
                assert_eq!(
                    f.store().write(|tx| tx.allocate_story_no(project)).unwrap(),
                    StoryNo::new(3)
                );
            }

            #[test]
            fn a_rolled_back_write_leaves_nothing_after_a_reopen() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let _ = f.store().write(|tx| {
                    tx.allocate_story_no(project)?;
                    tx.append_events(
                        project,
                        StoryNo::new(1),
                        ExpectedSeq::Any,
                        &[created("Never", "2026-01-01T00:00:00Z")],
                    )?;
                    Err::<(), _>(StoreError::Validation("no".into()))
                });
                let f = f.reopen();
                assert!(
                    f.store()
                        .read(|tx| tx.events_for(project, StoryNo::new(1)))
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    f.store()
                        .read(|tx| tx.project(project))
                        .unwrap()
                        .unwrap()
                        .next_story_no,
                    1
                );
            }

            #[test]
            fn the_catalog_survives_a_reopen() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                f.store()
                    .write(|tx| tx.put_member(project, &member()))
                    .unwrap();
                let settings = ProjectSettings {
                    sync_auto_transition: Some(true),
                    doctor_stale_threshold: Some("14d".into()),
                    github_sync: Some(serde_json::json!({"sync": {"mode": "off"}})),
                };
                f.store()
                    .write(|tx| tx.put_settings(project, &settings))
                    .unwrap();

                let f = f.reopen();
                assert_eq!(f.store().read(|tx| tx.states(project)).unwrap(), states());
                assert_eq!(f.store().read(|tx| tx.types(project)).unwrap(), types());
                assert_eq!(
                    f.store().read(|tx| tx.members(project)).unwrap(),
                    vec![member()]
                );
                assert_eq!(f.store().read(|tx| tx.settings(project)).unwrap(), settings);
            }

            // ===============================================================
            // Concurrency
            // ===============================================================

            #[test]
            fn concurrent_writes_to_one_project_all_land() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                let store = f.store();

                std::thread::scope(|scope| {
                    for _ in 0..4 {
                        scope.spawn(move || {
                            for _ in 0..10 {
                                let story =
                                    store.write(|tx| tx.allocate_story_no(project)).unwrap();
                                store
                                    .write(|tx| {
                                        tx.append_events(
                                            project,
                                            story,
                                            ExpectedSeq::Exact(EventSeq::ZERO),
                                            &[created("Concurrent", "2026-01-01T00:00:00Z")],
                                        )
                                    })
                                    .unwrap();
                            }
                        });
                    }
                });

                let feed = f
                    .store()
                    .read(|tx| tx.events_since(project, GlobalSeq::ZERO, 1000))
                    .unwrap();
                assert_eq!(feed.len(), 40);
                let mut positions: Vec<i64> =
                    feed.iter().map(|e| e.event.global_seq.get()).collect();
                positions.sort_unstable();
                positions.dedup();
                assert_eq!(positions.len(), 40, "change-feed positions must be unique");
            }

            #[test]
            fn concurrent_readers_are_not_blocked_by_a_writer() {
                let f = <$fixture>::create();
                let project = seed(f.store(), "alpha", "SH");
                new_story(f.store(), project, "First");
                let store = f.store();

                std::thread::scope(|scope| {
                    let writer = scope.spawn(move || {
                        for _ in 0..20 {
                            store.write(|tx| tx.allocate_story_no(project)).unwrap();
                        }
                    });
                    let readers: Vec<_> = (0..4)
                        .map(|_| {
                            scope.spawn(move || {
                                for _ in 0..20 {
                                    // Reads are served from the write-ahead
                                    // log's snapshot, so they never wait on the
                                    // writer and never see a half-written
                                    // transaction.
                                    let rows = store
                                        .read(|tx| tx.stories(project, &StoryQuery::all()))
                                        .unwrap();
                                    assert_eq!(rows.len(), 1);
                                }
                            })
                        })
                        .collect();
                    writer.join().unwrap();
                    for reader in readers {
                        reader.join().unwrap();
                    }
                });
            }
        }
    };
}
