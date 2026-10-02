# SH-821: Registration loss without a completed uninstall receipt

Version: v3.0.3. Investigation started 2026-10-02.

## Historical evidence

| UTC | Observation | Source |
|---|---|---|
| 2026-09-23 05:22:18 | Last install recorded by the receipt inspected at filing | SH-821 description |
| 2026-09-25 23:17 | Plugin registry mtime; story entry absent when inspected later | SH-821 description |
| 2026-09-26 19:37 | Marketplace registry and cache-directory mtimes; storyhook absent when inspected later | SH-821 description |
| 2026-09-26 19:38:03–19:38:31 | Operator session reads the receipt and both provider registries | Retained Claude session `307faf53-3a25-4221-a76e-ce6c191c6db6` |
| 2026-09-29 18:46:37 | A later successful install receipt identifies the installed StoryHook executable | Current receipt, read 2026-10-02 |

The retained daemon activity files for September 25 and 26 have no plugin RPC
messages in the inspected windows (September 25 22:00–24:00 UTC and September
26 18:00–19:40 UTC). Later warnings recommend installation; these are not
evidence that an installation ran. Both registrations are present on October 2.

No retained trace inspected here identifies the writer that removed the
September entries. File mtimes do not identify a process. Missing daemon
entries do not exclude in-process tests or external writers. The earlier
SH-760 incident establishes a different actor, not this incident's actor.

## Reachable failure mechanisms

Both installers remove the plugin and then the marketplace. Before this change,
the rollback closure starts only after both removals. If the second call fails,
the first call's successful removal is not undone. The comment claiming that
nothing has been destroyed at that point is incorrect.

An interruption after removal and before receipt publication also leaves the
previous install receipt unchanged. Explicit uninstall has the same evidence
window before its tombstone. Claude uninstall additionally discarded a failed
marketplace removal and treated a failed plugin removal as optional, allowing
a completed tombstone to conceal an incomplete operation.

These are code findings and controlled reproductions. They do not
establish what ran on September 25 or 26.

## Operation evidence contract

The existing install receipt and uninstall tombstone retain their format.
Each provider has a sibling `<target>-operations/current.json`. Before replacing
it, an operation archives the previous bytes under `history/<unique-id>.json`.
An unreadable previous file stops mutation; malformed but readable bytes can
be archived without destroying them. A later successful operation supersedes
the current diagnostic without deleting history.

An operation records its version, ID, provider, verb, start/end time, HOME,
StoryHook PID and executable/build identity, override status, previous source,
intended source, steps and outcome. A step records its action, start, completion
and result. Start evidence is synchronized before an effect. JSON is published
by same-directory atomic replacement; synchronization errors propagate.

A synchronous thread-local scope connects the operation to the shared provider
runner, including rollback calls. It is cleared on ordinary return or unwind.
No actor PID is invented for a provider result that does not carry one; the
existing process activity journal remains the source for child process starts.

The nonblocking lock is at
`$HOME/.local/state/storyhook/provider-locks/<target>.lock`, independent of data
directory overrides. Provider children inherit its descriptor so an orphaned
provider retains exclusion. Another StoryHook mutation refuses while it is
held. This does not lock out Claude, Codex, or another external writer.

Doctor is read-only. It reports incomplete and failed recorded operations,
even when the marketplace still exists. It does not equate an incomplete record
with a killed process or prove that the last operation caused a later loss.
Without causal evidence, lost registration is reported with `cause unknown`.

## Regression evidence

| Check | Before | After |
|---|---|---|
| Second removal fails after plugin removal, both providers | Failed: no re-registration | Pass |
| Seven initial operation tests | All seven failed | All seven passed |
| Expanded operation matrix | Includes the initial failures | Nine passed |
| Provider child retains lock after request scope ends | New lifetime proof | Pass |
| Refused test build creates no operation record | Expanded guard regression | Pass |
| Spawn inventory includes the owned lock-test child | Updated inventory | Pass |

The interruption test uses the production installer and daemon, with a fake
provider as the endpoint. The fake removes only its fixture marketplace and
sends SIGKILL to its parent, the fixture's owned daemon. Both explicit install
and uninstall leave the old receipt unchanged. The new record identifies the
incomplete marketplace removal; doctor does not claim that a signal caused it.

The expanded matrix also checks reinstall verb identity, successful terminal
records, archived failed attempts after recovery, unknown-cause reporting,
malformed/foreign/inconsistent evidence, concurrent lock refusal, evidence
publication failures, and preservation of both an install and rollback error.

Only new and changed cases ran: `plugin_install::operation_tests::` (nine),
`plugin_install::failed_marketplace_removal_restores_the_plugin_already_removed`,
`plugin::operation::tests::inherited_provider_lock_survives_scope_exit`,
`plugin_guard::a_test_binary_is_refused_every_verb_before_any_provider_call`, and
`spawn_inventory::every_way_storyhook_starts_a_process_is_classified`.
Targeted Clippy with `-D warnings`, formatting, and whitespace checks pass.
The full suite remains the verifier's responsibility.

## Acceptance boundary

The historic actor remains unproved. Do not claim that diagnostic improvements
alone satisfy SH-821. Preserve the code, regression evidence, and this limitation
until a retained trace or supported reproduction identifies the responsible
process. Do not remove or rewrite the operator's live registration to produce
such evidence.
