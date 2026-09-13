# SH-712: Documented installed readers denied by the guard

## Failure and cause

StoryHook v2.4.2's installed-artifact hook recognized `context` and
`context --full`, but omitted the documented story selector. The same fixed
reader list omitted `handoff` and `triage`. This is missing argument
classification, not evidence that those helpers edit installed artifacts.

| Hypothesis | Discriminating evidence | Result |
|---|---|---|
| Valid reader arguments are missing from the guard | Both installer-owned entry points pass legacy context controls, then deny the selector before execution | Confirmed |
| Installed launcher/helper identity is invalid | The same fixture identities admit legacy readers; actual helper obviation tests succeed | Refuted |
| Handoff/triage require story or installed-file mutations | Real helper flows return useful output; integration assertions compare store events and installed bytes/modes | Refuted for admitted forms |

Commit `61424b52c` introduced the context selector in the helper without
updating the guard's argument contract. Last known good for the installed
selector form remains unknown. Classification: checking, missing; trigger:
documented installed-helper invocation. A narrow classifier repair suffices.

The completed obviation review compared SH-699, SH-700, SH-702, SH-703,
SH-705, SH-707, and SH-708, including full discussion histories and linked
commit file sets. None supplies this repair. SH-708 separately owns
reset/create/unclaim and bounded heredoc classification; its shared-file
changes must be preserved during later integration.

## Repaired contract

| Reader | Accepted shape |
|---|---|
| Context | No options; one `--full`; one `--story ID`; or both options in either order |
| Handoff | No options, or one `--since DURATION` |
| Triage | No arguments |

The selector stays adjacent to its value. IDs use the existing view/capture
operand grammar, `[A-Za-z0-9][A-Za-z0-9_-]*`, including numeric shorthand.
Durations use `[0-9]+[mhdw]`, including zero. Admission classifies literal
argument shape; story lookup and duration range remain CLI concerns.
Duplicate flags, missing values, extra operands, and equals forms remain
outside this contract. The guard intentionally accepts a conservative subset
of helper-tolerated spellings, including at most one `--full`.

Both installed entry points retain their existing identity checks: exact
installer bytes for the Codex launcher, same-plugin identity for the direct
helper, unredirected ancestry, and bounded regular-file reads. Classification
executes neither entry point nor the provider. Existing managed operands,
substitutions, redirections, wrappers, and shell compositions remain refused.

The reported compound `ensure-cli` refusal is consistent with this
standalone-only contract. Standalone `ensure-cli` remains admitted. Python's
[shlex documentation](https://docs.python.org/3/library/shlex.html) describes
lexical splitting without shell semantic validation; this repair does not
introduce a general shell parser or a blanket helper exemption.

The sibling audit preserves the established view/list/capabilities/capture/
doctor forms. `sync` is not a reader: it records commit-sync state. Triage's
temporary diagnostic files under `/tmp` are not installed-artifact writes.

## Regression evidence

- Context RED: both installed-entry grammar tests failed at
  `context --story TST-1` after their legacy controls passed.
- Sibling RED: the dedicated regression collected all six refusals for
  handoff, handoff with a duration, and triage through both entry points.
- Shared matrices cover providers, interpreter spellings, project selectors,
  malformed arguments, shell composition, managed operands, foreign/stale
  helpers, altered launchers, missing files, symlinks, and FIFOs.
- Real helper/CLI/store flows verify selected obviation evidence, both full
  orderings, nonexistent-target errors, handoff summaries, and triage findings.
  Every read preserves store event positions and installed file snapshots.
- Handoff assertions use stable document/summary content, not an assumption
  that events cannot age out of a one-minute window under host load.

Direct validation uses the installed-entry cases in `plugin_install`,
`protect_install_hook`, `hook_budgets`, and the context, obviation-review,
handoff, triage, and triage-read-failure shell tests, plus formatting and
targeted Clippy with warnings denied. The actual-tree selector returned
`ALL` because its certified baseline had no coverage map; the central
verifier owns that full-suite run. Exact outcomes are recorded on SH-712.

No installed cache, override marker, trust, version, or release was changed.
The direct `story load-context --story SH-712` workaround remains until a containing
release is installed and its exact managed-launcher invocation is separately
validated. Source-level tests do not certify the current installed copy.
