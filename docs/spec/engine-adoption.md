# Live engine configuration and manual dispatch adoption (SH-700)

Version: v2.4.2. The CLI gains `engine configure` and `engine adopt`.

Configuration patches only supplied flags, in the same write transaction that
reads current configuration. REST configuration remains a full replacement.
Occupied lanes survive contraction; future claims honor the new capacity.

Adoption is explicit, atomic for the requested IDs, and bounded by the current
run capacity. It requires claimed, unblocked, non-epic work in the run scope,
a valid Git worktree cleanup lease, and the exact live provider pane on the
leased tmux socket. It does not launch or change the existing agent. `no-auto`
is eligible when explicitly named; `human-only` is not.

External observations occur outside SQLite transactions and are revalidated
before commit. The commit rechecks story versions, scope, run state, ownership,
and capacity. Identical retries do not consume capacity twice.

Adopted provenance survives restart. Adopted bindings release on verification,
closure, or ordinary unclaim; resources remain with the verifier/operator.
Blocked or failed work uses existing quarantine handling. Engine-dispatched
lanes retain their current verification occupancy behavior. All pane monitoring
uses the captured lease socket, since pane IDs are server-local.

Automatic adoption at start is outside this change. Tests use isolated stores,
Git repositories, and test-owned tmux sockets; live runs are not test fixtures.

## Persistence and operational identity

Schema 40 adds nullable `adopted_identity_json`: provider, original pane PID,
and window ID. Existing rows remain NULL. Adopted rows must carry their story,
pane, worktree, and cleanup lease and be working or quarantined. CLI and HTTP
lane views expose optional `adopted_identity`; existing wire shapes omit it.

Inspection uses the registered repository's worktree inventory, validates the
existing marker reader's repository/branch/path contract, and probes the marker's
socket explicitly. Default StoryHook windows use the exact story ID; ambiguous
names refuse adoption. Configured process-pattern/launch overrides retain the
notify acceptance rules. PID changes are replacement evidence even when tmux
respawns the same pane. External state cannot participate in a SQLite transaction:
two matching observations precede the commit, and subsequent reconciliation
uses the captured process identity to catch replacement after that boundary.

A capacity reduction does not invalidate an identical adoption retry: a retry
adds no lanes. New ownership still requires capacity. Adopted binding release
is recorded as `adopted-released`; it performs no resource cleanup. A manual
session retains its original launch options and interactive/full-auto mode.

## Reserving room before adoption

For a live run, pause before raising capacity and adopting manual work, then
resume. This keeps the reconciler from filling the intended adoption slots:

```sh
story engine pause
story engine configure --lanes 6
story engine adopt SH-694 SH-691 SH-692
story engine resume
```

These are operator examples, not development commands. Adoption may also run
while Running when sufficient idle capacity is still available; its transaction
refuses safely if another claim wins those slots first.
