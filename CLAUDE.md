<!-- semver:start -->
## Semantic Versioning

This project uses semantic versioning managed by the `/semver` plugin.

### Version Awareness
- Read the `VERSION` file at the start of each conversation to know the current version.
- Read `.semver/config.yaml` to understand the versioning configuration.
- When discussing releases, deployments, or changes, reference the current version.

### Commit Discipline
- Write meaningful, descriptive commit messages. Each commit message may appear in an auto-generated changelog.
- Use conventional-commit-style prefixes when they fit naturally: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- The first line of the commit message should be a concise summary (under 72 characters). Add detail in the body if needed.

### Version Bump Guidance
When recommending or performing a version bump:
- **patch** (0.0.x): Bug fixes, documentation corrections, minor refactors with no behavior change.
- **minor** (0.x.0): New features, new capabilities, non-breaking additions to the public API or user-facing behavior.
- **major** (x.0.0): Breaking changes — removed features, changed interfaces, incompatible API modifications, behavior changes that require consumers to update.

When you notice the user has completed a logical unit of work, suggest running `/semver bump` with the appropriate level.

### Configuration
Versioning settings are in `.semver/config.yaml`. Do not modify this file unless the user explicitly asks to change semver settings.
<!-- semver:end -->

## Rearchitecture roadmap

Story data is moving out of per-repo `.storyhook/` directories into one global SQLite store
behind a local daemon. Design of record: [`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md).
Execution state — wave status, step log, discovered defects — lives in
[`docs/rearch/STATE.md`](docs/rearch/STATE.md); read it before resuming this program.

| Wave | Scope | Status |
|---|---|---|
| W0 | quality-gate repair, shared test harness, baseline capture | **complete — merged** |
| W0b | wire-serializable envelope + the `Invoker` seam | **complete — PR open** |
| W1 | `Store` trait, SQLite engine, migrations, rebuild-diff | entry-ready |
| W2a–d | services over the store (`app.rs` frozen) | pending |
| W3 | legacy importer (`story migrate`) — also W4's rollback path | pending (parallel with W2) |
| W4 | **the flip**: the global store becomes the default | pending; one uninterrupted session |
| W5 | daemon promotion + `/api/v1/invoke` transport | pending |
| W6 | git features re-pointed; full commit-body scanning | pending (gated on W4) |
| W7 | migrate this repo; retire `.storyhook/` | pending |
| W8 | crash, concurrency, and corruption hardening | pending |

Standing rules for every wave:

- Every commit passes `make test`; history stays bisectable and two-hats clean.
- Story IDs belong in commit **bodies**, never subjects — a subject reference makes the
  post-commit hook re-dirty the tree.
- Waves end at "PR opened". The work happens in a linked worktree: no version bumps, no
  deploys, no direct pushes to `main`, no force-pushes.
- Deviations from the spec get recorded in STATE.md rather than edited into the spec.
