# Revivify integration (SH-825)

StoryHook v3.0.3 consumes RV-10's persistent server ownership contract. A
connection failure is not proof that a protected tmux process exited. Only
revivify may decide to create a successor. Its upstream contract is
`mikeydotio/tmux-revivify:docs/spec/server-ownership.md` (PR #10).

## Transport

`plugins/story/lib/tmux_target.py` is shared source, imported by the plugin
and composed into native helpers. Discovery uses the canonical logical socket's
SHA-256 key under the standard XDG state root, never the snapshot directory.
Only an absent record or a valid inactive tombstone is unmanaged. Unsupported,
unreadable, insecure or ambiguous records fail visibly. Private endpoints need
validated current or historical activation evidence; they never fall through
to ordinary startup.

Startup callers use the recorded executable's `server ensure --json` with the
logical socket and state directory. Observations use `server inspect --json`.
Success requires exit zero, restore readiness and agreement with the published
generation. Each operation then pins every tmux command to `-N -S endpoint`.
`-N` prevents a failed allocation from creating a replacement server.

The daemon's verification view retains default-server selection; interactive
helpers retain their caller's selection. Server environment filtering and
per-pane overrides remain separate. Integration calls share the helper's
existing deadline and never extend the daemon's startup publication deadline.

Exact-name session creation tolerates only a duplicate-session race, followed
by successful inspection of that exact session. All other errors propagate.

Native resource inventory and conflict-hold probes embed the same resolver.
They inspect ownership before testing socket existence and share one absolute
deadline across the bridge and tmux. Cancellation reaches both subprocesses.
A protected missing endpoint is unavailable, not evidence that an agent died.
Resource reports carry the observed private endpoint; their candidate leases
retain the original authority. Card-reset cleanup uses that captured endpoint
and refuses a generation change between reservation and cleanup.

The engine inspects ownership before census, liveness and adoption queries.
Numeric lane probes require a binding to the current private endpoint; a
logical name or predecessor endpoint returns `Unanswered` until explicit
re-adoption supplies the new binding. Adopted identity and activity reads use
one endpoint and deadline. Protected ownership or transport errors bypass
the ordinary missing-target classifier.

Native liveness, adoption and census clients force UTF-8 output with `-u`.
Without it, tmux replaces the inventory's tab delimiters under `LC_ALL=C`.
The older reset door captures one protected endpoint for its cleanup pass,
retains workspace ownership through teardown, and refuses logical or stale
numeric bindings. Its caller-window guard also recognizes the logical alias.
Store-free census and claim-comment clients use ambient discovery without
opening a store. Census is an inventory; a claim comment requires a current
pane binding and reports uncertainty before falling back to host-only text.

Python lifecycle helpers use `tmux_client.py` to cache the checked target only
within one operation. Nested calls share the existing deadline. Inventory
normalizes a server's proven logical/private alias to the actual endpoint;
numeric effects still require that endpoint as their explicit binding.
Protected endpoint failures never become an empty inventory. Process identity,
workspace ownership and continuation authority checks remain separate.

Bash dispatch ensures before terminal or checkout preflight. Resource-only
commands inspect first; only unmanaged servers retain the legacy socket
observation. The shell operation captures the private endpoint and rejects
selector changes. Self-window protection recognizes the proven logical alias.
An explicit private launch selector cannot follow a successor. Session startup
uses `=name` and rechecks a duplicate creation result against that exact name;
other creation failures retain their diagnostic and roll back as before.

## Restoration contract

Cleanup lease version 1 adds optional `tmux.revivify` with `logical_socket` and
`origin_generation`. Both fields are required when the object is present;
deserialization rejects a relative logical socket or malformed generation ID.
Legacy and unmanaged leases omit it and retain their wire shape. Dispatch
publishes this evidence from its captured target. `tmux.socket_path` remains
the current private endpoint. Provenance is corroboration, never sufficient
authority to rebind a pane or approve a plan.

The shared evidence reader validates the current generation's restore receipt
against exactly one retained source snapshot. Protected snapshots must carry
matching owner history and provenance. Legacy snapshots require the activation's
exact explicit selection. UUIDs and current pane IDs must be unique; restored
sessions must agree with the snapshot links. A changed activation, failed receipt,
missing source or conflicting map refuses adoption.

The dispatch proof joins that source to the retained lease, pane registration,
provider and conversation. A live candidate must retain RV-10's replay-wrapper
command and its exact current-generation UUID ticket. Native process observations
preserve argv boundaries and bracket cwd/ancestry reads with kernel incarnation
checks. They decode no environment values. Exactly one provider must run below
the captured pane, in the retained worktree, with the exact resume session;
fork arguments and changed ancestors refuse. These evidence helpers alone do
not publish a new binding or authorize input. The pinned RV-10 Darwin snapshot
records process start in UTC whole seconds; it must match the registered kernel
start time at that resolution. An unsupported clock cannot establish lineage.

Restoration clients explicitly request readiness. Ordinary clients remain
read-only. An operation which already captured one target cannot switch to a
successor when it later requests readiness.

Restoration uses explicit re-adoption of agent,
engine, reader and continuation identities. The provider maps pane UUIDs to
new pane IDs; old numerical IDs and PIDs cannot authorize re-adoption.
Generation lineage, restored options, repository/worktree/provider identity,
and a unique live process must agree before any durable binding changes.
Existing claims, conversations and work progress survive re-adoption.
Only already-authorized autonomous panes may receive a replacement watcher.
Ordinary manual Resume retains its fresh-session contract.

## Regression dependencies

The real transport suite exports provider commit
`7f77ee8997a9e984479a910c14735852b63b5c83` into a disposable `/tmp` directory.
Set `STORY_TEST_REVIVIFY_REPO` to a repository containing that object, or keep
the provider beside StoryHook's primary repository. A missing prerequisite is
a failure, never a skip. The suite does not install, migrate, or change routing
on the user's tmux servers.

The composed-helper tests preserve the original server identity, generation,
socket inode and session through listener saturation and endpoint errors.
Production time bounds remain unchanged; fixture patience uses load grace.

## Publication and continuation

The background engine pass ensures readiness before restoration. The synchronous
restart sweep remains read-only and retains its shared publication deadline.
Each restoration attempt shares one absolute deadline across readiness, proposal,
publication and watcher scheduling. Workspace exclusion uses the common Git
directory already proven by the proposal; the child revalidates that directory.

`restoration.py` joins the immutable snapshot and current receipt to one source
dispatch, the live UUID, RV-10's exact replay ticket, the native provider argv and
ancestry, the registered Git worktree, and the SessionStart/transcript witness.
It rechecks that proof before publication. Each local field must contain either
the proven source value or its exact derived replacement. Pane identity, window
continuation metadata, and the atomic cleanup marker therefore recover from an
interrupted prefix without granting a second conversation authority.

Native lane publication compares the entire observed lane, story event sequence,
and matching continuation records. Only physical binding fields change. Claims,
charters, provider conversations, message/turn identity, progress and reviewed
work remain intact. Retained manual and Auto dispatches receive the same proof
and continuation guards in the background; an engine-owned dispatch stays with
its engine. A conflicting or busy story does not prevent other dispatches from
being examined. No restoration operation sends a charter or starts a provider.

RV-10 live adoption intentionally has no replay receipt. Its explicit adopted
activation and matching `ownership-restore.json` authorize only a same-process
public-to-private alias rebind. The kernel incarnation must remain unchanged.
A predecessor private generation still requires snapshot/replay proof.

Approval watchers use one private endpoint and exact kernel process identity,
including a restored provider child when its pane process is a shell. A file lock
deduplicates live watchers; a process-bound completion option prevents replay.
Only the observed dialog transition after Return records completion. Probe
failure or a polling limit does not prevent a later watcher from trying again.
Only preserved Auto/Full Auto metadata and eligible current story policy can
rearm a watcher. The input boundary checks current state and reserved labels
again. A dead replay shell is not provider absence while its captured child lives.

Verification readers carry a pane-local native process witness. The view joins
that witness to the restored UUID and retains a healthy exact reader even when
the pane/PID marker changed. Replay shells that did not restart a reader remain
stale and can be replaced once. Restored verifier agents must prove the exact
provider conversation and process ancestry before they count as an existing agent.
