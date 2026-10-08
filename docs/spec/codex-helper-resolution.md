# Codex helper resolution cache (SH-816)

Engine ticks, dispatch options, dispatch and continuation callers share
`plugin::codex_installed_plugin_root`. It still asks `codex plugin list --json`
for the installed, enabled version and validates that version's manifest.
It never chooses an arbitrary directory from the plugin cache.

The resolver now keeps a process-local result cache: successful roots for
30 seconds, absent plugins and typed failures for 5 seconds. Lifetimes begin
when the probe finishes, so a slow probe is reusable. A short-lived CLI may
still need one probe; long-lived daemon callers share results. This implements
the story's short-cache option without changing provider registry authority
or the capabilities cache's digest and protocol checks.

Keys contain HOME, working directory, PATH, XDG config/data/state directories,
the selected Codex executable's identity and metadata, and metadata for
`~/.codex/config.toml`. Changed keys require another authoritative probe.
A successful cached root is also rechecked against its manifest and helper
metadata; a missing or nonregular manifest cannot produce a positive hit,
including when it disappears while the result is being published. Metadata
is inspected without reading provider configuration contents.

Same-key callers share an in-flight probe. Different keys never hold the map
lock while invoking a provider. A follower waits at most the existing provider
deadline plus termination grace (currently 65 seconds), then returns a named
error. Context changes retire the answer instead of publishing it under an
outdated key. The map holds at most 32 entries and evicts only unreferenced,
idle entries; if every entry is active it returns a retryable diagnostic.
A probe panic also releases its entry and wakes followers.

Codex installation and removal invalidate the current HOME before and after
the existing guarded mutation, including errors and rollback. Invalidation
retires in-flight entries, so old probes cannot repopulate them. Installation's
exact-version verification continues to invoke the provider directly. External
changes are observed through metadata changes or cache expiry; registry-only
changes without a metadata change may remain cached until the short TTL ends.
The existing explicit helper overrides and file-first control verbs retain
their precedence. No registration or provider configuration is changed by
resolution itself.

The twelve `plugin::codex_cache::tests` cases use private temporary files,
logical expiry clocks, injected registry results and bounded thread rendezvous.
They cover reuse, expiry, typed negative caching, context and file changes,
registry version authority, same-key coalescing with unrelated-key progress,
in-flight invalidation, panic recovery, cardinality, publication races and both
mutation invalidation boundaries. No test invokes a real provider.
