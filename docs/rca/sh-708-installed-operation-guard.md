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

## Literal report sibling

The hook also treated paths in document content as file operands. An automated
quoted-heredoc report reproduced the same refusal before the second fix.

The new recognizer accepts one `cat` (including `/bin/cat` and `/usr/bin/cat`),
one `>` or `>>` literal destination, and one fully quoted identifier delimiter,
in either redirection order. The first exact terminator must end the program,
apart from blank lines. The body can contain arbitrary report text, including
apparent substitutions, because Bash does not expand quoted-delimiter bodies.
[Bash heredoc semantics](https://www.gnu.org/s/bash/manual/html_node/Redirections.html)

Output identity uses the same resource checker as cleanup; relative destinations
require an absolute payload `cwd`. Managed paths, symlink aliases, special-file
destinations and invalid path evidence are refused. Header expansions, extra
redirections, pipelines, executable suffixes, unquoted delimiters and `<<-`
remain outside this bounded grammar. A body is never stripped out of an
arbitrary shell command to manufacture a safe-looking remainder.

Regression tests cover both normalized host payloads, quoting and redirection
orders, real literal report writes, installed-hook packaging, managed output
aliases, and shell compositions. Real execution proves body substitutions stay
literal and installed artifact inventories remain unchanged.

The suffix uses Bash blanks (ASCII space/tab), not Python Unicode whitespace.
A nonbreaking-space suffix regression failed against the initial recognizer;
it and a carriage-return suffix are now denied.

## Focused validation

| Check | Result |
|---|---|
| Combined installer suite | 60 passed, including installed report execution. |
| Hook suite | 10 passed; rerun after the suffix correction. |
| Resource filesystem cases | 11 passed, including unreadable and special registries. |
| Existing reset/unclaim/completion/reap scripts | 9 passed. |
| Rust formatting, shell syntax, diff whitespace, targeted Clippy | Passed; warnings treated as errors. |

The original launcher denial, quoted-report denial, stub resource checks and
nonbreaking-space suffix each supplied failing regression evidence before their
corresponding corrections. The full suite and delivery remain the verifier's
responsibility.

## Verifier merge reconciliation

PR 806 conflicted with base `5315307b39d04ca369a36c6400ec68890fa60f8b`
after SH-712 added installed reader grammar. A two-parent merge preserves
published SH-708 head `36cb3d076367c0caf398b2c59aae663f10b6bbc8` and retains
both argument classifiers: context/handoff/triage readers and bounded domain
operations. Literal report handling and resolved-resource checks are unchanged.

The combined negative matrix retains SH-712's malformed reader cases and
removes only the three valid create/reset/unclaim forms now covered by SH-708's
positive matrix. Existing real-helper tests exercise both contracts together;
this reconciliation adds no new command grammar or resource policy.

After reconciliation, `cargo test --offline --test plugin_install --test
protect_install_hook --test hook_budgets` passed all 77 tests (61/10/6), including
the resource filesystem contract and real domain/reader/report flows. Targeted
Clippy with `-D warnings`, Rust formatting, hook shell syntax and diff whitespace
checks passed. The actual-tree selector again returned `ALL` for the missing
baseline coverage map; full-suite certification remains with the verifier.
