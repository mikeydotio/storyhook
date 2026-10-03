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

## Restoration contract

The approved scope also requires restore-specific re-adoption of agent,
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
