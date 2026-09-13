# SH-707 installation and rollback preparation

## Status and ownership

**Supervisor staging and installation are complete.** The repaired development
candidate is installed and disabled; the original Greenlight 3.9.1 producer
remains enabled. Both caches and existing trust are preserved. See
[DEVELOPMENT.md](DEVELOPMENT.md) and the
[safe live receipt](installed-disabled-receipt.json). Only Mikey's native trust
and final selection remain, coordinated by the root supervisor. Do not rerun
staging/install. The procedures below describe earlier isolated preparation.
The personal Codex hook is repaired and revalidated. StoryHook is v2.4.2.
AGE-103 owns Greenlight source delivery; AGE-84 owns candidate/release validation.
Neither a candidate test pass nor an installer receipt certifies a release.

| Artifact | Immutable identity |
|---|---|
| Tagged baseline v3.9.1 | `c25f78e2337013370ac69bc2265d47f2431359c4` |
| Tested AGE-103 candidate | `af73549175847d158c5a59de772f6cfee7b256f5` |
| AI boolean repair | `11f2ae9a2b65fc09659531fc442e30eb31d5e649` |
| Both-host root resolution | `2b5e20ce391e3c72174516928ac2ee5bafa17a15` |
| Provider serialization | `66fe64406f55011ff36cbf368d56f269f5e8c304` |

At the newest dependency read, AGE-103 was verifying at `1305100174dbbc3702fc2af1d3b6a5502200eef3`
with AGE-84 prerequisite fixes incorporated. Its Greenlight subtree is identical
to the pinned candidate. No source-unavailable dependency remains; this run does
not certify their combined central gate.

## Repeat the independent preparation

From the SH-707 worktree, use a new output path:

```sh
TMPDIR=/tmp bash scripts/select-tests.sh
PYTHONDONTWRITEBYTECODE=1 python3 -W error docs/rca/sh-707/test_packaging_evidence.py
PYTHONDONTWRITEBYTECODE=1 python3 -W error docs/rca/sh-707/smoke_install.py \
  --agentics-repo /Volumes/Code/mikeyward/agentics/.codex/worktrees/AGE-103 \
  --output-directory /tmp/sh707-install-new-run
PYTHONDONTWRITEBYTECODE=1 PERSONAL_HOOK=/Users/mikey/.codex/hooks/git-readonly-allow.py \
  python3 docs/rca/sh-707/test_personal_hook.py
```

If the AGE-103 worktree was reaped, supply another Agentics checkout containing both
objects. The exporter reads committed blobs, not concurrent worktree edits, and
uses Git's `--no-replace-objects` to prevent local replacement views from redefining
receipt identity. It rejects existing destinations, moving refs and unsafe entries.
[Git object replacement documentation](https://git-scm.com/docs/git-replace)

The smoke creates its own HOME and CODEX_HOME without copying credentials or
configuration. It registers a one-plugin marketplace, then runs the real installer
for baseline, candidate, and rollback. Changing a registered marketplace's source
requires the normal marketplace remove/add commands; re-adding a different path
without removal was experimentally refused. This removal is confined to the
fixture's one-plugin marketplace, with no live host attached.

Each stage checks every plugin file's SHA256 and Git executable mode, installer
identity, enabled state and registration. Tests execute the actual installed
manifest command. Payload commands are inert input; only HTTP is stubbed. The
candidate's Codex cases use PLUGIN_ROOT alone, including paths with spaces.
Historical baseline Codex cases supply CLAUDE_PLUGIN_ROOT because that manifest
does not support Codex's root variable. Denials retain their reasons; warnings
retain context; valid AI false must reach AI_RESULT rather than silent fallback.

The source suite was run separately, using the exported candidate from the first
rehearsal and Agentics' isolated-store wrapper:

```sh
TMPDIR=/tmp bash tests/with-isolated-store.sh bash \
  '/tmp/sh707-install-rehearsal-01/candidate marketplace/plugins/greenlight/tests/run-tests.sh'
```

That is the directly affected Greenlight suite, **not** Agentics' full repository
suite. The StoryHook selector returned ALL because its certified baseline lacks a
coverage map; the full suite remains central-verifier owned.

## Historical production installation boundary

The supported installer is `codex plugin add`, not `codex plugin install`.
Codex 0.154.0 successfully replaced same-version fixture bytes, so an installer
success and version string alone cannot establish identity.

The [official local update procedure](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/plugin-creator/references/installing-and-updating.md)
uses a distinct manifest cache identity before reinstalling. The operator has
now authorized a separate disposable development artifact identity, while still
prohibiting Agentics checkout version operations. SH-707 must not install the changed candidate
as the unchanged tagged 3.9.1 release, remove a manifest to obtain a misleading
`local` identity, or patch an installed cache.

The original delivery choices were:

| Delivery | Required evidence |
|---|---|
| Containing release | Approved source commit, truthful version, passed owner release validation, and registered source containing the repairs |
| Interim development artifact | Supervisor reviewed, staged and installed; candidate disabled and original enabled. Current evidence and remaining native action are in DEVELOPMENT.md |

The remaining boundary is Mikey's native trust and final selection. Supervisor
review, staging, installation and 20 actual installed controls are complete. The interim
artifact is not release-certified. Retire it after the containing
release is installed and passes the same byte and manifest checks. AGE-103 owns
lasting source delivery; SH-707 retains installation and retirement evidence.

The supervisor completed the recorded identity, inventory, enablement and
preservation checks. The live installed candidate passed all 20 manifest controls.
The original producer and cache remain intact; raw private backups are retained
by the supervisor. The earlier source-staging and plugin-add commands are
historical, not remaining work.

## Rollback and completion

The original one-plugin rehearsal proved reinstall-based rollback. The subsequent
native-toggle rehearsal in [DEVELOPMENT.md](DEVELOPMENT.md) supersedes that method
for the live host: retain both caches and select the already-installed original
through the supported native UI. No cache removal or reinstall is needed.
Rollback restores the known Codex bare-allow defect and is recovery, not acceptance.

Retained receipts are [installer-receipt.json](installer-receipt.json) and
[installer-commands.json](installer-commands.json). Their temporary paths describe
the completed run; the pinned Git objects and scripts make it reproducible.
All receipts explicitly set release_certified and active_host_validated to false.

Once live delivery and host validation succeed, record their separate evidence,
commit the final report, and make `story move SH-707 verifying` the last worktree
action. Until then, leave SH-707 open with its concrete external delivery boundary.
