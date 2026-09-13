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
