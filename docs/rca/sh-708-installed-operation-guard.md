# SH-708: Installed operation classification (v2.4.2)

The installed-path hook classified a verified launcher invocation of `reset`
as an unproven artifact edit because its argument contract omitted the verb.
`create` and `unclaim` had the same omission. Story mutations do not imply
installed-file mutations: the installer-owned entry point and the helper's
actual resource targets are separate proofs.

## Operation contract

The hook admits a bounded, single-command spelling of reset, unclaim and
create through the byte-verified Codex launcher or the unredirected helper
beside the executing hook. Existing interpreter and project-selector forms
remain supported. Duplicate/unknown options, missing values, conflicting
comment or description modes, managed operands and unsupported shell syntax
remain refused. Text with shell punctuation can use a description file;
this change does not introduce a general shell parser.

Shared completion preparation now checks repository, common Git metadata and
worktree targets against the installer-written managed-path manifest before
fetches, release or cleanup. It compares lexical and canonical identities by
path components, including removal targets containing an installation.
Configured traversal and symlinked parents cannot bypass it. Missing registries
retain the uninstalled-host behavior; invalid existing registries fail loudly.
`--force` does not override `installed-artifact-resource` refusals.

Path resolution permits missing suffixes but rejects other resolution errors.
Strict resolution of existing ancestry supports older Python versions without
depending on the recently introduced `ALLOW_MISSING` API. Python documents that
default non-strict resolution can suppress errors; it is insufficient evidence
for this resource check. [Python path documentation](https://docs.python.org/3/library/os.path.html)

This is a preflight against the observed filesystem, not a race-proof security
boundary against concurrent hostile filesystem changes. The hook still returns
`{}` for admitted forms; host authorization and all reset resource guards apply.
No installed file, override marker, provider configuration or release version
is changed by this repair.

## Documented router audit

| Verbs | Disposition |
|---|---|
| `reset`, `unclaim`, `create` | SH-708 adds argument admission and regression coverage. |
| `dispatch` | Existing bounded domain-operation admission retained. |
| `context`, `list`, `view`, `capabilities`, `ensure-cli` | Existing reader admission retained; SH-712 owns reader omissions. |
| `capture`, `doctor` | Existing terminal/domain admission retained; doctor fix flags remain refused. |
| `handoff`, `triage` | Reader omissions owned by SH-712. |
| `sync` | Commit-to-story synchronization; existing refusal retained, no new contract in SH-708. |
| `complete`, `reap`, `submit` | Existing refusal retained; lifecycle/lease contracts remain separate. Shared preparation gains target protection. |
| `notify` | Session delivery/interrupt contract remains separate; existing refusal retained. |
| `scaffold-agents-md`, `scaffold-claude-md` | Write-capable path selectors remain refused. |

## Validation evidence

- Installer-generated launcher regression reproduced the original denial.
- Resource checker stub failed 19 assertions; implementation passed the initial
  ten filesystem tests, then an added unreadable-registry case.
- Tests exercise both entry points, both normalized host payloads, interpreter
  spellings, project selectors, malformed arguments and redirected identities.
- Real CLI/daemon/Git fixtures execute create, unclaim and reset, assert refusal
  before claim release for managed targets, and compare installed inventories.
- All resources are fixture-owned; live stories are never reset.

The actual-tree selector returned `ALL` because certified baseline tree
`777296317b7c548d5ce34f7bbd2e54d49ed8ee60` had no coverage map. Directly impacted
tests run locally; the centralized verifier owns full-suite certification.
