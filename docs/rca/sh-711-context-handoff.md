# SH-711: Context transfer stranded as a task block

## Evidence and diagnosis

In v2.4.2, SH-702/703 committed their primary fixes, adopted bounded follow-up
work, then stopped under the unknown/half-context rule. The charter had no durable
continuation operation. A generic hard-stop instruction converted an execution
capacity problem into a story block; SH-699 inherited a stale prerequisite wait.
No last-known-good revision is established.

The initial regression called production Codex Stop parsing, transcript identity,
eligibility and classification with external CLI response data. A structured
implementation plan was accepted; the same eligible root's context envelope was
ignored in both Plan and Default modes. This proves the missing administrative
route, not a deterministic reproduction of historical model reasoning.

Competing explanations included stale eligibility alone and a missing lifecycle.
The ordinary-plan/eligible-root controls falsified stale eligibility as the sole
cause. Independent RCA challenge accepted the lifecycle diagnosis, with medium
confidence in the historical causal chain. Eligibility correctly refuses actual
blocks and remains enforced.

## Native runtime falsification

An actual Codex 0.154.0 app-server probe used a test-owned home, exact native hook
trust hashes, read-only provider sandbox, loopback model responses and no live
credentials. Manual compaction controls preserved mode, but a queue race showed
that starting compaction after an idle observation could interrupt a correction
that had just begun. This rejected forced live compaction as the implementation.

Production Stop continuation feedback instead preserved and later executed queued
corrections in both modes, with exactly one durable request. Default executed the
fixture acknowledgement endpoint; Plan continued read-only without acknowledging.
No implementation approval or forced compaction occurred. The pending correction
was absent from both Stop payload and rollout until its later queued turn began.
Accordingly the solution never equates acknowledgement with a drained native queue.

## Repair and limits

Typed context requests are durable and distinct from task blocks. Native Stop
feedback owns live continuation; exact retained dead-pane recovery uses no-k
respawn and revision-checked daemon ownership. Resource uncertainty becomes a
visible diagnostic without destroying work. Review binds story sequence and HEAD
before verification. Three unchanged handoffs bound recurrence.

The adopted SH-710 administrative Plan-mode stop uses the same trusted supervisor
boundary to record pending human obviation review. It grants no implementation
approval and does not decide the human question.

Regressions cover the missing route, mode/identity/recursion controls, transactional
state and idempotency, receiving acknowledgements, later durable corrections,
daemon restart, real hold preservation, process incarnation changes, missing and
duplicate panes, dirty content, and non-destructive recovery. The runtime and
durable-store layers have separate fixtures; no claim of released/installed
end-to-end validation is made. The tracked operational workaround remains until
a containing installation passes that check.

See [the contract](../spec/autonomous-context-handoff.md) for commands, recovery
states, compatibility boundaries and the executable runtime probe.
