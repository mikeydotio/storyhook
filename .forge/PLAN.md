# Implementation Plan: Story Types & Epics

## Requirements

| ID | Requirement | Type | Priority |
|----|-------------|------|----------|
| R1 | `StoryTypeSet` event variant mirroring `StoryPrioritySet` | functional | high |
| R2 | `TypeDef` struct with slug + optional description | functional | high |
| R3 | `StorySnapshot.story_type: Option<String>` with serde(default) | functional | high |
| R4 | Fold `StoryTypeSet` into snapshot, `last_activity_type` arm | functional | high |
| R5 | `has_children` and `compute_progress` helper functions | functional | high |
| R6 | `ProgressRollup` struct (children_done, children_total) | functional | high |
| R7 | `types.toml` config file lifecycle (path, load, save, CRUD, defaults) | functional | high |
| R8 | `ensure_types_file` auto-creates on first use, `init_project` integration | functional | high |
| R9 | `default_type` returns first slug from types.toml | functional | high |
| R10 | `add_type` validates no duplicates, rejects reserved slug "none" | functional | high |
| R11 | `remove_type` rejects if any story (open or archived) uses the type | functional | high |
| R12 | `TypeAction` enum (List, Add, Remove) and `parse_type` parser | functional | high |
| R13 | `EpicAction` enum (List, Show, Create, Add) and `parse_epic` parser | functional | high |
| R14 | `--type` flag on `parse_new`, `parse_list`, `parse_set` | functional | high |
| R15 | `Invocation::Type` and `Invocation::Epic` variants | functional | high |
| R16 | `story_type: Option<String>` on `Invocation::New`, `List`, `SetFields` | functional | high |
| R17 | Type/Epic command handlers in app.rs | functional | high |
| R18 | Type validation at write time (new, set, epic create) | functional | high |
| R19 | `--type` filter in List handler, including `--type none` for untyped | functional | high |
| R20 | Parent skip in Next handler (skip stories with children) | functional | high |
| R21 | Progress rollup computed in `build_story_views` | functional | high |
| R22 | Doctor type integrity check (unknown types flagged) | functional | medium |
| R23 | `story_type` param on MCP create/update/list tool schemas | functional | medium |
| R24 | `build_invocation` updates in mcp.rs for type param | functional | medium |
| R25 | `StoryView.progress: Option<ProgressRollup>` field | functional | high |
| R26 | Type + progress rendering in human output (story show, list) | functional | high |
| R27 | Type + progress in JSON output via serde | functional | medium |
| R28 | Backward compat: old snapshots deserialize with `story_type: None` | non-functional | high |
| R29 | Help text updated for new commands and flags | functional | low |
| R30 | `epic create` emits StoryCreated + StoryTypeSet in single lock | functional | high |
| R31 | Default type resolution at display time (None -> first types.toml entry) | functional | high |
| R32 | `cargo test` passes with 0 failures after all changes | non-functional | high |
| R33 | `cargo build` succeeds with no errors after each wave | non-functional | high |
| R34 | `story_type: Option<String>` on `ImportStory` struct for bulk_create/import | functional | high |
| R35 | `ProjectExport` includes types config; import restores types.toml | functional | high |

## Task Waves

### Wave 1 (parallel -- foundational types, no inter-dependencies)

These three tasks modify independent files and establish the contracts that later waves depend on.

#### T1.1: domain.rs -- StoryTypeSet event, TypeDef, snapshot field, fold logic

- **Requirement(s)**: R1, R2, R3, R4, R6, R28, R34
- **Acceptance criteria**:
  - [ ] `StoryEvent` enum contains a `StoryTypeSet { at: String, story_type: String }` variant
  - [ ] `TypeDef` struct exists with fields `slug: String` and `description: Option<String>`, deriving Serialize, Deserialize, Clone, Debug
  - [ ] `StorySnapshot` has field `story_type: Option<String>` with `#[serde(default)]`
  - [ ] `ImportStory` struct has field `story_type: Option<String>` with `#[serde(default)]`
  - [ ] `ProgressRollup` struct exists with fields `children_done: usize, children_total: usize`, deriving Serialize, Deserialize, Clone, Debug
  - [ ] `fold_story` handles `StoryTypeSet` by updating `story_type` on the snapshot and `updated_at`
  - [ ] `last_activity_type` returns `"type-set"` for `StoryTypeSet` variant
  - [ ] Existing unit tests in domain.rs pass (`cargo test domain::tests`)
  - [ ] A new test verifies fold correctly sets `story_type` from a `StoryTypeSet` event
  - [ ] A new test verifies that folding without `StoryTypeSet` yields `story_type: None`
- **Files**: `src/domain.rs`
- **Estimated scope**: medium

#### T1.2: storage.rs -- types.toml config lifecycle

- **Requirement(s)**: R7, R8, R9, R10, R11
- **Acceptance criteria**:
  - [ ] `ProjectPaths` has a `types_file()` method returning `.storyhook/types.toml`
  - [ ] A `TypesFile` struct wraps `Vec<TypeDef>` for toml serialization (matching `StatesFile` pattern)
  - [ ] `load_types(root)` reads and parses types.toml; if file is missing, calls `ensure_types_file` first then reads
  - [ ] `load_type_map(root)` returns `BTreeMap<String, TypeDef>` keyed by slug
  - [ ] `save_types(root, &[TypeDef])` writes types.toml
  - [ ] `add_type(root, slug, description)` validates no duplicates, rejects slug "none", appends and saves
  - [ ] `remove_type(root, slug)` returns error if any story snapshot (open or archived) has `story_type == Some(slug)`, otherwise removes and saves (matches `remove_state` precedent which checks all stories via `load_all_snapshots`)
  - [ ] `ensure_types_file(root)` creates types.toml with 5 default entries (story, epic, bug, chore, task) if missing, is a no-op if file exists
  - [ ] `default_type(root)` returns the slug of the first entry in types.toml
  - [ ] `init_project` calls `ensure_types_file` (so new projects get types.toml)
  - [ ] `cargo build` succeeds (note: `TypeDef` import from domain.rs means T1.1 must land first OR a stub is used; however, `TypeDef` is a simple struct that can be defined here if needed -- see dependency note)
- **Files**: `src/storage.rs`
- **Depends on**: T1.1 (needs `TypeDef` from domain.rs)
- **Estimated scope**: medium

#### T1.3: cli.rs -- TypeAction, EpicAction, Invocation variants, parsers, flags

- **Requirement(s)**: R12, R13, R14, R15, R16, R29
- **Acceptance criteria**:
  - [ ] `TypeAction` enum exists with variants `List`, `Add { slug: String, description: Option<String> }`, `Remove { slug: String }`
  - [ ] `EpicAction` enum exists with variants `List`, `Show { id: String }`, `Create { title: String }`, `Add { epic_id: String, story_id: String }`
  - [ ] `Invocation::Type { action: TypeAction }` variant exists
  - [ ] `Invocation::Epic { action: EpicAction }` variant exists
  - [ ] `Invocation::New` has field `story_type: Option<String>`
  - [ ] `Invocation::List` has field `story_type: Option<String>`
  - [ ] `Invocation::SetFields` has field `story_type: Option<String>`
  - [ ] `parse_invocation` dispatches `"type"` to `parse_type` and `"epic"` to `parse_epic`
  - [ ] `parse_new` accepts `--type <slug>` flag
  - [ ] `parse_list` accepts `--type <slug>` flag
  - [ ] `parse_set` accepts `--type <slug>` flag
  - [ ] `parse_type` follows the `parse_state` / `parse_phase` pattern: `story type list`, `story type add <slug> [--description "<text>"]`, `story type remove <slug>`
  - [ ] `parse_epic` parses: `story epic list`, `story epic show <id>`, `story epic create "<title>"`, `story epic add <epic-id> <story-id>`
  - [ ] `HELP_TEXT` updated with type and epic commands and --type flag on existing commands
  - [ ] Existing cli.rs tests pass (`cargo test cli::tests`)
  - [ ] New test: `parse_invocation(&["type", "list"])` returns `Invocation::Type { action: TypeAction::List }`
  - [ ] New test: `parse_invocation(&["new", "My story", "--type", "bug"])` returns `Invocation::New` with `story_type: Some("bug")`
  - [ ] New test: `parse_invocation(&["epic", "create", "Auth System"])` returns `Invocation::Epic { action: EpicAction::Create { title: "Auth System" } }`
- **Files**: `src/cli.rs`
- **Estimated scope**: medium

**Wave 1 Dependency Note**: T1.2 depends on T1.1 for the `TypeDef` struct. T1.3 is fully independent of T1.1 and T1.2. If T1.1 and T1.3 are executed in parallel, T1.2 follows immediately after T1.1 completes. Alternatively, T1.1 and T1.3 run in parallel, then T1.2 runs.

**Resumption point after Wave 1**: All three inner layers are done. `StoryTypeSet` event exists and folds correctly, `types.toml` loads/saves, CLI parses all new commands. `cargo build` may have warnings about unused Invocation variants (handled in Wave 2). The system is internally consistent but no commands produce user-visible results yet.

---

### Wave 2 (parallel -- app.rs handlers + output.rs rendering)

These tasks integrate the Wave 1 outputs. T2.1 and T2.2 can run in parallel because they touch different files and T2.2 only needs the domain structs (not the app handler logic).

#### T2.1: app.rs -- Type and Epic command handlers, type validation, --type filter, epic create two-event pattern

- **Requirement(s)**: R17, R18, R19, R30
- **Acceptance criteria**:
  - [ ] `Invocation::Type { action: TypeAction::List }` handler returns a `Response::Message` listing all types from `load_types`
  - [ ] `Invocation::Type { action: TypeAction::Add { .. } }` handler calls `storage::add_type` and returns success message
  - [ ] `Invocation::Type { action: TypeAction::Remove { .. } }` handler calls `storage::remove_type` and returns success message
  - [ ] `Invocation::Epic { action: EpicAction::Create { title } }` handler: inside `with_project_lock`, calls `storage::create_story` then immediately writes `StoryTypeSet { story_type: "epic" }` event, returns the updated story view
  - [ ] `Invocation::Epic { action: EpicAction::Add { epic_id, story_id } }` handler delegates to the existing `Relate` logic with relation `"parent-of"`
  - [ ] `Invocation::Epic { action: EpicAction::List }` handler: lists stories filtered to `story_type == Some("epic")` with progress rollup displayed
  - [ ] `Invocation::Epic { action: EpicAction::Show { id } }` handler: delegates to existing `Show` logic
  - [ ] `Invocation::New` handler: when `story_type` is `Some(slug)`, validates slug exists in `load_type_map`, writes `StoryTypeSet` event after `StoryCreated`
  - [ ] `Invocation::SetFields` handler: when `story_type` is `Some(slug)`, validates slug exists in `load_type_map`, emits `StoryTypeSet` event
  - [ ] `Invocation::List` handler: when `story_type` is `Some("none")`, retains only stories where `story.story_type.is_none()`; otherwise filters to `story.story_type.as_deref() == Some(slug)`
  - [ ] Unknown type slug on `New` or `SetFields` returns `AppError::Validation` with message containing "is not defined" and listing available types
  - [ ] `cargo build` succeeds
- **Files**: `src/app.rs`
- **Depends on**: T1.1, T1.2, T1.3
- **Estimated scope**: large

#### T2.2: output.rs -- StoryView.progress, type + progress rendering

- **Requirement(s)**: R25, R26, R27, R31
- **Acceptance criteria**:
  - [ ] `StoryView` struct has field `progress: Option<ProgressRollup>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
  - [ ] `render_story` (human single-story view) displays `type: <slug>` after the priority line; when `story.story_type` is `None`, displays the default type from `types.toml` (requires passing default_type or accepting it as a parameter)
  - [ ] `render_story` displays progress line (e.g., `progress: 4/5 children done (80%)`) when `progress` is `Some`
  - [ ] List rendering shows `[type]` badge after the story ID when a type is set
  - [ ] List rendering shows progress summary (e.g., `(3/5)`) for stories with progress
  - [ ] JSON output includes `story_type` field (may be null for old stories) and `progress` field via serde
  - [ ] `cargo build` succeeds
- **Files**: `src/output.rs`
- **Depends on**: T1.1 (needs `ProgressRollup`)
- **Estimated scope**: medium

**Resumption point after Wave 2**: All commands work end-to-end. `story type list/add/remove`, `story epic create/add/list/show`, `story new --type`, `story list --type`, `story set --type` all function. Output shows types and progress. The system is feature-complete except for Next parent-skip, build_story_views progress computation, doctor integration, and MCP.

---

### Wave 3 (parallel -- integration points)

These tasks wire up the remaining integration points. All are independent of each other.

#### T3.1: app.rs -- build_story_views progress rollup, Next handler parent skip, doctor type check

- **Requirement(s)**: R5, R20, R21, R22
- **Acceptance criteria**:
  - [ ] `has_children` function exists in domain.rs (or is inlined in app.rs) and returns true when a story has any `parent-of` relationship
  - [ ] `compute_progress` function exists and returns `Some(ProgressRollup)` when the story has children, counting direct children only where `superstate == Closed` counts as done, including archived children
  - [ ] `build_story_views` calls `compute_progress` for each story and attaches the result to `StoryView.progress`
  - [ ] `Invocation::Next` handler, after filtering to `is_ready`, additionally filters out stories where `has_children` is true
  - [ ] `doctor_report` loads `load_type_map`, and for each story with `story_type = Some(slug)` where slug is not in the type map, adds a flagged reason `"unknown type \`{slug}\`"`
  - [ ] Running `story next` on a project where the only ready story is a parent returns "no ready stories"
  - [ ] `cargo build` succeeds
- **Files**: `src/domain.rs` (has_children, compute_progress), `src/app.rs` (build_story_views, Next handler, doctor)
- **Depends on**: T2.1, T2.2
- **Estimated scope**: medium

#### T3.2: mcp.rs -- story_type param on MCP tools

- **Requirement(s)**: R23, R24
- **Acceptance criteria**:
  - [ ] `handle_tools_list` includes `"story_type"` in `storyhook_create_story` inputSchema properties with type "string" and description
  - [ ] `handle_tools_list` includes `"story_type"` in `storyhook_update_story` inputSchema properties
  - [ ] `handle_tools_list` includes `"story_type"` in `storyhook_list_stories` inputSchema properties
  - [ ] `build_invocation` for `storyhook_create_story`: passes `story_type` from arguments to `Invocation::New`
  - [ ] `build_invocation` for `storyhook_update_story`: when `story_type` is present, maps to `Invocation::SetFields` with `story_type` set
  - [ ] `build_invocation` for `storyhook_list_stories`: passes `story_type` from arguments to `Invocation::List`
  - [ ] `cargo build` succeeds
- **Files**: `src/mcp.rs`
- **Depends on**: T1.3 (needs updated Invocation variants)
- **Estimated scope**: small

#### T3.3: storage.rs + app.rs -- Export/import types.toml, ImportStory type handling

- **Requirement(s)**: R34, R35
- **Acceptance criteria**:
  - [ ] `ProjectExport` struct has field `types: Vec<TypeDef>` with `#[serde(default)]`
  - [ ] `export_project` includes `load_types(root)` in the export
  - [ ] `import_project` calls `save_types` to restore types.toml from the export data (if non-empty)
  - [ ] Import loop: when `ImportStory.story_type` is `Some(slug)`, writes `StoryTypeSet` event after `StoryCreated`
  - [ ] MCP `storyhook_bulk_create`: `build_invocation` maps `story_type` per item through `ImportStory`
  - [ ] Round-trip test: export a project with types → import → types.toml restored, typed stories retain their type
  - [ ] `cargo build` succeeds
- **Files**: `src/storage.rs`, `src/app.rs`, `src/mcp.rs`
- **Depends on**: T1.1, T1.2
- **Estimated scope**: small

**Resumption point after Wave 3**: Feature is fully complete. All CLI commands, MCP tools, output rendering, progress rollup, parent skipping, doctor checks, and export/import all work. The system is ready for final verification.

---

### Wave 4 (sequential -- final verification)

#### T4.1: Full compilation and test pass

- **Requirement(s)**: R32, R33
- **Acceptance criteria**:
  - [ ] `cargo build` succeeds with no errors
  - [ ] `cargo test` passes with 0 failures
  - [ ] `cargo clippy` produces no errors (warnings acceptable if pre-existing)
- **Files**: all modified files (no new changes expected, just verification)
- **Depends on**: T3.1, T3.2
- **Estimated scope**: small

## Requirement Traceability

| Requirement | Tasks | Coverage |
|-------------|-------|----------|
| R1: StoryTypeSet event | T1.1 | full |
| R2: TypeDef struct | T1.1 | full |
| R3: story_type on snapshot | T1.1 | full |
| R4: Fold StoryTypeSet | T1.1 | full |
| R5: has_children, compute_progress | T3.1 | full |
| R6: ProgressRollup struct | T1.1 | full |
| R7: types.toml lifecycle | T1.2 | full |
| R8: ensure_types_file + init | T1.2 | full |
| R9: default_type | T1.2 | full |
| R10: add_type validation | T1.2 | full |
| R11: remove_type validation | T1.2 | full |
| R12: TypeAction enum + parser | T1.3 | full |
| R13: EpicAction enum + parser | T1.3 | full |
| R14: --type flag on new/list/set | T1.3 | full |
| R15: Invocation::Type, ::Epic | T1.3 | full |
| R16: story_type on New/List/SetFields | T1.3 | full |
| R17: Type/Epic handlers | T2.1 | full |
| R18: Type validation at write | T2.1 | full |
| R19: --type filter in List | T2.1 | full |
| R20: Parent skip in Next | T3.1 | full |
| R21: Progress in build_story_views | T3.1 | full |
| R22: Doctor type check | T3.1 | full |
| R23: MCP tool schemas | T3.2 | full |
| R24: MCP build_invocation | T3.2 | full |
| R25: StoryView.progress | T2.2 | full |
| R26: Human output rendering | T2.2 | full |
| R27: JSON output | T2.2 | full |
| R28: Backward compat | T1.1 | full |
| R29: Help text | T1.3 | full |
| R30: Epic create two-event | T2.1 | full |
| R31: Default type display | T2.2 | full |
| R32: cargo test passes | T4.1 | full |
| R33: cargo build succeeds | T4.1 | full |
| R34: ImportStory story_type field | T1.1, T3.3 | full |
| R35: Export/import types config | T3.3 | full |

## Dependency Graph

```
T1.1 (domain.rs)  ──┬──> T1.2 (storage.rs) ──┬──> T2.1 (app.rs handlers)  ──┬──> T3.1 (app.rs integration) ──> T4.1
                     │                         │                               │
                     ├──> T2.2 (output.rs) ────┘                               │
                     │                         │                               │
                     ├──> T3.3 (export/import) ┘                               │
                     │                                                         │
T1.3 (cli.rs)  ─────┼──> T2.1 (app.rs handlers)                               │
                     │                                                         │
                     └──> T3.2 (mcp.rs)  ──────────────────────────────────────┘
```

Execution order with maximum parallelism:
1. T1.1 + T1.3 in parallel
2. T1.2 + T2.2 in parallel (both depend on T1.1 only)
3. T2.1 (depends on T1.2 + T1.3)
4. T3.1 + T3.2 + T3.3 in parallel (T3.1 depends on T2.1+T2.2, T3.2 depends on T1.3, T3.3 depends on T1.1+T1.2)
5. T4.1

## Risk Register

| # | Risk | Impact | Likelihood | Mitigation |
|---|------|--------|-----------|------------|
| 1 | `fold_story` signature change affects many callers | Medium | Low | `story_type` is added to StorySnapshot struct with `#[serde(default)]`; fold just needs a new match arm. No signature change to `fold_story` itself. |
| 2 | `build_story_views` progress computation adds latency | Medium | Low | Story map is already loaded. `compute_progress` is O(children) per story, no additional I/O. Compute unconditionally — cheap relative to existing disk I/O. |
| 3 | `Invocation::New` field addition breaks pattern matches | Low | High | One match arm to update. The compiler will catch it. |
| 4 | `StoryView` progress field breaks existing JSON consumers | Medium | Low | `skip_serializing_if = "Option::is_none"` means field is absent when null. Backward compatible. |
| 5 | `types.toml` auto-creation races with concurrent commands | Low | Low | `ensure_types_file` checks `exists()` before writing. File lock via `with_project_lock` protects write commands. |
| 6 | Old archived snapshots fail deserialization with new `story_type` field | High | Low | `#[serde(default)]` on `Option<String>` ensures old JSON blobs without `story_type` deserialize as `None`. Add regression test with fixture blob. |
| 7 | `epic create` returns snapshot without type if second event fails | Medium | Low | Both events written within same `with_project_lock` closure. If `StoryTypeSet` write fails, `StoryCreated` already persisted — story exists untyped. User gets error, can `story set --type epic`. Acceptable degradation. |
| 8 | GitHub sync silently drops `story_type` on push/pull | Medium | Likely | Deferred per design (Issue Types API immature). Document as known limitation. `RemoteSnapshot` is separate from `StorySnapshot`. |
| 9 | `doctor --fix` cannot remediate unknown types on archived stories | Low | Possible | Doctor should report but not attempt to fix archived story type issues. Events are in SQLite snapshots, not writable JSONL. |
| 10 | `story show SH-1` loads ALL stories for progress rollup | Medium | Possible | `story_view_response` already loads everything via `build_story_views`. Progress computation adds negligible cost. Benchmark with 200+ stories if concerned. |

## Scope Boundaries

### IN scope
- StoryTypeSet event and fold
- TypeDef struct and types.toml lifecycle
- CLI commands: type list/add/remove, epic create/add/list/show
- --type flag on new/list/set
- MCP tool schema updates for story_type param
- Progress rollup for parent stories (direct children only)
- Parent skip in story next
- Doctor type integrity check
- Human + JSON output for type + progress
- Default type resolution at display time

### OUT of scope (captured for future work)
- TUI changes for type display (per DESIGN.md: deferred)
- GitHub sync for types (per DESIGN.md: deferred, Issue Types API immature — known data-loss on sync)
- Auto-typing in decompose (per DESIGN.md: not doing)
- `story summary`/`story context` type breakdown (DESIGN.md open question)
- `story decompose` type annotations like `[EPIC]` markers (DESIGN.md open question)
- Color/icon/role fields on TypeDef (per DESIGN.md: premature)
- Recursive progress rollup beyond direct children
- `generate_claude_md`/`generate_agents_md` updates for `--type` flag (AI agent discovery)

## Task Sizing Concerns

- **T2.1 is the largest task** (estimated large). It touches many match arms in the `run()` function. If it proves too large for one session, it can be split into:
  - T2.1a: Type handlers (TypeAction::List/Add/Remove) + type validation on New/SetFields
  - T2.1b: Epic handlers (EpicAction::Create/Add/List/Show) + --type filter on List
  This split is optional; the task is coherent as-is since all handlers follow established patterns.

- **T3.1 touches two files** (domain.rs and app.rs). This is acceptable because the domain.rs changes (has_children, compute_progress) are small pure functions, and the app.rs changes are targeted insertions into existing functions.

## Test Strategy

Tests are integration tests in `tests/story_types.rs`, following the project's `assert_cmd` + `tempdir` pattern. No mocks — all tests run the real `story` binary against real filesystem state.

### Test Groups (50 tests total)

| Group | Count | Component | Key Tests |
|-------|-------|-----------|-----------|
| types.toml lifecycle | 9 | storage.rs | init creates defaults, add/remove CRUD, reserved slug "none" rejected, remove in-use rejected, auto-create on first use |
| StoryTypeSet fold | 5 | domain.rs | fold sets type, fold without event yields None, type can be changed multiple times |
| Backward compat | 2 | domain.rs | old JSONL without StoryTypeSet folds cleanly, default type resolution at display |
| --type filter | 4 | app.rs | filter by type, filter --type none, combined with --priority |
| story next skip | 3 | app.rs | skips parent, skips when all children closed, skips multiple levels |
| Progress rollup | 7 | domain.rs/app.rs | parent shows progress, updates on child close, 100%, includes archived, no children = None, direct only |
| Epic sugar | 5 | cli.rs/app.rs | create sets type, two events emitted, add creates relationship, list shows epics, show delegates |
| Doctor | 1 | app.rs | unknown type flagged as integrity issue |
| MCP tools | 3 | mcp.rs | create/update/list accept story_type param |
| JSON output | 2 | output.rs | type field in JSON, progress object for parents |
| CLI parsing | 5 | cli.rs | missing args errors, bad subcommands |
| E2E workflow | 1 | all | full epic lifecycle: create, add children, track progress, complete |

### Test Integration with Waves

- **Wave 1**: Unit tests in domain.rs (fold tests). CLI parser tests in T1.3.
- **Wave 2**: Integration tests become runnable as handlers are wired up.
- **Wave 3**: Progress rollup, next-skip, doctor, and MCP tests become functional.
- **Wave 4**: Full test suite runs: all 50 new tests + existing 535 tests must pass.

### Key Design Decisions

- `remove_type` checks ALL stories (open + archived), matching `remove_state` precedent — prevents doctor integrity issues after removal
- Progress rollup computed unconditionally in `build_story_views` — no new flag needed, cost is negligible relative to existing disk I/O
- `epic create` two-event pattern: both events within same `with_project_lock` closure. If second event fails, story exists untyped (acceptable degradation)
