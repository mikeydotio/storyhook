# Compatible CLI consumer audit (SH-901)

StoryHook v3.0.3, approved package A. This audit covers the command model in
SH-898, explicit JSON input in SH-899 and offline discovery in SH-900. The
[approved design PR](https://github.com/mikeydotio/storyhook/pull/984),
[SH-755 audit](SH-755-cli-audit.html) and
[agent CLI specification](../spec/cli-agent-usability.md) are the sources.
Focused tests are evidence for these contracts, not an integrated release gate.

## Migration examples

```sh
story describe --json
story describe set --json
story describe verifier --audience operator --json
story describe verifier repair-admit --audience internal --json
story describe --audience all --json
story --json set SH-42 --input-json '{"complexity":"medium"}'
```

The discovery document has schema `storyhook.command-discovery`, version 1,
contract `legacy-compatible`, and sorted canonical paths. Default audience is
`task`. Audience filters visibility, not execution permission. Dynamic enum
sources are named, not queried. Discovery works outside a project and never
opens a store or launches the described helper. Project/store global flags are
accepted but unused for discovery. `describe --help` describes the help flag;
use `help describe` for human instructions.

Existing `story --json set SH-42 --json '{"complexity":"medium"}'` remains
supported. Migrate input selection to `--input-json` independently of JSON
output selection. Do not pass both input forms: this is an exit-2 refusal before
mutation. Empty, malformed, non-object and unknown-field inputs still fail
atomically. No stdin/file convention or automatic consumer rewrite is added.

## Accepted A rows and evidence

Tests named below are newly added; prior regression suites are not claimed run.
The command-model and discovery tests compare metadata with actual parsers and
selected handlers. The consumer fixtures invoke the binary in private HOME,
XDG, store and project directories, own every long-lived child, and use bounded
waits. Provider forwarding uses a private fake executable and plugin cache.

| Accepted row | Evidence |
| --- | --- |
| One command model | `cli_command_model`: every registered path has a real parser/early-handler witness, exact legacy help fixture, flag scope and invalid-input checks. `cli_discovery`: every registered path appears once across audiences; offline binary leaves fixture filesystem unchanged. |
| Task/operator/internal separation | `cli_discovery::audiences_partition_every_registered_path_and_filter_subcommands` checks subcommands, including verifier operator and internal paths. Visibility is explicitly not authorization. |
| Explicit JSON input | All eight `cli_json_input` cases plus `cli_consumer_contract::input_migration_and_legacy_next_shapes_work_for_real_consumers`: actual new/legacy input and atomic refusal. |
| Aliases and lifecycle | `cli_consumer_contract::aliases_and_legacy_help_termination_remain_compatible` and `lifecycle_verbs_and_real_previews_have_distinct_observable_effects`; model parser witnesses cover the remaining alias spellings. |
| Output capabilities | Consumer tests cover envelope success/empty/error, raw export round trip, JSONL follow framing, delegated helper streams/status, quiet errors and terminal cancellation. Discovery tests check the five declared classes and protocol exceptions. |
| Mutation/preview metadata | Consumer tests assert byte-identical exported data after supported previews, unsupported flags, confirmation refusal and guarded conflicts. Discovery tests compare generic confirmation against actual `Invocation::forced()` behavior and parse declared previews. |

## Lifecycle and alias distinctions

`relate`/`link` and `unrelate`/`unlink` manipulate story relationships. `project
link` registers a project origin or checkout; it is a different command path.
`context` aliases `load-context`; `sync-git` aliases `commit-sync`. `states`,
`is`, `awaits` and `priority` are help-only aliases for `state`, `move`, `block`
and `prioritize`; they do not become executable verbs.

`close` abandons a story into Dropped and records its reason. `archive` hides a
closed story while preserving its data. `delete --force` permanently deletes
it. `unclaim` releases a claim and restores its prior state, with a documented
fallback if restoration is impossible. `reset` removes owned runtime/workspace
resources and returns the story to Todo; it is not claim release or deletion.
The consumer reset fixture has no owned external resources; production resource
custody remains covered by its dedicated stories, not asserted by this audit.

## Output and retry rules

Consumers must select the declared output mode before parsing. `--json` does
not wrap exports, context documents, JSONL logs or delegated helper streams in
one universal envelope. `--quiet` suppresses ordinary successful responses but
not errors; raw export, discovery and delegated helpers keep their respective
contracts. Follow readers must accept complete newline-delimited records; a
partial final line is not yet a record. Log-reading failures still use ordinary
error output rather than promising that all failure bytes are JSONL.
Ordinary errors use `result: "error"`; guarded state conflicts instead use
`result: "conflict"` with expected/actual state and process status 9. `set`
returns a message envelope; read `show` to obtain the updated story.

The stable plugin launcher supports Codex. It forwards both helper streams and
its exit status, including nonzero status, even with JSON/quiet globals. GitHub
helper failures instead map through StoryHook's error status. SessionStart can
return unavailable-context output plus a warning, usually with status zero;
it is not a universal empty-on-error interface. Interactive questionnaire output
belongs to the terminal; nonterminal callers must provide required fields.

An ordinary read-only store command may start a daemon and create runtime files.
Preview support is per-command; unsupported `--dry-run` is an error. A lost reply
is not cancellation or proof of failure. Read current state and reconcile before
retrying a write; discovery grants no universal idempotency or retry guarantee.

## Retained and deferred behavior

The source audit starts after SH-752 removed assignment and SH-756 introduced
explicit complexity/policy resolution. It includes later atomic `new
--blocked-by`, `project settings automations.enabled`, Reset previews, and
private recovery protocols. Discovery names dynamic settings rather than
fetching configured values; unassessed medium remains different from explicitly
assessed medium. `final_surface_keeps_assignment_retired_and_later_capabilities_reachable`
checks the retired interface, actual assessed complexity, dependency readiness,
project setting access and discovery of the later protocol paths. Nothing here
restores assignment or changes dispatch policy.

Legacy `new -- --help` still returns help instead of creating literal text.
Legacy `next --count 1` returns a single-story envelope when available; a
requested count greater than one returns a stories array even when only one is
available. Multi-item `next` orders dependencies and can include a dependent
after its blocker; use `list --ready` for the strict readiness filter. An empty
queue returns the existing message envelope. Package B's
literal termination and stable queue schema changes remain deferred. There is
no `--cli-contract 2` implementation or silent default flip.

The grammar describes canonical forms. Legacy parsers retain permissive option
placement, duplicate handling and service validation; metadata is not an exact
recognizer for every accepted legacy token sequence. Focused fixtures cover
representative destructive refusals and output boundaries, not every production
remote/helper/resource path. Full-suite and release validation remain separate.
