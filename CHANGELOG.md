# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [v1.0.0] - 2026-07-29

### Breaking
- migrate the tracker to the store and retire .storyhook (bdbb60f)
- move story data to a single global SQLite store (4b932ed)

### Added
- commit-link idempotency becomes a database constraint (44ab201)
- live updates come from the store; the notify dependency dies (7288e70)
- the /api/v1/invoke transport, and the client that speaks it (32e641e)
- lifecycle — portfile, pidfile lock, hello, and auto-spawn (74bd28e)
- the dashboard serves the store through the service layer (c4365c3)
- undo by compensating events, not by rewriting the past (ca4f4f7)
- tell an unmigrated repository to migrate, rather than to init (c50a650)
- resolve the project by walking up from the working directory (ab6b2ff)
- fold repository config into the pointer file, and adopt the registry (1c512d0)
- story migrate with repair-on-import (6f817c3)
- read-only reader for the legacy on-disk format (036c768)
- env-selected invoker and the store test leg (80e8233)
- catalog web arms and session utilities (40ba417)
- github-sync storage edges behind the store (1babe06)
- git commit-sync service (1be07fe)
- export, import, and decompose over the store (c2b214c)
- ProjectSnapshot bulk read for seam clients (b937e8c)
- integrity service with the rebuild-diff doctor (eb6aa63)
- query service over ReadOps (e9e170e)
- grouping service for phases and epics (ddfe113)
- system service for scaffold, plugin, and hooks (b4e7598)
- configuration service over column-backed state and type tables (afa2bca)
- project service with atomic init (4056817)
- relation service with single-transaction symmetry (fa4d610)
- dispatch skeleton, service context, and story lifecycle (c67642b)
- the store conformance suite over the SQLite engine (e792a3e)
- rebuild-and-diff oracle and the fault-injection points (33c9454)
- schema v1 and the migration framework (a7a8cc6)
- wire-serializable Response, Invocation, and error envelope (2db8310)
- add --auto flag to dispatch for autonomous sessions (1e53c02)
- add the statuses editor modal (b7b125f)
- add the per-repo statuses editor to the dashboard (a3f3618)
- expose the per-repo state configuration API (2acb6e3)
- add story state list, set, and reorder (81813af)
- add state edit, reorder, and usage operations (034aae1)

### Fixed
- tell people to stop the daemon before restoring a snapshot (9a25b2f)
- give every call to a daemon a deadline (92c7b50)
- stop offering a sync mode nothing implements (c19ce50)
- the restore instructions were incomplete, and stopped short (6fef03e)
- adopt the identity a clone's pointer file already names (20b9a8f)
- name the pointer file when it cannot be read (e955611)
- say a damaged store is damaged, and where the snapshots are (912b205)
- refuse to resolve a real data home in a test build (1d3fcfa)
- the story CLI does have a --version flag (27de975)
- make the session hooks' kill switch actually switch (26480e2)
- two processes migrating one store must both survive it (a72f3da)
- the post-merge hook reads whole commit messages (2d28aa5)
- commit-sync scans the whole commit message, not the subject (7091c17)
- run-store-leg aborts when given no exclusions (46e9e13)
- stop two concurrent gates racing on one fixture name (75dd9c1)
- make migration safe under concurrency (9aefd45)
- read the journal mode before writing it (c30c0f7)
- roll back a transaction whose COMMIT failed (d3ee37c)
- a story moved back into an open state stops being closed (845afb9)
- give the priority slug a constraint of its own (aac3a5c)
- stop `story export --json` double-encoding its document (d272a7b)
- answer a help flag with help, for every verb (e2531c4)
- send plain-text errors to stderr, not stdout (da3109d)
- bound the tailnet probe so a wedged CLI cannot mute the dashboard (e8d4cf8)
- give tests kernel-assigned ports and a real readiness signal (4c1aed9)
- create web_test fixtures outside the indexed tmpdir (2316ddf)
- push live updates when a project's states change (101249d)
- keep state descriptions when rewriting states.toml (995c082)

### Changed
- Merge pull request #73 from mikeydotio/chore/plugin-manifest-bump (7276e08)
- Merge pull request #72 from mikeydotio/rearch/w8-hardening (80d03e6)
- Merge pull request #71 from mikeydotio/rearch/w7-cutover (0b25838)
- drop the worktree-anchoring workarounds the store made obsolete (832ce94)
- Merge pull request #70 from mikeydotio/rearch/w6-git-features (5e209b2)
- delete the legacy write path the flip quarantined (cf80e54)
- Merge pull request #69 from mikeydotio/rearch/w5-daemon (30176c2)
- extract the HTTP plumbing and the tailnet identity (a5320d9)
- resolve the environment once, in main, and pass it down (5996898)
- Merge pull request #68 from mikeydotio/rearch/w4-flip (01d8332)
- retire the store test leg and quarantine what the daemon wave deletes (eef531d)
- Merge pull request #67 from mikeydotio/rearch/w3-importer (33991c3)
- Merge pull request #66 from mikeydotio/rearch/w2d-git (7eccae2)
- Merge pull request #65 from mikeydotio/rearch/w2c-query (59ee60c)
- drive the TUI through the Invoker seam (8bf7eeb)
- Merge pull request #64 from mikeydotio/rearch/w2b-config (da446e9)
- Merge pull request #63 from mikeydotio/rearch/w2a-lifecycle (67b516a)
- Merge pull request #62 from mikeydotio/rearch/w1-store (271c7cf)
- Merge pull request #61 from mikeydotio/rearch/w0b-envelope (2e103ac)
- route the CLI and web server through the Invoker seam (ef717f2)
- Merge pull request #60 from mikeydotio/rearch/w0-gate-repair (b1b3f8c)
- migrate three proof files to the shared harness (c444313)
- move the scratch/readiness/daemon helpers into test-support (a170106)
- Merge pull request #59 from mikeydotio/worktree-sto-SH-62 (b1f3238)
- Merge pull request #58 from mikeydotio/chore/commit-link-records (838d68a)
- Merge pull request #57 from mikeydotio/chore/reconcile-main-sync (f8645a0)
- Merge pull request #56 from mikeydotio/chore/track-sh-58 (6c3e2da)
- Merge pull request #53 from mikeydotio/worktree-sh-41 (3d67bf2)
- Merge origin/main into worktree-sh-41 (2ba5eec)
- Merge pull request #55 from mikeydotio/chore/track-sh-56 (e170a63)
- Merge pull request #54 from mikeydotio/chore/makefile-install-target (84b64e1)
- Merge pull request #52 from mikeydotio/chore/track-sh-51-53 (c7f0d19)
- Merge pull request #51 from mikeydotio/chore/release-v0.17.0 (0aa0094)
- group the state verbs under Invocation::State (e1bc48b)
- extract the state-transition event batch (3f58ff3)

### Documentation
- correct what is actually running on this machine (dd486f2)
- record the hardening wave, and close the program (1ac450f)
- say which binary is refusing, and how to get a usable one (949e3ae)
- record W7 — the tracker is in the store and the ledger is filed (8a57466)
- describe the storage model storyhook actually has (fd844d1)
- record W6 — the quarantine deleted, the git features fixed (6e9a3da)
- record W5, hand off the quarantine deletion (fe15698)
- record W4, close the flip checklist, hand off to the daemon wave (b6e8ca5)
- record W3, the SH-60 ruling, and the W4 rollback procedure (a5e5f26)
- record W2d, the completed roster, and the store leg (6f24967)
- record W2c, the roster delta, and two defects (dbe8387)
- record W2b and the service API the remaining waves build on (a02b990)
- correct the W2a gate timings to the warm steady state (a9cd168)
- record W2a and the service API the next waves build on (96cadde)
- mark W0b merged in the ledger (f04792b)
- record W1 and the store API the service wave builds on (c8e6f87)
- record the orchestrator merge policy (8ffee70)
- mark wave 0 merged (639111a)
- record W0b in the execution ledger (70e2ae7)
- record the wave-0 pull request link (c01c116)
- add the rearch roadmap and wave-0 handoff (a3937a9)
- enumerate the W4 flip checklist (34ef1cb)
- commit the data-layer rearchitecture spec (d327894)
- capture the pre-rearch baseline (1a6eb8b)
- record W0.2 and the harness API W0.3 builds on (6320609)
- record the W0.1 gate fixes and two follow-up defects (5270cb4)
- document the --auto flag and STORY_AUTO_PROMPT (d3a6f70)
- add execution continuity ledger (28ec480)
- document configuring a project's statuses (4384ccc)

### Testing
- a fixture that is about the file does not get a daemon (00821dd)
- the corruption matrix, starting with what already works (5319bcb)
- concurrency soak — mixed local and daemon clients under load (c4c6325)
- kill -9 at every fault point, against every shape of write (82a82d3)
- prove commit-sync's churn loop terminates, and fix the PATH trap it exposed (2b279f0)
- carry differential_git's behavioural rows into service_git (eaf625a)
- write legacy fixtures directly instead of through app::run (b95dce6)
- make test-daemon, daily backups, and three defects it found (44c5bf7)
- un-ignore the pair the whole program exists to turn green (1445291)
- burn down the store leg's exclusion list before the flip, not after (1e70f58)
- the pre-flip old-versus-new --json diff harness (e8c1d1c)
- pin the empty-window commit's date so the two legs cannot race (ff634f8)
- a corruption-fabrication API and store-default fixture helpers (e97ca5f)
- the reverse path and the round-trip guarantee (a5871a6)
- differential rows for the config and project families (1354798)
- differential harness proving legacy equivalence (757bc7b)
- property tests for the event-sourcing invariants (9f7d0e4)
- add the regression-baseline capture script (4179f48)
- pin the error-code and export round-trip contracts (6a86c4b)
- freeze the CLI surface in a golden snapshot corpus (b7539f3)
- prove two checkouts of one repo are separate databases (9aa8df1)
- isolate the bash suite's data home (f4eefe1)
- cover dispatch --auto and the stricter arg parsing (5ebccc7)

### Maintenance
- bump the manifest to 0.4.0 and state the CLI requirement (3cbd08a)
- add `make gate`, and let the baseline capture survive the cutover (12fd848)
- record the narrowing datum from the importer's relation-repair work (e70a632)
- pack split debuginfo to stop the deps-dir pileup (fb9b2f9)
- derive PartialEq on the value types the store compares (ebb7522)
- ban $TMPDIR fixtures and mark the files still using them (ca2b4a7)
- convert to a cargo workspace with a test-support crate (877c31f)
- defer web-UI stories and block daemon-dependent stories during the rearchitecture (be1601c)
- record commit links and file the churn-loop defect (e07a0b2)
- file SH-59 and SH-60, two defects found reconciling main (b60db32)
- record commit links from commit-sync (21258cf)
- file SH-58, commit-sync's subject-only commit scan (c2a929a)
- record commit link from post-commit hook (c54eef2)
- close SH-55, track SH-56 (20017ac)
- record commit links from post-commit hook (74869d4)
- add `make install` target using install(1), not cp (79e5cd7)
- track SH-54, SH-55 (7e6e69e)
- close SH-41 and SH-49 as done (f59d072)
- track SH-51, SH-52, SH-53 (e35a433)
- start SH-41 and file the states.toml defect (131512c)

_[manual]_

## [v0.17.0] - 2026-07-25

### Added
- route the new verbs through the /story skill (76f3650)
- add doctor and capture verbs to story.sh (eeb7eb7)
- add view, list, and create verbs to story.sh (8bf7721)
- add the complete verb to story.sh (6364b3b)
- add /story do dispatch actuator with ready-state gate (ed4f23d)
- vendor session-lifecycle lib for /story do (03f8e63)
- add /story router dispatching to story-* subskills (6313b01)
- sync description as the issue body's free text (c24a9c4)
- add description to the create form and detail editor (7c45387)
- add description, priority, and label fields to new-story modal (f577cad)
- expose distinct labels in dashboard meta (ce0ba42)
- support description/priority/labels/assignee on story new (9e4a21e)
- add first-class story description field (c25be1d)
- advertise the MagicDNS name in web start/status/address (4b73c7a)

### Fixed
- anchor story.sh's CLI calls to the project root (d6cbce4)
- exclude deleted stories from the dashboard's story feed (fae158d)
- renumber new stories off colliding SH-31-34 IDs (9eafc3a)
- repair SH-20 soft-delete state inconsistency (717a3aa)
- validate assignee in SetFields --json branch (faa0667)
- validate & normalize assignee in create/assign paths (0f888e0)
- trust the MagicDNS FQDN for tailnet mutations (bbe0034)

### Changed
- Merge pull request #49 from mikeydotio/chore/close-sh-45 (d08af8e)
- Merge pull request #48 from mikeydotio/worktree-sh-45 (e15bee0)
- Merge pull request #47 from mikeydotio/chore/track-sh-45 (af0fbbd)
- Merge pull request #46 from mikeydotio/fix/44-web-dashboard-hides-deleted-stories (977c8bd)
- Merge pull request #45 from mikeydotio/chore/fix-clippy-lints (6506f27)
- Merge pull request #43 from mikeydotio/fix/storyhook-story-id-collision (e76b2f7)
- Merge pull request #42 from mikeydotio/worktree-sto-40 (24c42cb)
- Merge pull request #41 from mikeydotio/worktree-sto-39 (8e8958a)
- rename storyhook plugin/skills to the story namespace (ce107d0)
- extract CreateStory/AssignStory mutations into fns (231dabd)
- Merge pull request #38 from mikeydotio/feat/web-new-story-fields (721763e)
- Merge pull request #37 from mikeydotio/fix/35-tailnet-magicdns-mutation-guard (92d3188)
- Merge pull request #34 from mikeydotio/chore/release-v0.16.0 (92ac402)

### Documentation
- document the /story lifecycle verbs (9a645e8)
- document the new story new/set flags in help text (e5b91be)

### Testing
- add bash harness for story.sh dispatch (fab7da2)

### Maintenance
- close SH-45 as done (e44c5fd)
- link PR #48 on SH-45 (fca171a)
- record SH-45 delivery summary (2c44a65)
- bump the story plugin to 0.3.0 (4a548c2)
- file three plugin defects found while scoping SH-45 (d2fce75)
- finalize git-sync comments for new story (56f5b64)
- sync SH-45 git-linked comment (90703c4)
- track SH-45 (23da8ba)
- fix clippy lints blocking make test (5286a5b)

_[manual]_

## [v0.16.0] - 2026-07-21

### Added
- add --if-state CAS guard to story move (30ab541)

### Fixed
- account for soft-deletion in --if-state CAS guard (728a7bd)
- give --if-state an unambiguous parse boundary in story move (f5fdc9b)
- close CAS review gaps in --if-state move guard (b3dfa12)
- run cargo-version sync as a pre-bump hook (1896992)

### Changed
- Merge pull request #33 from mikeydotio/feat/move-if-state (3a71fac)
- Merge pull request #32 from mikeydotio/fix/semver-sync-cargo-pre-bump (838c611)
- Merge pull request #31 from mikeydotio/chore/release-v0.15.0 (1f7d880)

_[manual]_

## [v0.15.0] - 2026-07-20

### Added
- add `story web open` and `story web address` (b1a73db)
- animate live dashboard updates by type of change (0caceaa)
- push live story updates to the dashboard over SSE (71e6659)
- add home/repo/settings screens to the dashboard frontend (cb185d8)
- make the dashboard registry-backed, one global daemon (808efe9)
- add ~/.storyhook/registry.toml repo registry (174d8d9)
- board + list dashboard with drawer and drag-and-drop (a828e0f)
- add mutation API with CSRF/DNS-rebinding guard (bb4351b)
- add GET /api/story/{id} and ordered /api/data meta (06fbafc)

### Fixed
- replace stray NUL byte in dashboard fingerprint separator (dcb71f2)
- supply missing force field in reopen route (735892a)
- supply Invocation::Reopen.force in the web reopen route (2a81190)
- bind loopback + tailnet only, never the public internet (a0c8ace)
- deleting a story now closes it, not just archives it (#18) (38f41ca)
- inject session-start context via additionalContext (silent) (70e3dd1)
- make sync-cargo-toml hook portable on BSD/macOS sed (faa0866)

### Changed
- Merge pull request #30 from mikeydotio/feat/web-open-address (1a7ca00)
- Merge pull request #29 from mikeydotio/worktree-sto-21 (431905b)
- Merge pull request #28 from mikeydotio/chore/untrack-stray-worktree-gitlink (142ac9f)
- Merge pull request #27 from mikeydotio/feat/multi-repo-web-dashboard (01635a8)
- Merge pull request #24 from mikeydotio/fix/23-web-reopen-force (8763004)
- Merge pull request #19 from mikeydotio/worktree-sto-17 (fd09e61)
- Merge pull request #22 from mikeydotio/worktree-sto-18 (8f58321)
- dedupe delete_story onto archive_story (f1be5bb)
- centralize security headers, add router scaffold (365d168)

### Documentation
- document the multi-repo dashboard (e9f2243)
- correct Makefile's false claim of CI parity (7d02de4)
- document the interactive dashboard and write API (4d5553b)

### Testing
- retarget grafted #23 reopen regressions at registry-backed API (d161006)

### Maintenance
- untrack stray .claude/worktrees gitlink and ignore the dir (6f911ca)
- bump plugin to 0.2.1 for silent session-start context (8222173)
- bump plugin to 0.2.0 to ship storyhook-update skill (dadd8d5)
- remove test workflow; tests run locally via make test (e362c56)

_[manual]_

## [v0.14.0] - 2026-07-03

### Added
- add storyhook-update skill (cba8213)
- add `story update` self-update command and `--version` flag (0b57ccf)
- add storyhook-install skill and CLI-presence guards (ae8d2de)
- register via Claude Code marketplace instead of copying (5274a65)
- implement web dashboard (story web start/stop/status) (5b6afe9)

### Fixed
- avoid a python3 spawn on every non-git Bash call (F074) (7cba1d6)
- no-op Stop-hook handoff when a forge pipeline is active (F072) (995b6e1)
- allowlist command -v in storyhook-setup skill (F073) (a11a7a9)
- storyhook-triage emits real verb-first mutations (d2287bf)
- storyhook-work emits real verb-first mutations (2de7edb)
- correct hooks.json to valid Claude Code schema (55f232c)
- address all 5 triage FIX items for web dashboard (2542fb8)

### Changed
- Merge pull request #3 from mikeydotio/worktree-web-ui (0c806ff)
- integrate origin/main into web-ui branch (ec801fe)
- project documentation for web dashboard (24f7488)
- 5 FIX, 0 ESCALATE — all findings have clear solutions (37b5dc7)
- static analysis and test hardening complete (63633e6)
- web dashboard implementation plan approved (9e147c7)

### Documentation
- fix workflow-patterns.md to verb-first command grammar (1636303)
- rewrite cli-reference.md to the real verb-first CLI (5aa6f6d)
- document Claude Code plugin install routes (5beae1c)

### Maintenance
- add Makefile mirroring CI checks (7cdf175)
- satisfy fmt + clippy on current stable toolchain (2801271)

_[manual]_

## [v0.13.0] - 2026-04-07

### Added
- rewrite session-start hook to use story session-start (5634aa9)
- remove MCP references from scaffold templates (6900939)
- add story session-start CLI command (9f98263)
- add --compact and --all flags to help system (871bf2d)
- remove MCP from documentation and plugin files (de6f31c)
- strip MCP server from Rust codebase (c32b117)
- make reserved slug "none" check case-insensitive (55bf68d)
- add story_type to JSON patch dispatch table (f574b7d)
- fix all clippy warnings for clean validation (7686399)
- add type breakdown test for HTML report (1527253)
- add type breakdown to Context handler (ccb2399)
- add type breakdown to summary and report output (35852aa)
- add import validation for story_type against types.toml (aed79cf)
- display "Default" for untyped stories + reserve "default" slug (84558db)
- add story_type to MCP update tool description (8f41b4b)
- add types to export/import and StoryTypeSet on import (5f022b8)
- add story_type param to MCP tool schemas and update handler (6d6988b)
- add progress rollup, parent skip in Next, doctor type check (2810707)
- add Type and Epic command handlers to app.rs (d2b93e4)
- add type + progress rendering to output.rs (d0d65c3)
- add types.toml config lifecycle to storage.rs (87dec1a)
- add TypeAction, EpicAction, Invocation variants, parsers, --type flag (b80ad6b)
- add StoryTypeSet event, TypeDef, ProgressRollup, story_type field (e33c2f0)

### Fixed
- Add CLI alternative to plugin install success message (d6e116d)
- Add CHANGELOG entry for MCP removal breaking change (d67848c)
- Sync Cargo.toml version to 0.12.0 and add post-bump hook (8bd17a6)
- Remove ghost --tree reference from scaffold template (5a40484)
- Add --compact and --all flags to help usage line (a555475)
- RawJson output bypasses --quiet flag (1da9cac)
- proper TOML parsing for plugin-config check (0b49f33)
- UTF-8 safe truncation in session-start (5eeb49f)
- guard against removing last type in remove_type (c665f76)
- sync storyhook state for SH-7 and SH-8 (missed during execute sessions) (0642d07)

### Changed
- pipeline complete — pushed to origin/main (3564d85)
- Fix cycle 5 complete — all 5 ESCALATE stories done (146db71)
- Decomposition complete — 5 ESCALATE stories mapped, plan-mapping.json created, DAG valid (2bbdffb)
- Fix cycle 5 plan approved — 5 ESCALATE stories, 1 wave, ready for decomposition (182adc7)
- ESCALATE review complete — 5 stories approved with recommended approaches, dispatching to plan (6c67314)
- SH-33 complete — 3/8 fix cycle stories done, pausing (b160ca6)
- SH-32 complete — 2/8 fix cycle stories done, pausing (75edc69)
- SH-31 complete — 1/8 fix cycle stories done, pausing (1abd52f)
- Decomposition complete — 8 stories mapped to plan tasks, parent SH-40, DAG valid (78d0e7e)
- FIX cycle 4 plan approved — 8 tasks in 1 wave (81ce55e)
- Project documentation complete — ready for ESCALATE review (1d21db0)
- 0 FIX, 9 ESCALATE (max fix cycles reached) (d8ab754)
- static analysis and validation reports (9b41d66)
- test hardening + 24 new tests (6b414a1)
- all stories complete (603f17d)
- SH-36 complete — session paused (1/1 stories) (1ece1f4)
- SH-35 complete — session paused (1/1 stories) (51fa5a0)
- SH-34 complete — session paused (1/1 stories) (887f4ed)
- SH-33 complete — session paused (1/1 stories) (5ed11ee)
- SH-32 complete — session paused (1/1 stories) (5fb06ba)
- Decomposition complete — 6 stories created (SH-32 to SH-37) across 4 waves (8ca3fb8)
- implementation plan approved — replace MCP with CLI documentation (50ab759)
- all fix cycle 3 stories complete — transitioning to review+validate (77d9661)
- SH-29 done — story_type JSON patch dispatch, session paused (5e51c1f)
- create stories from fix cycle 3 plan (7a5073f)
- fix cycle 3 — 2 FIX items planned, 6 red tests pre-written (0ca5fef)
- 2 FIX, 0 ESCALATE — cycle 2 (2b1b93e)
- test hardening + report -- cycle 2 (af620bb)
- add missing tests and fix unused import warning (92e4790)
- static analysis complete — cycle 2 (2d283e9)
- all stories complete — transitioning to review+validate (5af5c75)
- SH-26 complete — session 5 paused (1/1 stories) (18f799e)
- SH-25 complete — session 4 paused (1/1 stories) (f93a8e6)
- SH-24 complete — session 3 paused (1/1 stories) (7cc23b5)
- SH-23 complete — session 2 paused (1/1 stories) (a0f3eda)
- SH-22 complete — session 1 paused (1/1 stories) (2444dd5)
- create 6 task stories from ESCALATE fix cycle plan (b1ddddb)
- ESCALATE fix cycle plan approved — 6 tasks, 3 waves (d573f23)
- archive cycle-1, record user decisions on 4 ESCALATE stories (e590a24)
- project documentation for Story Types & Epics (301dfa0)
- fix cycle 1 complete — all 3 stories done (0 retries) (3469293)
- remove dead code branch in progress rendering (10ed49a)
- fix cycle 1 — SH-17 complete, session paused (1/3) (3f8e9de)
- fix cycle 1 — 3 stories from triage FIX items (7fd1ba4)
- fix cycle 1 — 3 tasks, 1 wave approved (62be55c)
- 3 FIX, 4 ESCALATE (626d12f)
- add handoff document (b713fc8)
- test hardening + report (9ad2d6b)
- static analysis complete (3933ab4)
- all stories complete (9/9 sessions, 11/11 stories) (630a9b8)
- SH-10 complete, session paused (8/9 stories) (8e49cf5)
- SH-9 complete, session paused (7/9 stories) (e8e449b)
- SH-8 complete, session paused (6/9 stories) (8f660c7)
- SH-7 complete, session paused (5/9 stories) (505af5e)
- SH-6 complete, session paused (4/9 stories) (da76aff)
- SH-5 complete, session paused (3/9 stories) (4761c2d)
- SH-4 complete, session paused (2/9 stories) (c6ba348)
- SH-3 complete, session paused (1/1 stories) (90e820d)
- create stories from plan (5d93266)
- implementation plan approved (a0ad016)
- architecture design approved (8f22f44)
- domain research + team roster (084a94d)
- capture idea — Story Types & Epics (8808f62)

### Testing
- Update test to verify post-bump hook instead of config.yaml (de73026)

### Maintenance
- track tool config (.storyhook, .semver, .planning) (1866a1b)
- sync storyhook state for SH-10 completion (cf9286d)
- track tool config (.storyhook, .semver, .planning) (462e90a)

_[manual]_

## [v0.12.0] - 2026-03-31

### Added
- **Phase support** — organize stories into phases using `phase:N` labels (convention on existing labels, zero storage changes)
- `story phase list` — per-phase progress overview with completion counts
- `story phase show <N>` — list stories in a specific phase
- `story phase add <id> <N>` — assign story to phase (auto-strips old phase assignment)
- `story phase remove <id>` — clear phase assignment
- `story phase create <N> ["<title>"]` — create a named grouping story for a phase
- `--phase <N>` filter on `story list` and `story next` — scope queries to a specific phase
- `story load-context` — renamed from `story context` for clarity; auto-detects phases and includes Phase Progress section
- `### Wave N` in `story decompose` now preserves phase identity via `phase:N` labels (previously lost after import)
- `storyhook_phase_list` MCP tool and `phase` parameter on `storyhook_list_stories` / `storyhook_get_next`
- Phase number validation — must be a positive integer
- 11 new integration tests for phase commands

### Changed
- `story context` renamed to `story load-context` (old name kept as alias)
- Phase counting uses state roles instead of hardcoded "in-progress" — works with custom active states

### Removed
- **MCP server removed** — the built-in JSON-RPC server is no longer part of storyhook
- `story --mcp` flag — no longer available; the MCP server process cannot be launched
- `story mcp-config` command — MCP configuration is no longer needed
- **Migration path**: session hooks via `story session-start` replace MCP for AI agent integration. Run `story plugin install claude-code` to set up hooks automatically.

_[manual]_

## [v0.11.0] - 2026-03-31

### Changed
- **CLI grammar restructured to verb-first** — all story commands now use `story <verb> <id> [args]` instead of `story <id> <verb> [args]`. Old forms removed entirely.
- `story <id> is <state>` → `story move <id> <state>` — industry-standard verb for state transitions
- `story <id> awaits "<reason>"` → `story block <id> "<reason>"` — universally understood verb
- `story <id> awaits --clear` → `story unblock <id>` — symmetric pair with `block`
- `story <id> priority <level>` → `story prioritize <id> <level>` — verb form
- `story <id> label --remove <csv>` → `story unlabel <id> <csv>` — consistent `un-` prefix
- `story <a> <rel> <b> [--remove]` → `story relate <a> <rel> <b>` / `story unrelate <a> <rel> <b>`
- All help topics, AGENTS.md, CLAUDE.md template, cursor-rules template, and git hooks updated to verb-first syntax

### Added
- `story show <id>` — explicit verb for viewing stories (previously `story <id>`)
- `story comment <id> "<text>"` — explicit verb for adding comments (previously `story <id> "<text>"`)
- `story set <id> [--field value ...]` — batch update multiple fields in one command
- `story set <id> --json '{"key":"value"}'` — JSON mode for structured batch updates with field validation
- `story link` / `story unlink` — aliases for `story relate` / `story unrelate`
- 14 new help topics: show, move, block, unblock, set, comment, assign, prioritize, label, unlabel, relate, unrelate, reopen, delete
- Redirect aliases: `story help is` → move, `story help awaits` → block, `story help priority` → prioritize, `story help link` → relate
- 22 new integration tests in `tests/cli_grammar.rs` covering all verb-first commands

_[manual]_

## [v0.10.0] - 2026-03-31

### Added
- Default `in-progress` state (OPEN, role=active) — new projects now ship with todo/in-progress/done out of the box
- `--state <slug>` flag on `story new` to set initial state at creation time
- `state` parameter on `storyhook_create_story` and `storyhook_bulk_create` MCP tools for setting initial state
- `storyhook_bulk_update` MCP tool for batch state changes (bulk close, bulk reopen, bulk transitions)
- `storyhook_add_relationship` MCP tool with `{a, relation, b}` params and enum for all 8 relationship types
- `storyhook_delete_story` MCP tool and `story <id> delete "<reason>"` CLI — soft-delete with required reason, archived with deletion flag for full audit trail
- Nested checklist support in `story decompose` — indented `- [ ]` items create parent-child relationships to their parent checkbox
- Relationship summary in decompose response — shows created relationships after decomposition
- `state` field on `ImportStory` for setting initial state during bulk import

### Changed
- All MCP tool descriptions enriched with relationship type enums, available states, dependency hints (`blocks`/`blocked-by`), and cross-references to related tools
- `storyhook_decompose_spec` description now documents Wave syntax, nested checklists, and inline priority/label markers
- Archive database schema: added `deleted_reason` column for soft-delete audit trail

_[manual]_

## [v0.9.0] - 2026-03-31

### Added
- Two-way GitHub Issues sync via `story github-sync [<id>] [--dry-run]` — full bidirectional sync between storyhook stories and GitHub Issues with three-way merge conflict detection, interactive resolution, and per-story atomicity
- GitHub API client (`ureq` 3.x, synchronous, no tokio) behind `github-sync` cargo feature flag for optional builds without network dependencies
- Fenced `storyhook` YAML code block in GitHub Issue bodies for encoding non-native fields (priority, awaiting, non-native relationships)
- Native GitHub Sub-issues and Dependencies API integration (API version 2026-03-10) for `parent-of`/`child-of` and `blocks`/`blocked-by` relationship sync
- Initial sync setup wizard with import-all, title-match, push-only, and start-fresh strategies
- Configurable sync modes (`off`/`manual`/`auto`) — auto mode triggers per-story sync on any story-modifying command
- Sync state persistence in `.storyhook/github-sync.toml` with base snapshots for three-way merge and pre-sync backups for rollback
- `story github-sync --dry-run` to preview sync changes without applying them
- `storyhook_github_sync` MCP tool for AI agent integration

### Changed
- Renamed `story sync-git` to `story commit-sync` (old name kept as alias for backward compatibility)
- Renamed `storyhook_sync_git` MCP tool to `storyhook_commit_sync` (old name kept as alias)
- New error exit codes: 6 (GitHub auth), 7 (GitHub API), 8 (sync conflict)

_[manual]_

## [v0.8.0] - 2026-03-29

### Added
- TUI drag-and-drop: drag story rows to section headers to move between states (d564d12)
- TUI dependency graph view (`3` key) with Tree, Dependencies, Critical Path, and Focus modes (a82c01b)
- TUI session-only undo/redo via `Ctrl+Z` / `Ctrl+Y` with event snapshot/restore (b08edfb)
- Wave-based markdown format in `story decompose` — `### Wave N` headings auto-generate `follows` relationships between waves (4f9336f)
- `story help json-format` documenting the complete JSON output contract for programmatic consumers (09af4c2)

### Fixed
- `.storyhook/lock` and SQLite WAL/SHM files now gitignored via `.storyhook/.gitignore` created during `story init` (88a6736)

_[manual]_

## [v0.7.0] - 2026-03-29

### Added
- Full-featured terminal UI via `story tui` — dashboard home screen, grouped-table board view with collapsible state sections, story detail modal with inline editing, create form, persistent filter bar, help overlay, and Phase 1 mouse support (b9024ae, 8009ddc, 11079e4, f6fbca8, 1c44348, 7d8ddae)
- `StoryTitleSet` event variant in the domain layer, enabling title editing from the TUI (ba3461d)
- 245 tests covering the TUI (225 unit + 19 integration + 1 performance) (6f3bc0e)
- `story help tui` help topic documenting TUI keybindings and usage (6f3bc0e)

### Fixed
- Title editing in TUI now actually persists via `StoryTitleSet` event instead of writing a comment (ba3461d)

### Maintenance
- Track tool config files (.storyhook, .semver) and gitignore .planning directory (b682963)

_[manual]_

## [v0.6.0] - 2026-03-27

### Added
- Claude Code plugin with 7 skills (setup, context, work, plan, handoff, triage, sync) and 3 session hooks (context injection, git sync, auto-handoff) (09d9f21)
- `story help <command>` extended help system with 18 agent-optimized topics (09d9f21)
- `story plugin install|uninstall claude-code` for one-command plugin management (09d9f21)
- `story init` now generates AGENTS.md by default for universal AI agent discoverability (09d9f21)

### Changed
- All 14 MCP tool descriptions expanded from 1-line to 2-4 sentences with usage guidance and cross-references (09d9f21)

_[manual]_
