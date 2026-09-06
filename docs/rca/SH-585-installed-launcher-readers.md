# SH-585: Installed launcher readers

## Failure and cause

On v2.4.2 the installed-artifact hook denied the exact Codex stable launcher
with `context`, reporting execution as an artifact edit. SH-584 transferred
guard ownership and its failing reproduction to SH-585 before this change.

| Hypothesis | Discriminating evidence | Result |
|---|---|---|
| Executable-only classification rejects a valid reader | Installer-produced launcher plus `context` is denied by the tracked hook before execution | Verified |
| Reader helpers require domain writes | Trace helper startup and query service; execute actual readers against isolated store and compare global event sequence, story data and installed files | Refuted for admitted forms |

`714933513` introduced installed-artifact refusal; `ac877b8ac` added six
ordinary reader basenames but did not classify launcher arguments. This is a
compatibility omission at that classifier, not evidence that a particular
earlier release supported launcher reads. Last known good remains unknown.
The missing command contract produces a false positive at the protection
boundary. Classification: checking, missing; trigger: supported launcher
execution. Surgical repair; no dispatch, installer, or store redesign.

## Supported contract

The exact absolute `HOME/.codex/storyhook/story.sh` must have its parent in
the managed manifest, match the installer-produced bytes, and have no symlink
redirection below HOME. Special files and changed/missing launchers refuse.
The bounded, nonblocking identity read never executes the launcher or calls
the daemon. Canonical macOS ancestry above HOME is permitted.

| Form | Arguments |
|---|---|
| Direct launcher, or `bash`, `/bin/bash`, `/usr/bin/bash` plus launcher | No interpreter flags |
| Optional leading project selector | One `--project value` or `--project=value`; nonempty literal value, separate value cannot start with `-` |
| `context` | No arguments or one `--full` |
| `view` | One `[A-Za-z0-9][A-Za-z0-9_-]*` ID |
| `list`, `ensure-cli` | No arguments |
| `capabilities` | No arguments or one `--agent=claude` / `--agent=codex` |

Only a single simple command qualifies. Raw expansions, shell operators,
comments, multiline input, redirections and substitutions refuse, even when
quoted. Wrappers, assignments, cache helpers, lookalikes, duplicate flags and
extra operands receive no exception. Existing ordinary-reader behavior is
preserved. Structured edits retain their edit diagnosis; unsupported shell
operations say that read-only safety cannot be established.

## Read-path evidence and limits

`context` invokes load-context and optionally graph/list queries; `view`
invokes show twice; `list` invokes ready-list and state-list; `capabilities`
renders catalog JSON; `ensure-cli` looks up the CLI and reads its version.
The sourced libraries and provider configuration do not dispatch work.
`list` creates/removes a temporary diagnostic file, and CLI reads can create
daemon runtime bookkeeping. Neither is a domain or managed-artifact write.

The guard remains a workflow detector, not a security sandbox. It retains
the existing trust in the installed helper, PATH executables and inherited
environment (including STORY_BIN and shell startup behavior). It does not
execute a provider command to authenticate every downstream executable.
Ordinary reader semantics are not broadened by this change.

Python's [shlex documentation](https://docs.python.org/3/library/shlex.html)
explicitly describes lexical parsing without shell semantic validation;
the conservative raw-syntax fence prevents that distinction from granting
execution permission. Installer comment backticks use byte escapes in the
identity constant so the existing conservative hook-budget source scanner
does not mistake inert data for a shell substitution.

## Validation

- RED: `plugin_install::protect_launcher::installed_launcher_reader_grammar`
  failed on the unchanged hook, denying the exact generated launcher with
  `context` before any helper execution.
- The grammar matrix exercises both hosts' shared Bash/command envelope,
  including Codex permission-mode metadata, all allowed verbs, selectors,
  and invocation prefixes. A PATH sentinel detects classifier execution.
- Negative cases cover mutators, unknown arguments, shell composition,
  substitution, redirects, changed/missing/symlink/FIFO identities and
  inaccurate edit diagnoses.
- Real integration uses production launcher/helper/CLI and an isolated
  daemon/store, with only the provider installation boundary simulated.
  Each read returns meaningful output and preserves domain/event snapshots
  and installed file contents/permissions.
- Directly impacted suites: plugin_install, protect_install_hook,
  hook_budgets, hook_bounds. Central verification owns the full suite.

The approved plan and subsequent decisions are recorded on SH-585. The
read-only challenger reviewed the helper side effects and exception grammar.
No installed plugin, override, release, or version was changed.
