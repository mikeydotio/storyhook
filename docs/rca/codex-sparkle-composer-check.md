# Codex dispatch refused a submitted primer as unconfirmed under the Astra sparkle

- **Date**: 2026-09-12
- **Severity/Impact**: Every autonomous Codex dispatch, from the agent plugin and from the
  web UI alike, refused with `bootstrap-submit-unconfirmed` from the installation of Codex
  0.154.0 on 2026-09-10 until the fix. Three attempts were recorded on 2026-09-12 (17:54Z
  and 19:56Z on SH-679, 20:07Z on SH-694). Each attempt killed a correctly initialized Codex
  pane and rolled its claim back. No data was lost and no story was worked by a wrong
  process; no Codex story could be dispatched unattended.
- **Status**: Fixed in `78ff05fd2`, `513c75709` and `bfb7eae32`; test hardening in `d2488b73a`

## Summary

Autonomous Codex dispatch confirms that its task-free initialization turn was submitted by
reading the composer row back from the pane and matching it against the provider's empty
placeholder. Codex 0.154.0 decorates the idle composer of an Astra model with an animated
Braille "sparkle", so the placeholder never matched, the confirmation window expired, and a
primer that had been submitted, intercepted by the SessionStart hook and stopped exactly as
designed was reported as unconfirmed. The refusal then blamed hook identity, which sent the
first diagnosis after the hook. The fix disables animations for the managed process, strips
decoration in the reader so a launch override or a future decoration cannot fail the same
way, and makes every initialization refusal name the phase that actually failed.

## Timeline

| Date | Event | Anchor |
|---|---|---|
| 2026-09-10 | SH-675 landed first-turn initialization: a primer whose submission is confirmed by `input_state` reading the composer row | `fb5b97b2d` |
| 2026-09-10 15:55 PDT | Codex 0.154.0 installed (Homebrew cask); release notes: "Add Astra sparkle effects to the TUI composer" | Homebrew |
| 2026-09-12 17:54Z | SH-679 dispatched from the plugin refused `bootstrap-submit-unconfirmed` | SH-679 |
| 2026-09-12 19:56Z | SH-679 dispatched from the web UI refused the same way; SH-694 filed as a web-UI defect | SH-694 |
| 2026-09-12 20:07Z | Recorded reproduction on window SH-694, pane %220, nobody typing: 100 frames at 0.5 s show Braille dots before and after the placeholder; one clean frame before the effect started; Codex rollouts show the hook ran and stopped the turn in about 0.5 s | SH-694 comment |
| 2026-09-12 | Origin, defence-in-depth and diagnosis fixes landed with their regression tests | `78ff05fd2`, `513c75709`, `bfb7eae32` |

## Root cause & trigger

The verified defect→infection→failure chain was:

1. **Defect:** `plugins/story/lib/session.sh::input_state` matched the raw composer row
   against `EMPTY_INPUT_PATTERN` (`^[[:space:]]*Ask Codex to do anything[[:space:]]*$`,
   `bin/story.sh`) and answered `text` for anything else. Nothing tolerated decoration, and
   the managed launch pinned no rendering switch.
2. **Infection:** under Codex 0.154.0 with an Astra model the row rendered as
   `›⠁Ask Codex to do anything   ⠈       ⠂  ⠁     ⠄    ⠂         ⠐`, redrawn every 150 ms
   for the whole idle period. `send_prompt_confirmed` Phase B polled `input_state` for
   `empty` eight times at 0.3 s and never saw it.
3. **Failure:** `codex_bootstrap_ready` reported `bootstrap-submit-unconfirmed`; dispatch
   killed the pane, rolled the claim back, and `dispatch_ready_note` rendered "the task-free
   turn did not establish exact hook identity", a sentence with no evidence behind it.

The trigger is external: Codex's `codex-rs/tui/src/bottom_pane/chat_composer/sparkle.rs`
enables the effect only when `tui.animations` and `tui.whimsy` are both on (their defaults)
and the model name matches `astra`. The user's `~/.codex/config.toml` selects `gpt-6-astra`
and sets neither switch. Non-Astra models were unaffected, which made the failure look
intermittent.

**ODC classification:** **Checking**, qualifier **Incorrect**: a validation predicate that
rejects a valid state, triggered by a rendering change in an external component. The
confirmation contract itself (a submitted primer clears the box) was right; the observation
of it was too literal.

## Contributing factors

- The confirmation reads rendered characters, the one channel the provider restyles at will,
  and the only one available: Codex exposes no event at composer clear.
- The fake tmux rendered the placeholder byte-stable. Nothing in the suite modeled a
  decorated or animated composer, so the check had never been exercised against one.
- One `bootstrap-*` sentence covered four distinct reasons and asserted a hook-identity
  failure for all of them.
- The recorded field pane showed exactly one clean placeholder frame before the animation
  began: even a lucky poll could only have passed by racing the effect.
- The story was filed as a web-UI defect because a plugin dispatch had succeeded earlier on
  a non-Astra model; the model dependence hid the common cause.

## The fix

The verdict was **SURGICAL**: the defect is confined to how the plugin observes the
composer and how it reports the observation; no protocol or stored data changed.

- **`78ff05fd2`** (origin) adds `-c tui.animations=false` to `compose_codex_launch_tpl`,
  beside the existing `-c check_for_update_on_startup=false`, for attended, `--auto` and
  `--full-auto` launches. It is Codex's documented switch for the welcome screen, shimmer,
  spinner and sparkle, so every row the helper reads is static; the user's own config is
  untouched.
- **`513c75709`** (defence in depth) adds `strip_composer_decoration`, one `LC_ALL=C sed`
  over the UTF-8 byte sequence `E2 [A0-A3] [80-BF]` (exactly U+2800..U+28FF), called from
  `input_box_text` before any pattern is applied. Byte ranges behave identically on BSD and
  GNU sed and do not depend on the caller's locale. Only the observation is stripped; the
  empty pattern is deliberately not widened, because a real draft can contain the
  placeholder words and reading it as empty would report a never-submitted prompt as
  submitted.
- **`bfb7eae32`** (diagnosis) gives every `bootstrap-*` reason its own refusal sentence:
  what was observed, what was and was not typed, and where to look next. None asserts a
  hook-identity failure; the `hook-identity-*` arms are unchanged.

A live probe of the flag against the installed Codex could not reach the composer without a
trust-bypass flag or keypresses (hook-trust dialog from the checkout, directory-trust dialog
elsewhere), so the flag's effect rests on Codex's documented key and its `sparkle.rs` gate.
The second layer does not depend on the flag and is proven by the recorded row.

## Preventative action — killing the class

- **`plugins/story/tests/test-codex-sparkle-composer.sh`** pins the reader against the
  exact recorded row under both C and UTF-8 locales, keeps drafts (decorated or not) as
  text, checks the Braille block's boundaries, and reproduces the field failure as a whole
  `--auto` dispatch under the fake's new `FAKE_TMUX_CODEX_SPARKLE=1`, which rotates the
  decoration per capture so no two frames are byte-identical. A red run prints
  `wait_ready_reason` and `bootstrap_phase` in its own failure line (`d2488b73a`).
- **Mutation check:** with the strip call removed, that file refused with
  `wait_ready_reason=bootstrap-submit-unconfirmed`, `bootstrap_phase=submitted`, one
  submitted primer and no charter, the field failure verbatim.
- **`test-dispatch-launch-template.sh`** asserts `-c tui.animations=false` on the attended
  and `--auto` compositions, and every launch-string pin in the suite carries it.
- **`test-dispatch-codex-bootstrap.sh`** drives the submit-unconfirmed and plan-unconfirmed
  refusals and asserts that all three reachable `bootstrap-*` refusals name their phase and
  never hook identity.

## Lessons

- A confirmation predicate that reads rendered text must tolerate decoration, and its
  fixture must model decoration. A byte-stable fake proves nothing about an animated TUI.
- A managed child process gets its cosmetic switches pinned, the way its update chooser
  already was; the operator's preferences are not the helper's rendering contract.
- A refusal may name a subsystem only with evidence for that subsystem. Otherwise it says
  what was observed and what was not typed, and lets the operator look.
- "Works from the plugin, fails from the web UI" was model dependence in disguise. When a
  provider behaviour is conditional on configuration, record the configuration with the
  failure before comparing paths.
