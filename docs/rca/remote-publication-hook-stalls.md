# Remote publication stalled inside the global pre-tool gate — SH-681

- **Investigated:** 2026-09-11, StoryHook v2.4.2, checkout `91b66945d`.
- **Impact:** SH-665 publication calls appeared not to return; even a diagnostic
  comment started a hidden suite. SH-682 records the same mechanism on SH-668.
- **Disposition:** the tested Agentics repair is preserved in closed archival
  [PR #187](https://github.com/mikeydotio/agentics/pull/187). Concurrent,
  user-approved retirement under SH-682 / AGE-102 superseded installation.
  [Retirement PR #186](https://github.com/mikeydotio/agentics/pull/186) is the
  active source proposal; its verification remains separately owned.

## Evidence and causal chain

Codex's PreToolUse registration ran `~/.codex/hooks/pre-push-tests.sh` before
Bash, with a declared 900-second timeout. The gate searched the entire command
string for a push, discovered the Makefile test target, and ran it with an
840-second internal deadline. Output went to a private temporary log until the
gate returned. The requested shell command had not started while the tool
appeared stalled. Cancelling the orchestration cell did not immediately stop
the hook's suite.

The retained SH-665 transcript is
`~/.codex/sessions/2026/09/11/rollout-2026-09-11T07-13-32-01a090d1-0368-7242-a4b9-e5d466046225.jsonl`.
Its calls correlate with these logs in the operator's `TMPDIR`. Times are UTC
on 2026-09-11; birth and modification timestamps bound each log's lifetime.

| Call | Submitted | Log suffix | Created → last write | Bytes |
|---|---|---|---|---:|
| HTTPS-configured push | 14:26:17.754 | `eGPCwUWySJ` | 14:26:19.537 → 14:40:19.560 | 739,838 |
| Diagnostic comment containing quoted push text | 14:33:47.600 | `3M734hNlJ6` | 14:33:47.742 → 14:47:47.744 | 67,445 |
| Plain push | 14:36:35.650 | `Jw5y0qM8DR` | 14:36:35.816 → 14:50:35.818 | 726,909 |

Complete filenames are `prepush-tests.XXXXXX.<suffix>`. Each lifetime is 840
seconds within timestamp precision. Logs contain StoryHook test and gate-lock
output and end in Make termination, not a demonstrated test assertion failure.
Their SHA-256 digests preserve the evidence identity:

| Suffix | SHA-256 |
|---|---|
| `eGPCwUWySJ` | `04de9ed6a383e17cb26f303eaa97b4cee73d527d59fff988ef53324eb80ca789` |
| `3M734hNlJ6` | `788da4b5d4c3f5cb8f445e8c9f5b71719c9c4244a9fa06725766654f2c190d21` |
| `Jw5y0qM8DR` | `162be6b6c9a5ef95d182d3139ea7872a61c9a0cab6a107ab11a478ae7b10cddb` |

The comment invocation is the discriminating observation: it started the suite
without attempting network publication. Isolated regressions then reproduced
duplicate test discovery in a repository with its own Git gate and enforcement
in an ordinary repository. Together these locate this failure before shell
execution; they do not establish a GitHub or Git transport failure.

## Ownership correction

The gate was not an ownerless local file. Its canonical source was Agentics
`hooks/pre-push-tests.sh`; its installer distributed to Claude by default.
At investigation time:

| Artifact | State | SHA-256 |
|---|---|---|
| Agentics canonical source and installed Codex copy | Identical; lacked delegation | `892b3592835724bd5050f742eb8636427ee84e6fd5bb7768b6a49b2a46e47222` |
| Installed Claude copy | Local delegation patch absent from canonical source | `e6a99a00befb6e5e4cbc6592223582ee608a8d150cb763541462031d6fad6eba` |

The earlier D-C completion statement in
[the verification workflow](../spec/verification-workflow.md) described the
Claude-only live patch. It neither established Codex parity nor fenced the
canonical Agentics source. A fix to one installed copy could coexist with the
original defect in another provider and in future installations.

## Completed repair and regression coverage

The approved plan was posted verbatim on SH-681 before implementation. The
repair was developed in an isolated Agentics clone, preserving unrelated
changes in the existing checkout. Two commits remain on remote branch
`fix/SH-681-publication-hooks`, based on `4dccfa1b2946bbf601eb8d76332d0dd968165b66`:

| Commit | Change |
|---|---|
| `9b092f2478715082680c9fea509d49d55843611a` | Prove the effective repository gate before test discovery; add real Git regression fixtures |
| `f715b7063afeb8f7e9814cd676d06b2f32126fd3` | Install/check both provider bundles with backups; use Codex's own timeout; guard inherited target ambiguity |

The bounded probe tokenizes literal commands without executing them. Git
resolves the target checkout and effective hook path; delegation requires an
executable regular file tracked with executable mode inside that checkout.
The real Git operation still invokes that hook and honors its refusal.
Unknown syntax, ambiguous targets, or missing ownership retain global
enforcement. This follows the [Git hook contract](https://git-scm.com/docs/githooks)
and treats [shlex](https://docs.python.org/3/library/shlex.html) as a lexer,
not a general shell interpreter.

The first council voted 3–0 to keep the general command-position parser
redesign with existing AGE-63. SH-681 covered conservative delegation,
Claude/Codex distribution, drift, backups, and provider-specific deadlines.
The complete decision was commented on SH-681 before implementation resumed.

| Direct validation in Agentics | Passing checks |
|---|---:|
| `python3 -B tests/prepush_delegation.py` | 12 |
| `python3 -B tests/prepush_install.py` | 11 |
| `bash tests/prepush-gate.sh` | 68 |
| `bash tests/bounded-capture-guard.sh` | 25 |
| `bash tests/gate-deadline-guard.sh` | 19 |
| `bash tests/gate-integrity.sh` | 5 |
| **Total** | **140** |

New regressions failed before their fixes and passed afterward. Fixtures prove
allowed and rejected updates against a real bare Git remote; no test discovery
on delegation; quoted paths, subdirectories, linked worktrees, configuration
overrides and ambiguity; and installation, execution, backup, permission,
registration, and deadline behavior for both providers. ShellCheck at warning
level and diff whitespace checks passed. Agentics has no impacted-test
selector. Its existing direct target includes both new suites.

The deadline guard initially failed five process-ancestry checks because the
sandbox could not inspect `ps`; an authorized rerun passed all 19. An early
installer reproduction fixture did not override the old installer's home;
the sandbox refused its attempted backup write. The live Claude digest was
confirmed unchanged, and the fixture was corrected to isolate the child HOME
before subsequent runs. No real installation was performed by these tests.
These are repair results, not validation of the retirement implementation.

## Concurrent retirement and final disposition

At 16:39:55Z, SH-682 recorded completion of a separate user-approved live
retirement, from Agentics commit `abd398f0952aec3f30ffcf7631949935b2e341d4`.
Before installing the SH-681 repair, a fresh read found both provider scripts
absent. Read-only JSON inspection also confirmed neither provider retained a
PreToolUse registration for this gate. SH-682 preserved the original scripts
and settings under
`~/.local/state/agentics/retired-pre-push/20260911T163913Z-4bprdhth`.

A second native Codex council (DevOps, architecture, security) voted 3–0 to:

1. Preserve the completed repair and tests in an explicitly superseded, closed
   archival Agentics PR, retaining the remote branch.
2. Leave both retired hooks absent. The approved live installation step is
   superseded; it is not reported as executed or successful.
3. Submit this compatible StoryHook evidence change for central verification.
   SH-682 / AGE-102 retain the retirement PR and their installed-daemon
   portability blocker; this investigation does not resolve that blocker.

Both full council outcomes were commented on SH-681 before work resumed.
Run `story show SH-681`: the 2026-09-11 16:31:31Z comment records the scope
decision, and the 2026-09-11 16:53:49Z comment records the retirement decision.
These durable records survive worktree reclamation.

Agentics PR #187 is an archive, not a merge recommendation: merging or
installing it would conflict with retirement PR #186. Publication of the
archival branch over ordinary HTTPS Git completed after retirement, without
starting the global suite. No hook was disabled or reinstated by SH-681.

## Separate installed-prompt drift

The installed Codex StoryHook v2.4.2 helper still had `PROMPT_TPL` and
`AUTO_PROMPT_TAIL` telling agents to publish. The tracked
`plugins/story/bin/story.sh` already assigns submission to the verifier through
SH-647. This is installed-artifact drift, not a missing source fix. It explains
why sessions could continue receiving the old publication charter after the
source workflow changed. The cache was not edited, and no release or version
step was attempted. This session followed its explicitly approved two-PR
publication exception; the general verifier-owned workflow is unchanged.

The durable lesson is to inspect the active provider registration, installed
bytes, canonical source, and process logs separately. A successful local patch
or source merge alone cannot prove what another provider's active hook runs.
