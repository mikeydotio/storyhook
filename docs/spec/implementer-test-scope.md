# Implementer test scope: run only the tests your story writes

Design of record for **SH-864**: "An in-progress story should never run any
tests that it didn't write. Leave that for the verifier and release gates."

## The problem

Every surface that told an implementer which tests to run said "new and
(directly) impacted tests": both built-in dispatch charters, the verifier's
RED and CONFLICT returns, every project-recovery comment, the scaffolded
`AGENTS.md` and `.cursorrules`, and `story help verifier`. "Impacted" is a
judgment word, and agents stretched it — one lane ran 23 integration targets
(299 tests), another 13 — while several other lanes and the central gate
shared one machine. SH-863 measured that gate at load averages of 6–8 and
traced false REDs to contention. The central verifier already runs the full
suite on the exact merge tree (`full-auto-engine.md` D4/D5), so a lane-side
sweep added contention without adding certainty.

## The rule

`storyhook::service::verification::IMPLEMENTER_TEST_SCOPE`:

> Run only the tests this story adds or changes. Leave every other test to
> the central verifier and the release gates.

"Adds or changes" is read from the story's own diff: a new test, or an
existing test whose assertions the story edited. Builds, formatters and
linters are not tests and are unaffected.

### The one exception: a failed central gate

`FAILED_GATE_RERUN_SCOPE`, decided by a unanimous council (devops, QA,
skeptic) recorded on SH-864:

> When a central gate failed, you may also rerun each test case that its log
> names as failing, by its exact name only. Never rerun a whole target, file,
> script, or suite for it. Never edit or weaken a test that this story did
> not write to make it pass. If a named test does not fail when you rerun it,
> change no code for it and say so in a comment before you resubmit.

Why: one mistaken repair costs a whole serialized gate round (about 2 h, for
every story queued behind it), while rerunning one to three exact cases
costs seconds. Without the rerun, the only red-to-green loop left on a
pre-existing test is to change or duplicate it, which weakens the suite or
grows it. A local pass is not proof that the RED was false — the lane is not
the merge tree — so the agent changes nothing for it and the verifier stays
the judge. A target or script that the log names without a case-level name is
not rerunnable under the exception. It stays with the verifier.

Rejected: the strict reading (no reruns at all), for the reasons above; one
rerun of an indivisible target, which reopens the load hole; and "do not
resubmit unchanged", which leaves a returned story with no next actor.

## Where it is stated

| Surface | How |
|---|---|
| Attended and autonomous charters (`plugins/story/bin/story.sh`) | `TEST_SCOPE_CLAUSE` and `FAILED_GATE_RERUN_CLAUSE`, the constants' exact bytes |
| RED return, single story and batch culprit (`repair_return.rs`) | both constants |
| PROJECT REPAIR TESTS FAILED (`test_return.rs`) | both constants |
| CONFLICT return, project-recovery delivery, repair-story description, in-place repair comment, managed resume | the base rule |
| Scaffolded `AGENTS.md`, `.cursorrules`, legacy `.storyhook/CLAUDE.md`, `story help agent-guide` | the base rule, verbatim |
| `story help verifier`, continuation refusal | paraphrase, no retired wording |

Both constants are charter-inert (SH-226): no shell metacharacter, quote,
parenthesis or newline, so the charter carries them unchanged.

## The guard

`tests/implementer_test_scope.rs`:

- the constants are charter-inert and keep their load-bearing phrases;
- every imperative surface states the rule verbatim (whitespace-normalised);
- both charters define the clauses once, use each once, and scope the
  exception to the repair sentence;
- a reviewed inventory counts each runtime site's `{CONSTANT}` interpolations;
- a derived scan over tracked `src/`, `plugins/story/` (minus its test
  fixtures) and `AGENTS.md` fails on the retired word "impacted", with a
  negative control and a reviewed exemption list (empty).

No hook enforces the rule: a command line cannot reliably show which test
cases it runs, so a hook would refuse exact-name runs and admit broad ones
spelled differently. If lanes still over-run tests, measure it at the gate.

## As-built

- Shipped in the SH-864 branch. The plugin's charter tests pin the rendered
  text: `test-dispatch-auto.sh` and `test-charter-inert.sh`.
