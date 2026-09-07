# Release observer — SH-579

The release script is intentionally bypassable. SH-580 fixed its branch/bump
abort, and SH-576/578 fixed its pinned toolchain and Lima failures, but none
provided an independent observer. A green ordinary test run does not exercise
those host capabilities. The explicit ungated-release option remains intact.

## Commands and evidence

| Command | Contract |
| --- | --- |
| `make release-watch` | One serialized remote tag audit and real release preflight |
| `make release-status` | Read-only status, no network or guest startup |
| `make release-watch-plist` | Print hourly launchd configuration; never install it |

The observer uses an independent checkout under the repository's shared Git
directory, `storyhook/release-observer/checkout`. It fetches `main` and all tags
into private remote-tracking namespaces with `--no-tags` and pruning. Remote
tag moves/deletions are reflected without touching the developer's local tags.
Git documents these [fetch/refspec semantics](https://git-scm.com/docs/git-fetch).
The observer never repairs tags or ignores historical mismatches.

Each release-shaped tag uses the existing `release_version_is_valid` predicate.
An annotated tag is peeled to a commit; non-commit tags fail. The shared
`release-tag-commit.sh VERSION [REVISION]` resolver asks about each tag's own
history, so later VERSION bumps cannot invalidate an earlier correct tag.
Diagnostics identify incorrect targets and expected VERSION-changing commits.

Every pass runs `build-release-assets.sh --check` against fetched `origin/main`,
even when its tree is unchanged and even when tag validation fails. This probes
the real pinned host toolchain and Lima guest; it does not assemble archives,
prove artifact provenance, run the full suite, or produce a gate receipt.
Network/checkout failures prevent probing unknown source and are recorded as
failed observations. Tracked edits in the observer checkout are refused, never
reset or silently executed. The existing machine lock serializes observer
passes across repositories. Ordinary release invocations do not take this lock;
do not manually run release assembly concurrently with the observer.

`latest.json` is atomically replaced at start and finish. It contains schema 1,
commit/tree, start/finish Unix timestamps, PID and process start identity,
separate audit/preflight exit codes, and a unique persistent diagnostic log.
Unattempted checks have null exit codes, never success. A newer failure replaces
an older success. Interrupted runs, invalid records and missing logs fail loud.

Status is missing, running, failed, stale, or successful. Staleness means either
the locally known `origin/main` tree differs or two hourly intervals have elapsed.
The exact age is always shown for valid records. This is observation freshness,
not a timeout on the preflight. A status read cannot see a remote update that
nobody has fetched yet. `release-watch` exits 0 only when both checks pass;
`release-status` exits 0 only for successful/current evidence. All ordinary,
changed and full test bodies print the advisory without changing gate outcomes.

## Optional machine setup, after merge

Use a durable standalone checkout containing this change, never an agent lane.
Configuration generation refuses linked worktrees. It captures the invoking
PATH so installed `git`, `python3`, `limactl`, and toolchain utilities resolve
under launchd. Inspect it before installation. Configure HTTPS credentials
noninteractively; an SSH remote may require unavailable 1Password interaction.
The existing preflight can provision caches and the Lima guest on its first run.

From that durable checkout:

```sh
make release-watch-plist > /tmp/io.mikey.storyhook.release-observer.plist
plutil -lint /tmp/io.mikey.storyhook.release-observer.plist
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
install -m 644 /tmp/io.mikey.storyhook.release-observer.plist "$HOME/Library/LaunchAgents/"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/io.mikey.storyhook.release-observer.plist"
```

The job runs at load and at minute zero each hour. Apple's
[calendar scheduling](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html)
catches up after sleep. Standard output/error go to
`~/Library/Logs/io.mikey.storyhook.release-observer.log`; per-pass logs remain
beside the observation record. Keep the durable checkout updated after future
observer changes. No timer is installed by this PR. Without setup, normal test
output explicitly reports missing observations rather than suggesting coverage.

## Validation

`tests/release_observer.rs` runs the Python standard-library contracts against
real temporary Git repositories and remotes. Only the external preflight command
is a fixture: no modeled Git, observer, lock or status behavior. Cases cover
historical/invalid tags, independent outcomes, remote moves/deletions, reruns,
dirty checkouts, unavailable remotes, record corruption/interruption/staleness,
and competing public wrapper processes. Fixtures never start Lima.

The targeted release-tagging and tier contracts preserve existing release
semantics and prove advisory wiring. Actual host preflight is deliberately not
run from the implementation lane; centralized verification owns the full suite.
