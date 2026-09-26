# SH-780: Resume and remediation notify could press Enter on a dialog

- **Date**: found 2026-09-25 by the SH-772 council (seat 2, security review);
  fixed 2026-09-26. Not observed in the field.
- **Severity/Impact**: `story.sh notify` could approve a tool permission or a plan
  for the person, with no record that anyone approved it. Two paths: the
  verifier's remediation (`notify <id> <message>`, also used by project
  recovery) and the resume after an acknowledged interrupt
  (`notify <id> <prompt> --expected-target <target>`).
- **Status**: Fixed on `worktree-SH-780`. The dispatch sibling, SH-799, is
  fixed too (see the last section).

## Defect

Both paths ran `paste_prompt` and then `tmux send-keys "$SUBMIT_KEY"` with no
look at the pane. Claude Code and Codex draw a dialog's cursor with the
composer's own glyph (`❯ 1. Yes`, `› 1. Yes, implement this plan`), and Enter
there selects the highlighted answer. SH-772 had guarded only its new
`--registered-session` form, and even that guard accepted "any text" as proof
that the paste arrived, which a dialog row also is.

A native test reproduced it before the fix: with a dialog on the screen,
`notify` answered `ok` after it had typed the message and pressed Enter.

## What the evidence added

Read-only captures of live panes (2026-09-26, Claude Code 2.1.283, Codex
0.157.0) showed that an idle guard built on the old reader would have failed
the other way:

| Evidence | Consequence without a fix |
|---|---|
| Claude pads `❯` with U+00A0 NBSP | Under `LC_ALL=C` (a launchd daemon has no locale) every idle Claude composer read as `text` |
| Two of four idle Claude panes showed a faint (SGR 2) predicted next prompt | An idle guard would refuse about half of all remediations, and the verifier would park those stories |
| Codex's placeholder is faint too, on a `48;2;r;g;b` background | A parser that saw the digit 2 would read colours as faint |
| `capture-pane -e` carries SGR state across lines | Attributes must be read from the top of the capture |

## Fix

One guarded block serves every form that types a prompt: a strict idle check,
one paste, a receipt of this prompt (`composer_holds`: its first line, or the
provider's collapsed-paste placeholder), identity revalidation, and a new
`composer_holds` read before each submit key. The reader (`lib/composer.awk`)
leaves faint text out and resolves every doubt to `text`. Native provider
fixtures now draw a composer, so the tests see what a delivery did.

## Why it escaped

- The three delivery forms grew one at a time (SH-521, SH-690, SH-772), and
  only the newest got the guard.
- The receipt rule (SH-226) was written for a fresh dispatch, where the pane is
  known to be at an idle composer. It proves that some text arrived, not which
  text.
- The native fixtures drew nothing, so no test could observe the composer.

## Class detector

`tests/notify_reasons.rs`: `KEY_SENDERS` lists every place the plugin presses
a key in a pane, with the reason it may, and a scan demands exactly that set. A
second test pins `cmd_notify`'s single guarded delivery. A new key sender
fails the build until someone writes down why it is safe.

## Residual risk

A dialog that opens in the milliseconds between the last `composer_holds` read
and the key. A placeholder pattern that stops matching after a provider update
fails closed (`delivery-failed`, the story parks); `STORY_PASTE_PLACEHOLDER_PATTERN`
overrides it.

## SH-799: the dispatch sibling

`send_prompt_confirmed` (`lib/session.sh`) hands every dispatch its charter and
Codex its initialization turn. It kept the SH-226 receipt ("any text"), and it
re-sent the submit key up to `SEND_RETRIES` times without a look. Claude's
readiness is its SessionStart sentinel, so nothing read the screen before the
paste. A dialog that opened after readiness passed the receipt, and the key
approved it. Before the fix, the new tests showed exactly that: with a dialog
at the window, on the paste or on the first key, a key answered the dialog.

Evidence taken before the change, because a wrong pattern here fails every
dispatch: `~/.claude/history.jsonl` shows every storyhook dispatch as
`[Pasted text #1]` in the composer. The Claude Code 2.1.283 binary builds
exactly that string, and above 10 000 characters it keeps the first 500 (the
signature still matches). The Codex 0.157.0 binary has `[Pasted Content N
chars]`.

The function now takes the notify shape, plus what its design review found:

| Step | Rule |
|---|---|
| before typing | `poll_composer_idle`: a drawn, idle composer. Not drawn yet: wait on the READY budget. A row with text (a dialog, a draft): `CONFIRM_ATTEMPTS` reads, then stop |
| receipt | `poll_composer_holds`; re-paste only while the composer still reads idle, never over anything else |
| each key | `composer_holds` first; a key already sent whose clear showed late is the submission (`composer_cleared`), and no second key follows |
| confirmation | `composer_cleared`: not this prompt **and** no input. `input_state` alone leaves faint text out, so a faint placeholder after a swallowed key read as submitted |

`undelivered` now means exactly "no submit key was sent", and dispatch rolls it
back (the pane is stopped first). The refusal keeps `handoff-undelivered`, so
project recovery still reads it as a proven failure, and it adds
`delivery_detail`: `composer-not-idle`, `prompt-not-seen`,
`prompt-not-recognised`, `composer-changed` or `submit-unconfirmed`. A Codex
initialization with no key sent is `bootstrap-undelivered`.

The review's late-clear and faint findings also held for `cmd_notify`: a
remediation whose clear showed late was refused while the agent worked, and
the verifier parked the story. Notify now confirms the same way. The generic
`poll_input`, whose "text" form was the unsafe receipt, is deleted.

**Class detector**: `tests/notify_reasons.rs` checks one guarded shape for both
prompt senders. There is an idle check before the single paste, and a
`poll_composer_holds` receipt before the key loop. The one `$SUBMIT_KEY` send
sits inside its loop, at most two lines below a `composer_holds` call that stops
the loop. A positive control proves that the receipt does not count as the gate.

**Residual, beyond the notify ones**:
- A fresh Claude session shows `Try "..."` until the first submission. The
  binary draws it all faint, or with the first letter inverted when Claude
  paints its own cursor cell. The live captures show no painted cell, so the
  composer reads idle. If a later Claude draws the inverted form, every dispatch
  refuses `composer-not-idle`: visible, and closed. `test-composer-reader.sh`
  pins both renderings.
- A prompt echoed on a glyph row below a hidden composer could pass
  `composer_holds`.
