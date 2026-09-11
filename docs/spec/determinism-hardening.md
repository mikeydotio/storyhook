# Determinism hardening — SH-687

StoryHook v2.4.2 already owns most operational workflow in code. This audit
traces model-facing behavior from entry to recovery, including instructions
whose mechanical steps still depend on a model following prose. The objective
is to remove unnecessary interpretation without pretending deterministic
syntax can replace semantic judgment.

## Changes and compatibility

The council chose a strict whole-message JSON implementation-plan request:
`{"type":"storyhook.implementation-plan","version":1,"story_id":"SH-687","plan":"complete plan text"}`.
Only built-in autonomous Codex charters advertise it, in Default mode. Plan mode
continues to advertise the native `proposed_plan` envelope. A structured request
received in Plan mode redirects to that native review without permitting writes
or implementation. Attended sessions and custom prompts retain their behavior.

The parser distinguishes absent, invalid, and valid protocol data. Valid requests
bypass Luna; invalid, quoted, embedded, duplicate-key, mismatched, and unknown
version requests never fall through to model classification. Ordinary prose
retains Luna. The hook retains its root/session/turn/cwd/mode checks, eligibility
recheck, and locked, fsynced at-most-once receipt. Receipt hashes identify both
the whole request and the exact decoded plan. Fixed feedback requires that plan
verbatim as the first implementation comment. The plan text is never executed
or interpolated into a command by the hook.

This declares readiness; it does not prove semantic completeness. Approval
remains limited to the implementation plan and cannot grant network, credential,
deletion, deployment, scope, or unresolved-choice permissions.

`story session-eligibility <id>` supplies one typed read of the story, configured
active role, and dependency readiness, in one existing store transaction. It
does not claim, approve, or modify the story. Missing or unreadable evidence is
an error, not eligible. Provider identity remains the hook's responsibility.

Triage must not turn failed reads into empty successful findings. Actual cycle
members must be distinguished from downstream dependents; both can be blocked,
but only a member needs an edge in that cycle repaired.

## Audit inventory

The completed inventory and validation evidence are recorded below as each
production route is checked. Model judgment retained deliberately is not an
unimplemented deterministic replacement.

## Decision record

The full council verdict is persisted in SH-687 comments. API designer, software
architect, and security researcher independently proposed strict protocols;
after one deliberation all ranked the explicit type/version schema first.

The read query uses existing domain predicates and a transaction instead of
reimplementing role/readiness rules in Python. No new persisted workflow state
or storage migration is needed.

## References

- [Workflow versus agent orchestration](https://www.anthropic.com/engineering/building-effective-agents)
- [SQLite snapshot isolation](https://www.sqlite.org/isolation.html)
- [JSON object contracts](https://json-schema.org/understanding-json-schema/reference/object)
- [Existing prose continuation contract](codex-auto-plan-continuation.md)
