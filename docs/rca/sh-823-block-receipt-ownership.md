# SH-823: Block-delivery receipt ownership

Investigation: 2026-10-02 PDT, StoryHook v3.0.3, starting at `5264e53d`.

## Finding and limits

SH-823 asks whether an agent-block comment about SH-768 was sent to all
stories. The story had no reproduction steps, screenshot, or discussion
beyond its title and dispatch comment. Historical broadcast was not reproduced.

The read-only investigation used `story show SH-823 --json`,
`story load-context --story SH-823 --format json`, and `story list --all --json`.
The last command includes closed and archived stories.

| Evidence at investigation time | Result |
| --- | --- |
| Existing stories | 858 |
| Distinct comments starting with `AGENT BLOCK DELIVERY` | 119 |
| Receipt text appearing on more than one story | 0 |
| Receipt #107, interrupt delivered, 2026-09-26 18:23:29 UTC | SH-768 only |
| Receipt #108, resume delivered, 2026-09-26 18:41:50 UTC | SH-768 only |

This is evidence about retained current history, not proof of what an earlier
browser session displayed. Deleted stories, retracted comments, and an absent
historical browser trace cannot be reconstructed from that read.

## Ownership through the implementation

| Boundary | Owner used |
| --- | --- |
| Effective block derivation | Each story row's number within its project |
| Delivery storage | Composite foreign key `(project_id, story_no)` |
| Acknowledgement | Delivery ID, project ID, story number, expected status |
| Receipt event and fold | The delivery's project and story number |
| Dashboard comments | The selected story snapshot's comments |

The worker calls the helper with the delivery's project slug and story ID.
The helper's diagnostic is quoted evidence; it does not select the event
destination. A dependency transition can generate several distinct deliveries
for affected stories. That is not a shared receipt or a broadcast.

The investigation found no reason to change delivery routing, schema, or
dashboard rendering. The acknowledged limitation is that the old receipt
heading named the delivery number and outcome but not its story. Some helper
diagnostics name a story; others say only that no agent was reached.

## Change

New receipt headings append the persisted owner's ID:

```text
AGENT BLOCK DELIVERY #107 — interrupt delivered — SH-768
```

The prefix, delivery number, action, outcome, and quoted diagnostic retain
their existing roles. `finish` derives the owner from the same project prefix
and story number used to append the event. Historical comments are not
rewritten. This is a diagnostic clarification with isolation coverage, not a
claim that cross-story corruption was found and repaired.

## Regression coverage

`tests/block_delivery/receipt_ownership.rs`, inside the existing
`block_delivery` integration target, exercises the real service, SQLite store,
and worker. Only the external helper acknowledgement is a fixture.

- Interrupt and resume outcomes: delivered, unreached, and uncertain.
- Restart recovery and worker supersession, including repeated idle passes.
- Queued deliveries across two projects with overlapping story numbers.
- Unrelated stories keep their event heads and snapshots unchanged.
- Reopening a dependency produces one distinct receipt per affected dependent.

`e2e/specs/block-receipt-ownership.spec.ts` stores distinctive receipt-shaped
fixture comments through the real API, then navigates the production dashboard.
It delays genuine detail and board replies with the existing `holdFetch`
helper. It checks both rendered comments and retained API data. No browser
behavior is mocked.

The new heading assertion is the expected red-to-green contract. The isolation
checks are preventive coverage, not evidence of a historical reproduction.

## Validation

| Focused check | Result |
| --- | --- |
| Five new worker tests before the heading change | All five failed on the missing owner suffix |
| Same five tests after the change | 5 passed; 16 existing cases filtered out |
| Six golden cases with changed snapshots | 6 passed; 24 unrelated cases filtered out |
| New dashboard spec, WebKit | 1 passed, 7.5 seconds |
| New dashboard spec, Chromium | 1 passed, 8.5 seconds |
| Clippy, changed Rust targets, `-D warnings` | Passed |
| Rustfmt, changed Rust files; `git diff --check` | Passed |

The first golden run stopped at the shared fixture's old exact acknowledgement
string; the remaining cases inherited its poisoned `LazyLock`. Updating that
exact expectation to include the owner, without weakening it, produced the
six-case green result above.

Commands:

```sh
cargo test --offline --test block_delivery receipt_ownership:: -- --test-threads=1
cargo test --offline --test golden_cli -- show_human show_json list_json search_json phase_json export_document --exact --test-threads=1
bash scripts/run-e2e.sh --project=webkit block-receipt-ownership.spec.ts
bash scripts/run-e2e.sh --project=chromium block-receipt-ownership.spec.ts
cargo clippy --offline --test block_delivery --test golden_cli -- -D warnings
```

The initial browser command specified both project flags; this harness uses
only the last, so it ran WebKit. Chromium was then run separately. Both used
isolated real daemons, and the harness removed the created fixture stories.
No full suite, deployment, or live-story test mutation was performed.
