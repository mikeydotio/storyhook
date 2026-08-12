use std::collections::BTreeMap;

/// Extended help topics for `story help <command>`.
/// Each topic provides agent-friendly guidance: when to use, examples, and related commands.
pub fn get_help_topic(topic: &str) -> Option<&'static str> {
    TOPICS.get(topic).copied()
}

pub fn list_topics() -> Vec<&'static str> {
    TOPICS.keys().copied().collect()
}

static TOPICS: std::sync::LazyLock<BTreeMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| {
        let mut m = BTreeMap::new();

        m.insert(
            "project",
            r#"story project new [--prefix <PREFIX>] [--name <NAME>]
                  [--attach <PATH> | --no-attach] [--no-agents-md]
story project delete [--force]
story project set-prefix <NEW-PREFIX> [--force]
story project show
story project list
story project link origin [URL] | link checkout [PATH]
story project unlink origin [URL] | unlink checkout
story project settings list | get <key> | set <key> <value> | unset <key>

A repository's whole lifecycle with storyhook: register one, list them,
remove one, attach the git associations it answers to, and change how
storyhook treats this one.

NOT TO BE CONFUSED WITH
  story link / story unlink  are aliases for 'story relate' /
  'story unrelate' and join one STORY to another. They have nothing to
  do with the project verbs below, which are about git.

new
  Creates the project in storyhook's store with the states every
  project must have (todo, in-progress, blocked, done) and default
  types, writes .storyhook.toml naming it, and generates an AGENTS.md
  if the repository has none.

  With NO flags at a terminal it asks you the six questions instead.
  Any switch at all — or --json, or no terminal — makes it fully
  non-interactive, and then --prefix is required: it is minted into
  every story id this project ever creates, so it is never defaulted
  for you. 'story project set-prefix' can change it later, but only
  by rewriting every relationship this project's stories claim — treat
  it as effectively permanent and choose carefully.

  Commit .storyhook.toml. It is how a fresh clone — or a linked worktree
  — knows which project this checkout belongs to before it has a local
  database row to consult.

  Idempotent: run again and it re-registers this checkout, leaving the
  catalog and the prefix alone. --name may be given at any time.

  --attach names the checkout, and defaults to the directory you are
  standing in; a relative path resolves against that directory.
  --no-attach writes the store record and touches no directory at all,
  which is the shape for a project whose repository lives on another
  machine. There is no positional: a bare word after 'new' could be a
  name or a path with equal plausibility.

  If the repository still keeps its stories in a .storyhook/ directory,
  new refuses and points you at 'story migrate' — creating one would
  mint an empty second project beside data you still have.

delete
  Permanently deletes the project, every story, every event, every
  checkout registration and every registered origin. There is no undo.

  It always asks first, and the confirmation is the project's slug typed
  in full. --force skips the question; with --json, or with no terminal
  to ask at, --force is required rather than assumed either way.

  It takes no path or slug: it deletes the project this directory
  resolves to, or the one --project names. That is how a project whose
  checkout is gone is reached.

  It touches no files. The .storyhook.toml and AGENTS.md in every
  checkout are left exactly where they are — the warning lists those
  directories so you know which ones are now claiming an identity that
  does not exist.

set-prefix
  Renames the project's story-id prefix — SH-1 becomes AGE-1 — and
  rewrites everywhere the old one is embedded: the project row, every
  relationship any of its stories claim (via real compensating events,
  never a silent edit), and any github-sync merge-base snapshots. If
  this checkout has one, its .storyhook.toml is updated too.

  Nothing is deleted; every story, event and comment survives. What
  cannot be undone is the prefix itself: every id already written down
  anywhere under the old one — a commit message, a document, a browser
  tab — stops resolving the moment this runs.

  It always asks first, and the confirmation is the new prefix typed
  in full. --force skips the question; with --json, or with no
  terminal to ask at, --force is required rather than assumed either
  way.

  It takes no path or slug: it rewrites the project this directory
  resolves to, or the one --project names. Refuses if the prefix given
  is invalid, is the one this project already has, or already belongs
  to another project in this store.

  Free-text description and comment bodies are left untouched — there
  is no reliable way to tell a genuine story-id reference in prose
  from text that only looks like one, so nothing there is rewritten.
  A backup of the whole store is taken automatically before anything
  is written.

list
  Every project the store knows, including any whose checkout is not on
  this machine. This is the same set the dashboard shows. Under each,
  its registered origins and its linked checkout, if it has them.

link origin [URL] / unlink origin [URL]
  The origins this project answers to. A checkout of a registered origin
  resolves to this project with no flag, no pointer file and no recorded
  path — which is what lets a fresh clone, on this machine or another,
  work immediately.

  One project may hold MANY origins (a repository that moved; a second
  canonical remote), and an origin belongs to AT MOST ONE project.
  Registering one another project holds is refused, naming that project.

  URL may be omitted, and then it is the origin THIS directory's own
  repository records — storyhook reads exactly
  'git config --get remote.origin.url'.

  Either way, only the directory that OWNS an origin may register it:
  the repository's main working tree. That command walks up the
  directory tree and answers the same in every worktree, so from a
  subdirectory or a linked worktree it would otherwise register the
  enclosing repository's identity against this project, permanently,
  and lock out every other project in that repository. A project inside
  a larger repository is identified by its committed .storyhook.toml
  instead, or by --project.

link checkout [PATH] / unlink checkout
  Where this project's repo-side work runs. AT MOST ONE per project, and
  never consulted to decide which project you are in — linking a
  directory does not make commands run there resolve to this project.
  PATH defaults to the current directory; linking a second one replaces
  the first and says so.

settings
  Read and write this project's settings — the handful of per-project
  values that change how storyhook treats it. See 'story help
  project-settings' for the keys and what each one does.

Examples:
  story project new                     # Asks: six questions, then creates
  story project new --prefix API        # Fully stated → API-1, API-2, ...
  story project new --prefix TH --attach ~/code/thing
  story project new --prefix OFF --no-attach --name "on another machine"
  story project list
  story project link origin             # This checkout's own origin
  story project link origin git@github.com:me/thing.git
  story --project thing project link checkout ~/code/thing
  story project settings list
  story project delete                  # Asks before destroying anything
  story --project old-thing project delete --force
  story project set-prefix AGE          # Asks before rewriting anything
  story --project thing project set-prefix TH --force

Related:
  story help project-settings — The settings keys, in detail
  story help relate — story link/unlink, which are about STORIES
  story migrate     — Bring a .storyhook/ repository into the store
  story member add  — Add team members after creating a project
  story new         — Create your first story
"#,
        );

        m.insert(
            "project-settings",
            r#"story project settings list
story project settings get <key>
story project settings set <key> <value>
story project settings unset <key>

This project's settings: the per-project values that change how
storyhook treats it. They live in storyhook's store, alongside the
project itself, and travel with it rather than with a checkout.

Three different things wear the word "config" here. This is not the
other two:

  story project settings — per PROJECT, in the store. This command.
  story set <id> ...     — per STORY. Sets a story's fields (title,
                           state, priority, assignee). Nothing to do
                           with a project.
  .storyhook.toml        — per REPOSITORY. Its [plugin] and [hooks]
                           tables are decisions about this checkout,
                           versioned with the branch and carried by a
                           clone. Edit the file; storyhook does not
                           write those for you.

Settings:
  sync.auto_transition    true|false, default true
    Whether 'story commit-sync' moves a story a commit CLAIMS into the
    active state. A commit claims a story with a word like closes,
    fixes or starts immediately before the id; a bare mention such as
    'Refs SH-1' only ever records a link. See 'story help commit-sync'
    for the whole rule. Turn this off in a repository where even a
    claim should record a link and nothing more.

  doctor.stale_threshold  a duration such as 14d, 2w, 36h, 90m
    How long a story may sit untouched before it counts as stale.
    NOTE: no command reads this yet. You can store a value, and
    'story doctor' will not act on it. The listing says so too.

  github.sync             read-only
    The github-sync document: etags and story-to-issue mappings. It is
    listed and readable here, but only 'story github-sync' writes it —
    its contents have to agree with state this command cannot see.

list reports every setting with the value in force and where that value
came from:

  set      you wrote it on this project
  default  you have not written it, and the code applies this value
  unset    you have not written it, and nothing applies

Those last two are different answers, which is why they are different
words. An unwritten sync.auto_transition is 'true' and in force; an
unwritten doctor.stale_threshold means no threshold exists at all.

unset clears a value, returning the setting to whichever of those it
was born as. Unsetting something you never set is not an error.

Examples:
  story project settings list
  story project settings get sync.auto_transition
  story project settings set sync.auto_transition false
  story project settings set doctor.stale_threshold 14d
  story project settings unset doctor.stale_threshold
  story project settings list --json     # source and value as fields

Related:
  story project      — init, delete and list
  story commit-sync  — What sync.auto_transition governs
  story github-sync  — What owns the github.sync document
  story set          — Change a STORY's fields, not a project's settings
"#,
        );

        m.insert(
            "new",
            r#"story new <title> [--state <slug>] [--type <slug>] [--description <text>]
              [--priority <level>] [--assignee <member>] [--label <name> ...]
              [--labels <csv>] [--draft]

Create a new story with the given title. Returns the assigned ID.
All flags are optional — everything but the title can also be set
later via 'story set'. --label may be repeated; --labels accepts a
comma-separated list (both may be combined). Comma is always the
label delimiter, even inside a single --label value — a label can
never contain one.

--draft creates the story as a draft: it claims an id like any other
story, but is excluded from 'story next'/'--ready' and shown inline
in 'story list' with a [draft] badge (or filter to drafts-only with
'story list --drafts'). 'story publish <id>' makes it live — one-way,
so there is no flag to undo it.

When to use:
  When you have a discrete piece of work to track. For bulk creation
  from a spec, use 'story decompose' instead. Use --draft for an
  idea you're still shaping and don't want surfacing as ready work yet.

Examples:
  story new "Implement authentication middleware"
  story new "Fix: login page returns 500 on empty password"
  story new "Refactor database connection pooling" --json
  story new "Add rate limiting" --priority high --label backend --label api
  story new "Investigate flaky test" --description "Fails ~1 in 20 runs in CI"
  story new "Sketch: notification preferences" --draft

Related:
  story publish <id>     — Make a draft live (one-way)
  story decompose        — Create multiple stories from a spec file
  story set <id>          — Change any field after creation
  story prioritize <id>  — Set priority after creation
  story label <id>       — Add labels after creation
"#,
        );

        m.insert(
            "list",
            r#"story list [filters]

List open stories with optional filters. Returns all open stories by
default. Combine multiple filters to narrow results.

When to use:
  When you need to browse or filter the backlog. For the single best
  task to work on next, use 'story next' instead.

Filters:
  --state <slug>          Filter by state (e.g., todo, in-progress)
  --assignee <id>         Filter by assignee member ID or GitHub handle
  --priority <levels>     Comma-separated: critical,high,medium,low,none
  --label <labels>        Comma-separated label filter
  --phase <N>             Filter by phase number
  --flagged               Only stories with integrity warnings
  --blocked               Only stories blocked by dependencies or awaiting
  --ready                 Only stories ready to work on (unblocked)
  --stale <duration>      Stories not updated in duration (e.g., 2d, 1w)
  --created-after <date>  Stories created after ISO date
  --updated-after <date>  Stories updated after ISO date
  --drafts                Only draft stories (they otherwise show inline
                           with a [draft] badge — see 'story help new')

Examples:
  story list                          # All open stories
  story list --ready                  # Unblocked, ready to work on
  story list --phase 1                # Stories in phase 1
  story list --priority critical,high # Only critical and high priority
  story list --blocked --json         # Blocked stories as JSON
  story list --stale 3d               # Not updated in 3 days
  story list --drafts                 # Only drafts, to pick one to edit

Related:
  story next     — Get the highest-priority ready story
  story publish  — Make a draft live
  story summary  — Aggregate counts by state and priority
  story search   — Full-text search across stories
"#,
        );

        m.insert(
            "next",
            r#"story next [--count <n>]

Get the highest-priority ready stories. A story is "ready" when all its
predecessors are closed and it has no awaiting blockers.

When to use:
  At session start to pick your first task, or after completing a story
  to find the next one. Prefer this over 'story list' when you need a
  single actionable item.

Examples:
  story next                # Top-priority ready story
  story next --count 3      # Top 3 ready stories
  story next --phase 1      # Top-priority ready story in phase 1
  story next --json         # Structured JSON output

Related:
  story load-context  — Full project overview (use first in a new session)
  story list          — All stories with filters (use for exploration)
  story graph         — Dependency visualization (use to understand blockers)
"#,
        );

        m.insert(
            "state",
            r#"story state list
story state add <slug> --super OPEN|CLOSED [--role active] [--description "<text>"]
story state set <slug> [--super OPEN|CLOSED] [--role active|none]
                       [--description "<text>"] [--no-description]
                       [--move-stories-to <slug>]
story state remove <slug> [--move-stories-to <slug>]
story state reorder <slug,slug,...>

Configure the project's states — the vocabulary every story moves
through, stored with the project. Each state maps to a
superstate (OPEN or CLOSED) that decides whether stories in it count as
open work; moving a story into a CLOSED state closes and archives it.

State order matters: it is the column order on the web dashboard's
board, and the first OPEN state is where new stories land.

When to use:
  Setting a project up ('review', 'verifying', 'wont-fix'), or adjusting
  the workflow later. The same edits are available in the web dashboard
  (Settings -> Statuses) and the TUI (press 's').

Examples:
  story state list
  story state add review --super OPEN --description "Waiting on a reviewer"
  story state set review --role active
  story state set review --no-description
  story state reorder todo,in-progress,review,blocked,done
  story state remove review --move-stories-to todo

Moving stories out of the way:
  Changing a state's superstate, or removing it, would leave the stories
  sitting in it misfiled — so both refuse until you say where those
  stories go:

    story state remove review --move-stories-to todo

  Stories moved into a CLOSED state are closed and archived, exactly as
  'story move' would. A state that already has ARCHIVED stories cannot
  be removed at all: their history records the slug, and reopening one
  later would fail against a state that no longer exists.

Rules:
  - Slugs are lowercase letters, digits, and single dashes ('in-review').
    They are typed as CLI arguments and appear in dashboard URLs.
  - Every project keeps 'todo', 'in-progress' and 'blocked' as OPEN
    states and 'done' as a CLOSED one. They cannot be removed, and
    their superstates cannot be changed; anything else you add is
    yours to arrange. A project that predates this rule reports it in
    'story doctor', and 'story doctor --fix' adds what is missing.
  - A state set always keeps at least one OPEN and one CLOSED state.
  - At most one state may carry --role active, which marks the state a
    story enters when work starts (used by 'story commit-sync').
  - There is no rename: a slug is recorded in every state-change event
    ever written. Add the new state, migrate, and remove the old one.

Related:
  story move    — Move one story between states
  story type    — Configure story types instead of states
  story summary — Story counts per state
"#,
        );
        m.insert("states", m["state"]);

        m.insert(
            "summary",
            r#"story summary

Show project summary with story counts by state and priority, plus
lists of ready, blocked, and stale stories.

When to use:
  For a quick overview of project health. Use 'story load-context' for a
  more comprehensive session-start document.

Examples:
  story summary             # Human-readable summary
  story summary --json      # Structured JSON output

Related:
  story load-context  — Full project context document
  story list          — Detailed filtered listing
  story report        — HTML or text report
"#,
        );

        m.insert(
            "load-context",
            r#"story load-context [--format markdown|json]

Generate a comprehensive project context document suitable for AI agent
session initialization. Includes project state, open stories, blocked
items, ready tasks, and phase progress (if any stories have phase:N labels).

When to use:
  At the start of every session. This is the primary command for
  understanding what's happening in the project.

Note: Previously named 'story context'. The old name still works as an alias.

Examples:
  story load-context                       # Markdown format (default)
  story load-context --format json         # JSON format
  story load-context --format markdown     # Explicit markdown

Related:
  story next     — Pick the next task to work on
  story handoff  — Generate end-of-session summary
  story summary  — Quick state/priority counts
  story phase    — Phase management commands
"#,
        );

        // Keep old name as alias
        m.insert("context", m["load-context"]);

        m.insert(
            "phase",
            r#"story phase list|show|add|remove|create

Manage story phases. Phases are a convention on labels: a story in
phase 2 has the label "phase:2". Phase commands are sugar over the
existing label system.

Subcommands:
  list                     Per-phase progress overview
  show <N>                 List stories in phase N
  add <id> <N>             Assign story to phase N (removes old phase)
  remove <id>              Remove phase assignment
  create <N> ["<title>"]   Create a grouping story for the phase

When to use:
  When work is organized into sequential phases (e.g., from a spec
  decomposed with ### Wave N headings). Use 'story decompose' to
  auto-assign phases from Wave headings.

Examples:
  story phase list                           # Overview of all phases
  story phase show 1                         # Stories in phase 1
  story phase add SH-5 2                     # Move SH-5 to phase 2
  story phase remove SH-5                    # Clear phase assignment
  story phase create 1 "Foundation"          # Create phase grouping story
  story list --phase 1                       # List stories in phase 1
  story next --phase 2                       # Next ready story in phase 2

Related:
  story decompose     — Wave headings auto-assign phase labels
  story label         — Manual label management
  story load-context  — Shows phase progress when phases exist
"#,
        );

        m.insert(
            "handoff",
            r#"story handoff [--since <duration>]

Generate a session handoff document summarizing what changed in the
given time window. Includes stories created, updated, and completed.

When to use:
  At the end of a work session, or when switching contexts between
  different projects or tasks.

Examples:
  story handoff                   # Default: last 2 hours
  story handoff --since 4h        # Last 4 hours
  story handoff --since 1d        # Last day

Related:
  story load-context  — Session start (pair with handoff at session end)
  story commit-sync   — Link git commits to stories before handoff
"#,
        );

        m.insert(
            "search",
            r#"story search <query>

Full-text search across story titles, comments, and labels. Searches
both open and archived stories.

When to use:
  When you know part of a story's title or content but not its ID.
  For filtered browsing, use 'story list' instead.

Examples:
  story search "authentication"
  story search "bug fix login"
  story search "database" --json

Related:
  story list      — Filter by state, priority, label, etc.
  story show <id> — Show a specific story by ID
  story log <id>  — What changed it, when, and what wrote it
"#,
        );

        m.insert(
            "decompose",
            r#"story decompose <file> [--dry-run]
story decompose --stdin [--dry-run]

Parse a markdown or YAML specification into individual stories with
relationships, priorities, and labels. Supports both file input and
stdin.

When to use:
  When you have a spec, PRD, or feature description that needs to be
  broken into trackable work items. Use --dry-run first to preview.

Examples:
  story decompose spec.md --dry-run     # Preview without creating
  story decompose spec.md               # Create stories from spec
  story decompose spec.yaml             # YAML format supported
  cat spec.md | story decompose --stdin # Read from stdin

Related:
  story new        — Create a single story manually
  story graph      — Visualize dependencies after decomposition
  story import     — Bulk import from JSON
"#,
        );

        m.insert(
            "graph",
            r#"story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]

Analyze the dependency graph of open stories. Shows relationships,
bottlenecks, and parallelizable work.

When to use:
  After creating stories with dependencies, to understand execution
  order, identify bottlenecks, or find parallelizable work.

Modes:
  (default)           Overview of all relationships
  --critical-path     Longest dependency chain (highest impact path)
  --blocked-by <id>   What is blocking this specific story
  --parallel-groups   Stories that can be worked on simultaneously

Examples:
  story graph                         # Full overview
  story graph --critical-path         # Identify the critical path
  story graph --blocked-by SH-5      # Why is SH-5 blocked?
  story graph --parallel-groups --json # Parallelizable work as JSON

Related:
  story list --blocked  — Simple list of blocked stories
  story list --ready    — Stories ready to work on now
  story next            — Highest-priority ready story
"#,
        );

        m.insert(
            "doctor",
            r#"story doctor [--fix]

Run integrity checks on the storyhook project data. Detects orphaned
relationships, invalid states, and data inconsistencies.

When to use:
  When stories seem inconsistent, or periodically as a health check.
  Use --fix to auto-repair. Also checks that each story's stored row
  still equals a fold of its own event history.

  --fix is also how a project created before the required states
  existed gets them: it adds any of 'todo', 'in-progress', 'blocked'
  and 'done' the project is missing, placing a new OPEN state at the
  end of the OPEN run so the state new stories land in does not move.
  It only ever adds. A project that already defines one of those slugs
  under the wrong superstate is reported rather than rewritten, because
  changing it would reclassify the stories sitting in it.

  A repair is an event, so it can only land on an open story. When the
  only story a repair could be written to is closed, --fix names that
  story instead of skipping it in silence: reopen it, run --fix again,
  then close it again. The story to reopen is often not the one the
  finding names — a missing inverse relation is reported against the
  end that has its half and repaired on the end that does not.

Examples:
  story doctor          # Check only, report issues
  story doctor --fix    # Attempt to fix issues found

Related:
  story list --flagged  — Stories with integrity warnings
  story summary         — Project state overview
"#,
        );

        m.insert(
            "report",
            r#"story report [--html]

Generate a project report. Default is text format; --html produces a
standalone HTML document.

When to use:
  When you need a shareable project status document. The HTML report
  is suitable for stakeholder updates.

Examples:
  story report              # Text report to stdout
  story report --html       # HTML report to stdout
  story report --html > status.html  # Save to file

Related:
  story summary       — Quick summary counts
  story load-context  — Agent-friendly context document
"#,
        );

        m.insert(
            "commit-sync",
            r#"story commit-sync [--since <duration>]

Scan recent git commits for story ID references and record them in
each story's "referenced by" field. A commit that CLAIMS a story also
moves it into the active state.

Mentioning a story links it. Claiming it moves it:

  Closes SH-1                  claims — a claim word, then the id
  Closes: SH-1                 claims — a 'Key: value' git trailer,
                               whose value is the whole rest of the line
  Closes SH-1, SH-2 and SH-3   claims all three
  fix: SH-1 broken parser      links only — here the colon is a
                               Conventional Commits type, not a
                               trailer key
  Refs SH-1 / see SH-1 / SH-1  links only

Claim words, in any tense and any case: close, fix, resolve, implement,
complete, start, wip. The word must sit immediately before the id on
the same line.

Two things cancel a claim: the words not, no, never, without, unless,
or an n't word, immediately before the claim word; and a Revert "..."
subject, which claims nothing on its first line.

Every run reports why a story that linked did not also move — no claim
word, sync.auto_transition off, no active state configured, or the
story already out of its default state — so a project can tell each
of those apart from 'broken'. To stop even a claim from moving a
story:

  story project settings set sync.auto_transition false

When to use:
  After git operations (commit, merge, pull) to link code changes to
  stories. Safe to run repeatedly — skips already-synced commits.

Examples:
  story commit-sync                  # Default: last 7 days
  story commit-sync --since 1h      # Last hour only
  story commit-sync --since 2w      # Last 2 weeks

Note: Previously named 'sync-git'. The old name still works as an alias.

Related:
  story hooks install    — Auto-sync via git hooks
  story github-sync      — Sync stories with GitHub Issues
  story handoff          — End-of-session summary
"#,
        );

        // Keep old name as alias
        m.insert("sync-git", m["commit-sync"]);

        m.insert(
            "github-sync",
            r#"story github-sync [<id>] [--dry-run] [--resolve local|remote]
                   [--strategy import-all|match-titles|push-only|future-only]
                   [--mode manual|off]

Sync stories with GitHub Issues bidirectionally. Pulls remote changes
and pushes local changes using three-way merge. Requires the
STORYHOOK_GITHUB_TOKEN environment variable to be set with a GitHub
Personal Access Token.

When to use:
  After making local story changes you want reflected on GitHub, or
  to pull in changes made on GitHub (comments, state changes, etc.).

Examples:
  story github-sync                  # Full project sync
  story github-sync SH-1            # Sync a single story
  story github-sync --dry-run       # Preview changes without applying
  story github-sync SH-1 --resolve remote   # Take GitHub's side of SH-1

First time on a project:
  A project that has never run github-sync is asked how to handle the
  initial sync, interactively -- or, non-interactively (a script, a
  pipe, --json), told to say so up front:

    story github-sync --strategy future-only --mode manual

  strategy:
    import-all      import every open issue as a local story
    match-titles    link stories to issues whose titles match exactly
    push-only       push local stories to GitHub, import nothing
    future-only     sync only changes from now on (the wizard's default)

  mode:
    manual          run 'story github-sync' explicitly (the wizard's default)
    off             disable sync for this project

  --strategy and --mode must be given together for this first sync, and
  --strategy never applies to a project that is already configured.

Changing the mode later:
  story github-sync --mode manual|off

  On a project that is already configured, --mode alone (no --strategy,
  no <id>) changes the stored mode instead of syncing -- the way to
  turn a disabled project back on, or to repair one still carrying a
  mode this build refuses to run under. Needs no GitHub token: it
  writes the stored configuration and nothing else.

Conflicts:
  When both sides changed one field to different values, storyhook
  applies everything else, prints all three values, and exits 8 without
  deciding. Nothing is chosen for you, and nothing is lost: the
  merge base holds the disputed field, so the same conflict is still
  there next time rather than GitHub quietly winning. Answer it with
  --resolve on that one story, or set the field to the same value on
  both sides and re-run.

  --resolve needs an explicit <id>. A whole-sync resolution would decide
  conflicts you have not read.

Configuration (per project, in the store):
  sync_mode = "manual"    # off | manual

  Note: "auto" is not offered, and a project still carrying it -- from
  before the rearchitecture, which deleted the code that ran it -- is
  refused rather than silently treated as manual, naming the repair:
  `story github-sync --mode manual` (or --mode off). Honest auto-sync
  means a GitHub call on the tail of every story-modifying command, in
  the daemon as well as locally — a feature with a failure policy and a
  timeout to design, not a switch to flip.

Related:
  story commit-sync  — Link git commits to stories
  story doctor       — Check project health including sync status
"#,
        );

        m.insert(
            "github-auth",
            r#"story github-auth login|status|logout

Manage the durable GitHub credential the daemon's background poll uses
to check linked pull requests unattended (SH-212). Separate from the
STORYHOOK_GITHUB_TOKEN environment variable `story github-sync` and
`story pr-check` read per invocation: this one is stored once, in your
OS keychain (macOS Keychain, Windows Credential Manager, or the Secret
Service on Linux), and spent by the daemon on a five-minute timer with
nobody typing a command.

login    Prompts for a GitHub Personal Access Token (always
         interactive — there is no non-interactive form) and stores it.
status   Reports whether a credential is stored, without printing it.
logout   Deletes the stored credential. The daemon stops using it on
         its next poll tick; no restart needed.

When to use:
  Once, to let close-on-merge links (`story link-pr`) resolve on their
  own instead of requiring a human or a scheduler to run
  `story pr-check`. Everyone else can keep running `story pr-check` by
  hand or from cron/CI, which needs no stored credential at all.

Examples:
  story github-auth login     # prompts for a token, stores it
  story github-auth status    # "a GitHub credential is stored..."
  story github-auth logout    # removes it

Requires the github-sync feature, like `story pr-check` itself.

Related:
  story pr-check      — Check linked pull requests by hand
  story link-pr        — Link a pull request to a story
  story github-sync    — The other GitHub credential, read per invocation
"#,
        );

        m.insert(
            "scaffold",
            r#"story scaffold agents-md|claude-md|cursor-rules

Generate agent instruction files for different AI coding tools. These
files teach agents how to use storyhook in the project.

When to use:
  After 'story project new' to set up AI agent integration. Run the scaffold
  matching your agent tool.

Variants:
  agents-md      Universal AGENTS.md (works with most AI agents)
  claude-md      Claude Code-specific instructions
  cursor-rules   Cursor editor rules

Examples:
  story scaffold agents-md        # Generate AGENTS.md
  story scaffold claude-md        # Generate Claude Code snippet
  story scaffold cursor-rules     # Generate .cursorrules content

Related:
  story project new  — Create a project (writes .storyhook.toml)
  story load-context — Project state for session start
"#,
        );

        m.insert(
            "hooks",
            r#"story hooks install|uninstall|list|test <event_type>

Manage storyhook event hooks. Event hooks fire custom commands when
story events occur (create, state change, close, etc.).

When to use:
  'install' / 'uninstall': Set up or remove git hooks for auto-sync.
  'list': See configured event hooks.
  'test': Fire a test event to verify hook configuration.

Examples:
  story hooks install             # Install git hooks (post-commit, etc.)
  story hooks uninstall           # Remove git hooks
  story hooks list                # Show event hook configuration
  story hooks test on_create      # Test the on_create hook

Backgrounding work in a hook:
  A hook may start work that outlives it, and storyhook will not kill it.
  Redirect its output when you do:

    my-slow-job >/dev/null 2>&1 &

  Without the redirect the job inherits the hook's stderr and keeps it for
  its whole life. storyhook can no longer be blocked by that — it hands a
  hook files rather than pipes — but a job that writes forever grows a file
  nobody reads. That file is unlinked, so it has no name and 'du' cannot
  see it: if a disk fills with no visible cause, 'lsof +L1' lists the
  unlinked-but-open files responsible.

A hook's environment is the daemon's, not yours:
  A hook runs with whatever environment the daemon happens to be holding —
  not the shell that triggered the event, and not necessarily a fresh one.
  The daemon outlives the client that started it, so a variable exported in
  one shell for one project can still be present hours or days later, when
  an unrelated project's hook fires. Do not assume the environment is
  current, and name every variable your hook actually depends on rather
  than relying on ambient state.

Related:
  story commit-sync  — Manual git sync (hooks automate this)
  [hooks] in .storyhook.toml — Event hook configuration
"#,
        );

        m.insert(
            "tui",
            r#"story tui

Launch an interactive terminal user interface for managing stories. The
TUI provides a dashboard, a Kanban-style board view, story detail editing,
filtering, and story creation — all without leaving the terminal.

When to use:
  When you want a visual overview of project state, or prefer to browse,
  edit, and move stories interactively rather than via individual CLI
  commands.

Views:
  Dashboard (1)   Project summary with metrics and recent activity
  Board (2)       Stories grouped by state as a scrollable table

Key bindings (board view):
  j/k, Up/Down    Navigate between rows
  h/l             Jump to previous/next section header
  Enter           Open story detail modal
  > / <           Move story to next/previous state
  n               Create a new story
  /               Focus the filter bar
  Space           Toggle section collapsed/expanded
  s               Configure the project's statuses
  ?               Show full keybinding reference
  q               Quit

Statuses editor (s):
  Configures the project's states — the board's columns — without
  leaving the TUI. j/k selects, J/K reorders, o toggles open/closed,
  a toggles the active role, e edits the description, n adds, d
  deletes. Reclassifying or deleting a status that still holds stories
  asks where those stories should go first. Status edits are not
  undoable. Same operations as 'story state' on the CLI.

Filtering:
  Press / to focus the filter bar, then type a filter query:
    state:todo        Filter by state
    priority:high     Filter by priority
    assignee:mikey    Filter by assignee
    label:bug         Filter by label
    blocked           Only blocked stories
    ready             Only unblocked stories
    <free text>       Substring match on title or ID

Mouse support:
  Click on a story row to select it. Double-click to open the detail
  modal. Scroll wheel navigates the board. Click outside a modal to
  close it.

Examples:
  story tui                     # Launch the TUI in the current project

Related:
  story list         — CLI-based story listing with filters
  story summary      — Project summary (similar to TUI dashboard)
  story load-context — Full project context for AI agents
"#,
        );

        m.insert(
            "import",
            r#"story import [<file>]

Import stories from a JSON file or stdin. Expects an array of story
objects with at minimum a "title" field.

When to use:
  For bulk story creation from structured data. For markdown/YAML
  specs, use 'story decompose' instead.

Examples:
  story import stories.json           # From file
  cat stories.json | story import     # From stdin

Related:
  story export     — Export all stories to JSON
  story decompose  — Create stories from markdown/YAML spec
  story new        — Create a single story
"#,
        );

        m.insert(
            "export",
            r#"story export

Export all open stories as a JSON array. Useful for backup, migration,
or external processing.

When to use:
  For data backup, migration to another system, or feeding into
  external tools.

Examples:
  story export                        # JSON to stdout
  story export > backup.json          # Save to file

Related:
  story import  — Import stories from JSON
  story report  — Human-readable report
"#,
        );

        m.insert(
            "json-format",
            r#"JSON Output Format Reference

All storyhook CLI commands support structured JSON output via the global
--json flag. This document describes the envelope format, per-command
response shapes, error format, and exit codes.

== Global --json flag ==

Pass --json as a global option (before or after the subcommand) to get
machine-readable JSON instead of human-readable text:

  story list --json
  story show SH-1 --json
  story summary --json

The flag applies to every command. When --json is active, all output
(success and error) is valid JSON printed to stdout.

== JSON Envelope ==

Every successful response is wrapped in a standard envelope:

  {
    "result": "ok",
    <data fields depending on command>,
    "warnings": ["..."],          // omitted when empty
    "flagged_reasons": ["..."]    // omitted when empty
  }

The "result" field is always "ok" for success. Data fields vary by
command (see below). The "warnings" and "flagged_reasons" arrays are
present only when non-empty.

== Per-Command Response Shapes ==

Commands returning a single story ("story" field):
  story new <title>                -> "story": StoryView
  story show <id>                  -> "story": StoryView
  story comment <id> "<text>"      -> "story": StoryView
  story assign <id> <member>       -> "story": StoryView
  story move <id> <state>          -> "story": StoryView
  story block <id> "<reason>"      -> "story": StoryView
  story unblock <id>               -> "story": StoryView
  story prioritize <id> <level>    -> "story": StoryView
  story label <id> <csv>           -> "story": StoryView
  story reopen <id>                -> "story": StoryView
  story relate <a> <rel> <b>       -> "story": StoryView (of story a)
  story set <id> --field value     -> "story": StoryView
  story next                       -> "story": StoryView (single result)

  StoryView object:
    {
      "story": {
        "id": "SH-1",
        "title": "Implement auth",
        "state": "todo",
        "superstate": "open",
        "priority": "high",
        "assignee": "alice",
        "labels": ["backend"],
        "awaiting": null,
        "relationships": [
          {"relation": "blocked-by", "other_id": "SH-2"}
        ],
        "comments": [
          {"at": "2025-01-15T10:00:00Z", "text": "Started work"}
        ],
        "created_at": "2025-01-15T09:00:00Z",
        "updated_at": "2025-01-15T10:00:00Z",
        "closed_at": null
      },
      "derived_relationships": [],
      "referenced_by": {
        "commits": [
          {"at": "2025-01-15T10:30:00Z", "sha": "abc123...", "subject": "feat: closes SH-1"}
        ],
        "prs": [],
        "comment_mentions": [
          {"at": "2025-01-16T09:00:00Z", "other_id": "SH-7", "snippet": "superseded by SH-1"}
        ]
      },
      "warnings": [],
      "flagged_reasons": [],
      "stale_info": null
    }

  "referenced_by" is omitted entirely when all three lists are empty, and
  each list is omitted when it alone is. "commits" comes from `commit-sync`
  scanning git history; "prs" from `story link-pr`; "comment_mentions" from
  scanning every *other* story's comments for this story's id (SH-220) —
  "other_id" is the story that did the mentioning, and "snippet" is the
  matched line of its comment, capped at 120 bytes. None of the three ever
  appears in "comments" (SH-169).

  "prs" and "comment_mentions" are cross-story work, so they arrive on
  `story show` and are absent from `story list`, `story next` and `story
  search` — the same gate "derived_relationships" and "progress" are behind.

  A story removed via `story delete` also carries "deleted": true and
  "deleted_reason": "<reason>" (omitted entirely for non-deleted stories).
  Its "superstate" is always "CLOSED", regardless of "state".

Commands returning a story list ("stories" field):
  story list [filters]        -> "stories": [StoryView, ...]
  story search <query>        -> "stories": [StoryView, ...]
  story next --count <n>      -> "stories": [StoryView, ...]
  story import [file]          -> "stories": [StoryView, ...]
  story decompose <file>       -> "stories": [StoryView, ...]

Commands returning a summary ("summary" field):
  story summary               -> "summary": SummaryView
  story report                -> "summary": SummaryView

  SummaryView object:
    {
      "total_open": 5,
      "total_closed": 3,
      "by_state": [["todo", 3], ["in-progress", 2]],
      "by_priority": [["high", 2], ["medium", 1]],
      "blocked_count": 1,
      "flagged_count": 0,
      "ready_count": 4,
      "ready_stories": [StoryView, ...]
    }

Commands returning a graph ("graph" field):
  story graph                 -> "graph": GraphView
  story graph --critical-path -> "graph": GraphView
  story graph --blocked-by <id> -> "graph": GraphView
  story graph --parallel-groups -> "graph": GraphView

  GraphView object:
    {
      "critical_path": ["SH-1", "SH-3", "SH-5"],
      "blocked_chain": {"source": "SH-2", "blocked": ["SH-4"]},
      "parallel_groups": [["SH-1", "SH-2"], ["SH-3"]],
      "overview": {
        "total_open": 5,
        "total_edges": 3,
        "roots": ["SH-1"],
        "leaves": ["SH-5"]
      }
    }

  Fields are present only for the requested mode. For example,
  --critical-path only populates "critical_path"; other fields are null.

Commands returning issues ("issues" field):
  story doctor                -> "issues": ["issue description", ...]

Commands returning a message ("message" field):
  story project new           -> "message": "created story project..."
  story member add            -> "message": "added member alice"
  story state add/remove      -> "message": "added state in-progress (open)"
  story export                -> "message": "<json array of stories>"
  story context               -> "message": "<markdown or json string>"
  story handoff               -> "message": "<markdown string>"
  story report --html         -> "message": "<html string>"
  story scaffold              -> "message": "<template content>"
  story hooks install/...     -> "message": "<status text>"
  story commit-sync            -> "message": "scanned N commits..."
  story next (no results)     -> "message": "no ready stories"
  story help <topic>          -> "message": "<help text>"

== Error Format ==

Errors produce:

  {
    "result": "error",
    "error": "story `SH-99` not found",
    "exit_code": 3
  }

== Exit Codes ==

  0  Success
  2  Usage error or validation error (bad arguments, invalid input)
  3  Not found (story ID does not exist)
  4  Lock timeout (another process holds the project lock)
  5  Integrity or storage error (corrupt data, I/O failure)

== Examples ==

Show a story:

  $ story show SH-1 --json
  {
    "result": "ok",
    "story": {
      "story": {
        "id": "SH-1",
        "title": "Add login page",
        "state": "todo",
        "superstate": "open",
        "priority": "high",
        "assignee": null,
        "labels": [],
        "awaiting": null,
        "relationships": [],
        "comments": [],
        "created_at": "2025-01-15T09:00:00Z",
        "updated_at": "2025-01-15T09:00:00Z",
        "closed_at": null
      },
      "derived_relationships": [],
      "warnings": [],
      "flagged_reasons": []
    }
  }

List stories:

  $ story list --ready --json
  {
    "result": "ok",
    "stories": [
      {
        "story": {
          "id": "SH-1",
          "title": "Add login page",
          "state": "todo",
          "superstate": "open",
          "priority": "high",
          "assignee": null,
          "labels": [],
          ...
        },
        ...
      }
    ]
  }

Error:

  $ story show SH-999 --json
  {
    "result": "error",
    "error": "story `SH-999` not found",
    "exit_code": 3
  }
"#,
        );

        m.insert(
            "show",
            r#"story show <id>

Show full details for a single story by its ID, including state,
priority, labels, comments, relationships, and timestamps.

When to use:
  When you need the complete context for a specific story. For a
  filtered listing, use 'story list' instead.

Examples:
  story show SH-1              # Full story details
  story show SH-1 --json       # Structured JSON output

Related:
  story list      — Browse stories with filters
  story search    — Find stories by content
  story log       — What changed this story, and what did it
"#,
        );

        m.insert(
            "log",
            r#"story log <id>

Every event in a story's history, oldest first, with what wrote each one.
Answers "what moved this story, and when" — from the store, in one lookup.

The last column, which is the point of the command:

  set-state                        A bare word is the command STORYHOOK
                                   DERIVED from the request it dispatched.
                                   A caller cannot misstate it, because a
                                   caller is never asked for it.

  set-state (story.sh:dispatch)    Parentheses mean SELF-ATTESTED: the
                                   caller declared this about itself in
                                   $STORYHOOK_ACTOR.

  (unrecorded)                     Nothing was captured. Events written
                                   before storyhook recorded provenance,
                                   and events replayed by 'story migrate'
                                   or 'story import-project', which copied
                                   a history rather than performed it.

The command is the internal request name, which is not always the verb you
typed: 'story move' dispatches 'set-state', 'story sync-git' dispatches
'commit-sync'. The verb you type is parsed by the client and never reaches
the daemon, so recording it would make the attested half caller-supplied.

Declaring an actor:
  Export $STORYHOOK_ACTOR before running a command, and its value is
  recorded against every event that command writes. Scripts should use it
  to say which of their code paths ran — that is the difference between
  "something moved this story" and "dispatch rolled its claim back".

    STORYHOOK_ACTOR=story.sh:dispatch-rollback story move SH-1 todo

  A label is at most 128 bytes and may not contain control characters.
  A bad one is REFUSED rather than cleaned up: the value is rendered into
  a terminal and kept in a trail people reason from, so a newline or an
  escape sequence could rewrite what a reader sees.

This is a diagnostic aid, NOT a tamper-proof audit log. The declared half
is self-attested, and anyone who can set $STORYHOOK_ACTOR can already write
to the store directly. It answers "what wrote this" for a cooperating
caller, which is the question that is otherwise unanswerable.

Examples:
  story log SH-1               # The story's whole history
  story log SH-1 --json        # command and actor as separate fields

Related:
  story show       — The story's current state, not how it got there
  story comment    — Add a note to a story's history
"#,
        );

        m.insert(
            "move",
            r#"story move <id> <state> [--if-state <expected>] [--reason <text>] ["<comment>"]

Transition a story to a new state. Transitioning to a CLOSED state
automatically archives the story. Optionally add a comment in the
same operation.

--if-state <expected> guards the transition with a compare-and-swap:
the move only applies if the story's current state still matches
<expected>. Otherwise it fails with a machine-readable conflict
instead of overwriting a state you didn't know had changed —
useful for automated callers claiming stories concurrently.

--reason <text> sets the story's `awaiting` reason atomically with
the state change — the common case is `story move <id> blocked
--reason "..."` so a card in the Blocked column carries an
explanation from the moment it lands there. Strictly opt-in: omit
it and the move behaves exactly as before, so scripts, agents and
CI that never pass it see no behavior change. Refused if <state> is
a CLOSED state, since closing already clears `awaiting`. To set a
reason without moving state at all, use `story block <id> "<text>"`.

When --if-state and/or --reason are used, they must come immediately
after <state>, in either order; everything past them is treated as
free-text comment, exactly like today, with no restrictions on its
content.

When to use:
  To update the status of a story as you work on it, or to close
  it when complete.

Examples:
  story move SH-1 in-progress                          # Start working on it
  story move SH-1 done                                 # Mark as done
  story move SH-1 done "shipped v2.1"                  # Done with comment
  story move SH-1 in-progress --if-state todo          # Claim only if still todo
  story move SH-1 blocked --reason "waiting on SH-9"   # Block with a reason

Related:
  story reopen <id>  — Reopen a closed story
  story set <id>     — Update multiple fields at once
  story next         — Pick the next ready story
"#,
        );

        // Redirect old "is" name to "move"
        m.insert("is", m["move"]);

        m.insert(
            "block",
            r#"story block <id> "<reason>"

Mark a story as blocked with a reason. Blocked stories are excluded
from 'story next' results and highlighted in listings.

When to use:
  When external dependencies, decisions, or other factors prevent
  progress on a story.

Examples:
  story block SH-3 "waiting for API access from vendor"
  story block SH-7 "needs design review"

Related:
  story unblock <id>    — Clear the blocked status
  story list --blocked  — List all blocked stories
  story list --ready    — List all unblocked stories
"#,
        );

        // Redirect old "awaits" name to "block"
        m.insert("awaits", m["block"]);

        m.insert(
            "unblock",
            r#"story unblock <id>

Clear the blocked/awaiting status on a story, making it eligible
for 'story next' again.

When to use:
  When the blocking condition has been resolved and the story can
  proceed.

Examples:
  story unblock SH-3
  story unblock SH-7

Related:
  story block <id>     — Mark a story as blocked
  story list --ready   — List stories ready to work on
  story next           — Pick the next ready story
"#,
        );

        m.insert(
            "set",
            r#"story set <id> [--title "text"] [--state <slug>] [--priority <level>]
              [--assignee <member>] [--labels <csv>] [--blocked "<reason>"]
              [--unblocked] [--json '{"key": "value"}'] [--type <slug>]
              [--description "text"]

Update multiple fields on a story in a single command. Accepts any
combination of field flags. Use --json for arbitrary key-value data.
--labels adds to the story's existing labels; comma is always the
delimiter, so a label can never contain one.

When to use:
  When you need to update more than one field at a time, or when
  using the --json flag for structured metadata. For single-field
  updates, the dedicated verb commands (move, prioritize, assign,
  label, block, unblock) are more concise.

Examples:
  story set SH-1 --priority high --state in-progress
  story set SH-1 --assignee alice --labels "backend,urgent"
  story set SH-1 --json '{"estimate": "3d", "epic": "auth"}'
  story set SH-1 --blocked "waiting for deploy"
  story set SH-1 --unblocked
  story set SH-1 --description "Root cause: race condition in cache invalidation"

Related:
  story move <id>        — Change state only
  story prioritize <id>  — Set priority only
  story assign <id>      — Set assignee only
  story label <id>       — Add labels only
  story block <id>       — Set blocked status only
"#,
        );

        m.insert(
            "comment",
            r#"story comment <id> "<text>"

Add a timestamped comment to a story. Comments are append-only and
form part of the audit trail.

When to use:
  To record progress notes, decisions, blockers, or context that
  should be preserved in the story's event log.

Examples:
  story comment SH-1 "Started implementing the auth middleware"
  story comment SH-3 "Decided to use JWT instead of sessions"

Related:
  story show <id>  — View a story including its comments
  story move <id>  — Move state with an optional comment
"#,
        );

        m.insert(
            "assign",
            r#"story assign <id> <member>

Assign a story to a team member by their member ID or GitHub handle.

When to use:
  To indicate who is responsible for a story.

Examples:
  story assign SH-1 alice
  story assign SH-3 mikey

Related:
  story list --assignee <id>  — Filter stories by assignee
  story member add            — Add a team member
"#,
        );

        m.insert(
            "prioritize",
            r#"story prioritize <id> <level>

Set the priority of a story. Priority levels: critical, high,
medium, low, none.

When to use:
  After creating a story, or when reprioritizing work. Priority
  affects the ordering of 'story next' results.

Examples:
  story prioritize SH-1 critical
  story prioritize SH-5 high
  story prioritize SH-8 low

Related:
  story next                   — Get highest-priority ready story
  story list --priority high   — Filter by priority
"#,
        );

        // Redirect old "priority" name
        m.insert("priority", m["prioritize"]);

        m.insert(
            "label",
            r#"story label <id> <labels>

Add one or more comma-separated labels to a story. Comma is always
the delimiter — a single label can never contain one.

When to use:
  To categorize stories for filtering and organization.

Examples:
  story label SH-1 backend
  story label SH-3 bug,urgent

Related:
  story unlabel <id>       — Remove labels
  story list --label <csv> — Filter stories by label
"#,
        );

        m.insert(
            "unlabel",
            r#"story unlabel <id> <labels>

Remove one or more comma-separated labels from a story. Comma is
always the delimiter — a single label can never contain one.

When to use:
  To remove labels that no longer apply to a story.

Examples:
  story unlabel SH-1 wontfix
  story unlabel SH-3 bug,urgent

Related:
  story label <id>         — Add labels
  story list --label <csv> — Filter stories by label
"#,
        );

        m.insert(
            "relate",
            r#"story relate <a> <relation> <b>

Add a relationship between two stories. Relationship types:
  blocks / blocked-by    — Task dependencies (A blocks B)
  parent-of / child-of   — Hierarchy
  relates-to             — General link
  duplicate-of           — Mark as duplicate
  obviates / obviated-by — One story makes another unnecessary

When to use:
  To define dependencies, hierarchy, or other links between stories.
  Use blocks/blocked-by to control execution order. 'story next'
  respects blocking relationships.

Examples:
  story relate SH-1 blocks SH-2
  story relate SH-3 parent-of SH-4
  story relate SH-5 relates-to SH-6

NOT TO BE CONFUSED WITH
  story project link / unlink   attach a git ORIGIN or CHECKOUT to a
  project. Same word, different subject: these relate stories, those
  relate a project to a repository. See 'story help project'.

Related:
  story unrelate <a> <rel> <b>  — Remove a relationship
  story graph                   — Visualize the dependency graph
  story graph --blocked-by <id> — Trace why a story is blocked
  story help project            — story project link, which is about git
"#,
        );

        // "link" is an alias for "relate"
        m.insert("link", m["relate"]);

        m.insert(
            "unrelate",
            r#"story unrelate <a> <relation> <b>

Remove a relationship between two stories.

When to use:
  When a previously defined relationship no longer applies.

Examples:
  story unrelate SH-1 blocks SH-2
  story unrelate SH-3 parent-of SH-4

Related:
  story relate <a> <rel> <b>  — Add a relationship
  story graph                 — Visualize the dependency graph
"#,
        );

        m.insert(
            "reopen",
            r#"story reopen <id> [--force]

Reopen a closed/archived story, returning it to an open state.

Reopening a story that was soft-deleted (`story delete`) undeletes it: at
an interactive terminal you'll be prompted to confirm; in scripts/CI (no
TTY) or to skip the prompt, pass --force. Reopening an ordinarily-closed
story needs no confirmation.

When to use:
  When a completed story needs more work, was closed by mistake, or was
  deleted in error.

Examples:
  story reopen SH-5
  story reopen SH-12
  story reopen SH-7 --force

Related:
  story move <id> <state>  — Transition to a specific state
  story show <id>          — View story details
  story archive <id>       — Hide a closed story from the primary UI
"#,
        );

        m.insert(
            "archive",
            r#"story archive <id>

Hide a closed story from the primary UI (the dashboard board/list, and
the default view here and in the TUI). Refuses an open story — only an
already-closed one can be archived. Fully reversible: `story unarchive`
undoes it, and reopening an archived story (`story reopen`/`story move`
into an open state) clears the archived flag too, since an open story
cannot read as archived.

Archiving is a display preference layered on top of "closed", not a
second kind of closing: a story's state and superstate are unchanged.

When to use:
  To declutter a Done-like column of stories you no longer need to see
  day to day, without deleting them.

Examples:
  story archive SH-12
  story unarchive SH-12
  story archive-state done --force

Related:
  story unarchive <id>                     — Reverse an archive
  story archive-state <state> [--force]    — Archive an entire column at once
  story reopen <id> [--force]              — Reopen (also un-archives)
"#,
        );

        m.insert(
            "unarchive",
            r#"story unarchive <id>

Reverses `story archive`: the story reappears in the primary UI. The
story's state and superstate are unchanged — this does not reopen it.

When to use:
  To bring back a story you archived by mistake, or want to see again.

Examples:
  story unarchive SH-12

Related:
  story archive <id>  — Hide a closed story
"#,
        );

        m.insert(
            "publish",
            r#"story publish <id>

Makes a draft story live. One-way: there is no command to turn a
live story back into a draft. Idempotent on a story that is already
live — publishing again is not an error, it just has nothing to do.

Once published, the story loses its [draft] badge in 'story list',
becomes eligible for 'story next'/'--ready', and (if it's still in
its original state) starts appearing on the web dashboard board.

When to use:
  When a draft is ready to become real, actionable work.

Examples:
  story publish SH-42

Related:
  story new --draft   — Create a story as a draft
  story list --drafts — See every draft in the project
"#,
        );

        m.insert(
            "archive-state",
            r#"story archive-state <state-slug> [--force]

Archives every not-yet-archived story currently in a closed-superstate
column, in one call — the bulk equivalent of running `story archive` on
each. Refuses a state that is open, or one that is not defined.

Two-step like `story reopen`/`story purge`: without --force, this
answers with exactly which stories would be archived and writes
nothing; at an interactive terminal you'll be prompted to confirm, and
--force skips the prompt (for scripts/CI, or once you've reviewed the
preview).

When to use:
  To clear out an entire Done-like column at once, e.g. at the end of a
  sprint.

Examples:
  story archive-state done
  story archive-state done --force

Related:
  story archive <id>    — Archive one story
  story unarchive <id>  — Reverse an archive
"#,
        );

        m.insert(
            "delete",
            r#"story delete <id> "<reason>"

Soft-delete a story with a required reason. The story is archived with
a deletion flag — never truly lost — and its superstate becomes CLOSED,
so it no longer counts as open, ready, or a blocker for other stories.
Like any closed story it still appears in `story list` (marked deleted)
and can be found via search; `--json`/`show` expose "deleted": true and
"deleted_reason": "<reason>".

When to use:
  For duplicate, erroneous, or abandoned stories.

Examples:
  story delete SH-3 "duplicate of SH-1"
  story delete SH-7 "created in error"

Related:
  story reopen <id> [--force]  — Undelete (reopen a deleted story)
  story purge <id> [--force]   — Remove a deleted story permanently
  story search                 — Find deleted stories
"#,
        );

        m.insert(
            "purge",
            r#"story purge <id> [--force]

Remove a soft-deleted story permanently: its events, its comments, its
labels, its git links, and every trace of it in the store. There is no
undo. This is the only irreversible thing that can be done to a single
story.

It refuses a story that has not been soft-deleted first. `story delete`
is the reversible tombstone and the required step before this one, so
everything a purge destroys was already marked unwanted, by someone,
with a reason on the record.

At an interactive terminal you'll be asked to type the story's id to
confirm. In scripts/CI (no TTY) and under --json there is nobody to ask,
so the command refuses and names --force.

Two consequences worth knowing before you run it:

  * Any surviving story that still claims a relationship with this one
    has that claim retracted first, as a real event on that story. The
    confirmation lists them.
  * The story id is never reused. `story new` carries on from the next
    number, so a purged id stays a gap forever rather than pointing at
    something unrelated later.

When to use:
  For a story created in error that should never have existed — a
  mis-parsed import, a duplicate minted twice, a test story in a real
  project. Not for finished work, and not for work that was abandoned:
  both of those are what `story delete` is for.

Examples:
  story delete SH-7 "created in error"
  story purge SH-7

Related:
  story delete <id> "<reason>"  — Soft-delete (reversible)
  story reopen <id> [--force]   — Undelete a soft-deleted story
  story project delete          — Delete a whole project
"#,
        );

        m.insert(
            "web",
            r#"story web start [--port <PORT>]
story web stop
story web status
story web open
story web address

Launch a single web dashboard that serves every storyhook project
the store knows: a home screen with a summary card per
repo, a repo-select dropdown for fast switching, and — per repo — a
kanban board with drag-and-drop, a filterable/sortable list view, and
a detail drawer for full editing. Everything is live-updating and
backed by the same validated write path the CLI uses.

The server always binds 127.0.0.1, and also binds your Tailscale IP
if the 'tailscale' CLI reports one — reachable from localhost and
your tailnet only, never the public internet or a plain LAN address.
Default port is 3456. Data refreshes every 3 seconds via polling.

Commands:
  start        Start the dashboard as a background daemon (does not
               require running from inside a project — it serves every
               project in the store, not the one you are standing in).
               --port <PORT>  Use a custom port (default: 3456).
  stop         Stop the running dashboard daemon.
  status       Check if the dashboard is running.
  open         Open the running dashboard in your default browser
               (loopback URL — always reachable on this machine).
  address      Copy the running dashboard's URL to the clipboard. Uses
               the tailnet URL when Tailscale is up (so it works from
               your other devices), else loopback. Both open/address
               fail with this summary if the dashboard isn't running.

There is no separate registration step. A project reaches the dashboard
by existing: 'story project new' puts it in the store, and the store is
what the dashboard reads. 'story project list' prints the same set.

When to use:
  When you want a browser-based view across some or all of your
  storyhook projects that updates live as stories change, switch
  quickly between projects without juggling ports, or triage/edit
  stories visually — drag cards between states, edit fields, comment,
  block/unblock, link relationships — without leaving the browser.
  Useful during sprint planning, standups, or while working across
  multiple projects in parallel.

Examples:
  story web start                      # Start on default port 3456
  story web start --port 8080          # Start on custom port
  story web stop                       # Stop the dashboard
  story web status                     # Check if running
  story web open                       # Open the dashboard in your browser
  story web address                    # Copy the dashboard URL to the clipboard

Screens:
  Home      One summary card per project (open/ready/blocked
            counts). Click a card to open that repo. A repo whose
            data can't currently be loaded (moved, deleted) shows its
            error instead of a summary.
  Settings  Register a new repo, or deregister an existing one.
  Board     One column per project state (states.toml order). Drag a
            card to a different column to move it; dropping onto a
            CLOSED state archives the story in place.
  List      A filterable, sortable table.
  Drawer    Click any card or row for full detail: title, state,
            priority, assignee, type, labels, block/unblock, comments,
            relationships, reopen, and delete.

Security:
  Every request requires the daemon's bearer token ('story daemon
  token' prints it, and copies it to your clipboard when you're at a
  terminal — over SSH or Mosh too, via OSC 52), with one exception: a
  read arriving on 127.0.0.1 is answered without one, so opening the
  dashboard locally just works. Writes always need it, and everything
  over your tailnet IP needs it, reads included. That exemption is
  narrow: GET or HEAD only, a loopback Host, no X-Forwarded-* /
  Forwarded / X-Real-IP header, not the dispatch endpoint, and no
  reverse-proxy allowlist configured — any one of those failing means
  the token is required as before. The dashboard's own page asks the
  first time a request needs one and holds it in sessionStorage (gone
  when the tab closes; re-entering it after a daemon restart is
  expected — the token rotates then). Mutating requests (create/move/edit/delete a story,
  and anything the Settings screen changes) additionally require a
  same-origin request (a custom header a cross-site request can't
  replicate) and a Host header resolving to 127.0.0.1/localhost/::1,
  the tailnet IP this instance bound itself, or — when Tailscale
  MagicDNS is on — this machine's full MagicDNS name (e.g.
  host.tailXXXXX.ts.net); this stops DNS-rebinding, which the header
  check alone can't catch. The bare short hostname (just 'host',
  without the .ts.net suffix) is deliberately not trusted: unlike the
  full name, it can resolve through a DNS search domain that isn't
  your tailnet's, so trusting it could reopen the rebinding this
  check exists to stop. GET / is the one route reachable with no
  token, so it can serve the page that prompts for one; GET
  /api/events (the live-update stream) also accepts the token as a
  ?token= query parameter, since a browser's EventSource can't set
  headers. Set STORYHOOK_WEB_TRUSTED_HOSTS to a comma-separated
  allowlist before putting any reverse proxy in front of the daemon:
  it widens the Host allowlist so writes work under the proxy's
  hostname, and it switches off the loopback read exemption, because
  a reverse proxy connects over loopback — so 'arrived on 127.0.0.1'
  stops meaning 'came from this machine' once one exists. It does not
  change what the server binds. The daemon states which posture it is
  in on startup.

How it works:
  The repo list is the store's own projects table — the same rows the
  CLI reads — so a project you have used is a project the dashboard
  shows, with nothing to keep in step. 'story web start' spawns a single background
  process (not one per repo) that
  binds 127.0.0.1 and, if available, your Tailscale IP (never
  0.0.0.0, never a plain LAN address — best-effort: a failed tailnet
  bind just falls back to localhost-only, logged as a warning). Its
  PID file, lock, and log live under $XDG_STATE_HOME/storyhook
  (~/.local/state/storyhook by default). It
  polls GET /api/repos every 3 seconds for the repo list, and — for
  whichever repo is selected — GET /api/repos/<id>/data for that
  repo's stories, calling POST/PATCH/DELETE /api/repos/<id>/story/...
  for mutations.

Related:
  story report --html  — Generate a static HTML report (one-time snapshot)
  story summary        — Quick text summary in the terminal
  story tui             — Interactive terminal UI
"#,
        );

        m.insert(
            "session-start",
            r#"story session-start

Output a JSON object carrying a compact CLI reference and current project
state as SessionStart hook context. Designed for use by editor plugins and
shell hooks at session start.

Output format:
  {"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}
                              — when project exists and plugin is enabled
  {}                          — when no project, or plugin is disabled

The context is delivered via `additionalContext`, which Claude Code injects
silently into the model's context (it is NOT shown to the user as a visible
block, unlike `systemMessage`). It includes the compact CLI reference (same
as story help --compact) plus a project state summary: open story count,
ready story count, and the next recommended story if one exists.

The output is always valid JSON, uses serde_json for safety with
special characters, and the context is kept under 4000 chars.

When to use:
  Automatically called by editor plugin hooks at session start. Not
  normally invoked by hand.

Examples:
  story session-start         # Output JSON for plugin consumption

Related:
  story help --compact  — Just the CLI reference portion
  story load-context    — Full context document for interactive use
  story next            — Get the highest-priority ready story
"#,
        );

        m.insert(
            "update",
            r#"story update [--check] [--force]

Update the story binary in place to the latest GitHub release. Downloads the
release asset for your platform, verifies it runs, and atomically replaces the
running executable.

When to use:
  Periodically, to pick up new releases. Run 'story update --check' first to
  see whether a newer version is available without changing anything.

Flags:
  --check    Report whether an update is available; do not download or install.
  --force    Reinstall the latest release even if already up to date.

Examples:
  story update            # Update to the latest release if newer
  story update --check    # Just report whether an update is available
  story --version         # Print the currently installed version

Notes:
  - Requires the 'github-sync' build feature (enabled by default).
  - Installs into the directory of the current binary; if that directory is
    not writable (e.g. /usr/local/bin), re-run with elevated privileges or use
    the installer at https://github.com/mikeydotio/storyhook.
  - Set STORYHOOK_GITHUB_TOKEN to raise the GitHub API rate limit (optional).

Related:
  story doctor  — Check project integrity
"#,
        );

        m.insert(
            "migrate",
            r#"story migrate [<path>] [--dry-run]

Move an existing .storyhook/ project into storyhook's store. Reads the legacy
tree — states, types, members, every story's event history, the archive, and
the story-number counter — and writes it into the store as one project. The
.storyhook/ directory is never modified: it is your rollback, and it should
stay in the repository until you are satisfied with the result.

When to use:
  Once per repository. Run it with --dry-run first.

Flags:
  --dry-run    Report exactly what would be imported, and write nothing.

Examples:
  story migrate --dry-run     # See the plan, including any repairs
  story migrate               # Do it
  story migrate ../other-repo # Migrate a project you are not standing in

What it repairs, and what it refuses:
  A relation only one story's history claimed is completed — the missing half
  is written as an event stamped with the original instant — and every repair
  is listed. Anything that cannot be represented without guessing is refused
  with nothing imported: a story with two parents, a relation pointing at a
  story that is not there, a story that exists both open and archived, or a
  story sitting in a state that states.toml no longer defines. The message
  names each one and the command that resolves it.

Notes:
  - Refuses to run in a linked git worktree. A worktree's .storyhook/ is a
    diverged copy, and migrating it would create a second project with the
    same prefix.
  - Refuses to run twice against one checkout. There is nothing to merge into.
  - Event kinds written by a newer storyhook are carried over verbatim.

Related:
  story export          — A portable copy of the project, for backup
  story import-project  — Restore an export document
  story doctor          — Check project integrity before migrating
"#,
        );

        m.insert(
            "relink",
            r#"story relink is now story project link checkout

Point a project at the checkout it now lives in:

  story --project <SLUG> project link checkout <PATH>

`link checkout` is strictly more capable than `relink` was. `relink`
read a .storyhook.toml in the directory it was pointed at, and refused
if the uuid there named a different project — which meant it could not
point a project at a checkout that had never been initialized, or one
whose pointer file had been lost. `link checkout` asks the directory
for nothing to identify itself — it records the path against the
project you name either way, and if the directory has no pointer file
of its own, it writes one, so a bare story id resolves there from then
on. A directory that already names a *different* project keeps its own
pointer untouched; a checkout link is never a claim on identity.

Related:
  story help project   — The whole project verb group
  story project list   — Every project, and where its checkout is
  story doctor         — Report registrations pointing at nothing
"#,
        );

        m.insert(
            "storage",
            r#"Where storyhook keeps your data

Story data lives in one SQLite store, outside your repositories. A repository
carries a single committed file — .storyhook.toml — naming the project it
belongs to, and no story data at all.

== The store ==

  <data home>/store.db

The store file is named by the first of these that is set:

  --store-path <file>        names the store file itself
  $STORYHOOK_STORE_PATH      same, from the environment
  $STORYHOOK_DATA_DIR        names the directory the file sits in
  $XDG_DATA_HOME/storyhook
  ~/.local/share/storyhook   the default

--store-path and $STORYHOOK_STORE_PATH outrank $STORYHOOK_DATA_DIR: if a shell
has one of the first two exported, the third is silently ignored. One daemon
serves one store, keyed by this path, so naming a store here is what makes a
command unable to read or write any other.

story store new <path> creates an empty store beside the default one — the
supported way to give a test suite, or a second tracker, a store of its own.
It refuses to create the default path; that one is made by the daemon on its
first run, never by this verb.

story store backup [--label <text>] takes a verified, on-demand backup of the
ambient store — the safe way to protect it before a risky bulk operation,
replacing a hand-copied store.db (which can capture a hot write-ahead log
mid-write and look fine while being corrupt). It writes into a maintenance
directory the daily schedule never prunes, so the backup survives on its own
schedule rather than the daemon's. Runs immediately, with no confirmation
step: it only ever creates a file. --label distinguishes it from every other
backup sharing that directory (for example --label pre-migration); it
defaults to "manual" when omitted. story daemon status and story web status
report both the daily and the maintenance backups — story doctor does not,
since its output is pinned by the golden corpus and its exit code means a
project's integrity, not a machine's backup freshness.

story project new also refuses once too many projects appear in a real store
too fast: 5 or more inside ten minutes is refused, because that rate is the
signature of a test suite driving story without a store of its own, not a
person or a script creating projects on purpose. It names the same levers
above, plus STORYHOOK_ALLOW_PROJECT_BURST for the rare case where the volume
really was on purpose.

The store runs in write-ahead-logging mode, so it is three files in practice:
store.db, store.db-wal and store.db-shm. All three are part of the database.
That matters when restoring — see below.

== The pointer file ==

  .storyhook.toml

Written by story project new and by story migrate. Commit it. It names the project's
uuid and prefix, so a fresh clone — or a linked git worktree — resolves the
same project before it has anything local to consult. Resolution walks up from
the working directory, so commands work from a subdirectory too.

== Runtime files and snapshots ==

  <state home>/           daemon.json, daemon.pid, daemon.log
  <state home>/backups/   verified snapshots

The state home is $XDG_STATE_HOME/storyhook, else ~/.local/state/storyhook.

Snapshots are taken with SQLite's VACUUM INTO, opened and integrity-checked
after they are written, and rotated — seven are kept. Pruning happens when a
snapshot is taken rather than continuously, so a machine that migrates often
can hold more than seven for a while. A snapshot taken before a schema
migration carries the version it was taken from in its name, which is what
makes it useful after a bad upgrade. story daemon status reports how old the
newest one is.

== Restoring a snapshot ==

Copying a snapshot over store.db is NOT a restore. The old database's -wal and
-shm sidecars survive beside the new database's pages, and SQLite replays one
into the other — turning a recoverable machine into a malformed one.

The whole procedure:

  1. story daemon stop
  2. delete store.db, store.db-wal and store.db-shm from the data home
  3. copy the newest snapshot there as store.db
  4. story doctor

Step 1 is not optional. A running daemon holds the database open with its own
page cache, and will serve the old data happily while the files are swapped
underneath it.

== A repository that has not been migrated ==

A repository still carrying a .storyhook/ directory has not been migrated, and
storyhook will say so rather than guess. story migrate brings it across. It
never modifies the directory it reads: .storyhook/ stays exactly as it is, and
is your rollback until you choose to delete it.

== Backing up ==

  story export   a portable JSON document, one project at a time
  the snapshots  a binary copy of the whole store, every project

Related:
  story migrate — Move a legacy .storyhook/ project into the store
  story doctor  — Integrity checks, and the rebuild diff
  story project new  — Create a project and write .storyhook.toml
"#,
        );

        m
    });

/// LLM-optimized compact CLI reference. Hand-curated, 40-100 lines, <3000 chars.
/// No verbose examples or "When to use:" sections.
pub fn compact_reference() -> &'static str {
    r#"storyhook — CLI story tracker for AI-assisted development

LIFECYCLE
  story project new --prefix P  Create a project (asks if given no flags)
  story project show|list|delete Show this one; list all; delete one
  story new "<title>"             Create a story, returns assigned ID
  story show <id>                 Full details for a single story
  story move <id> <state>         Transition state (e.g., todo → in-progress → done)
  story reopen <id>               Reopen a closed story
  story delete <id> "<reason>"    Soft-delete with required reason
  story purge <id> --force        Permanently remove a deleted story (no undo)

QUERY & NAVIGATION
  story list [filters]            List open stories (--ready, --blocked, --state, --priority, etc.)
  story next [--count N]          Highest-priority ready story/stories
  story search "<query>"          Full-text search across all stories
  story summary                   Counts by state and priority
  story load-context              Session-start context document
  story graph [--critical-path]   Dependency graph analysis

STORY METADATA
  story comment <id> "<text>"     Add timestamped comment
  story assign <id> <member>      Assign to team member
  story prioritize <id> <level>   Set priority: critical|high|medium|low|none
  story label <id> <csv>          Add comma-separated labels
  story unlabel <id> <csv>        Remove labels
  story block <id> "<reason>"     Mark as blocked
  story unblock <id>              Clear blocked status
  story relate <a> <rel> <b>      Add relationship (blocks, parent-of, relates-to, etc.)
  story unrelate <a> <rel> <b>    Remove relationship
  story set <id> [--field val]    Update multiple fields at once

BULK & INTEGRATION
  story decompose <file>          Parse spec into stories with dependencies
  story import [file]             Bulk import from JSON
  story export                    Export all stories as JSON
  story commit-sync               Link git commits to stories
  story github-sync               Bidirectional GitHub Issues sync
  story handoff                   End-of-session summary document

PROJECT MANAGEMENT
  story phase list|show|add|remove  Manage story phases
  story doctor [--fix]            Integrity checks and repair
  story report [--html]           Generate project report
  story scaffold <variant>        Generate agent instruction files
  story hooks install|uninstall   Manage git hooks
  story tui                       Interactive terminal UI

GLOBAL FLAGS
  --json          Machine-readable JSON output (works with every command)
  --quiet         Suppress non-essential output
  --no-hooks      Skip event hooks for this invocation

WORKFLOW TIPS
  Start a session:   story load-context → story next → story move <id> in-progress
  End a session:     story commit-sync → story handoff
  Explore backlog:   story list --ready   or   story summary

Run 'story help <command>' for detail, or 'story help --all' for everything.
"#
}

/// Concatenate all help topics into a single document with clear headers.
pub fn all_topics_text() -> String {
    let mut out = String::from("# storyhook — Complete CLI Reference\n\n");
    // Use BTreeMap ordering (alphabetical) and skip aliases
    let aliases = ["awaits", "context", "is", "link", "priority", "sync-git"];
    for (name, content) in TOPICS.iter() {
        if aliases.contains(name) {
            continue;
        }
        out.push_str(&format!("## {}\n\n{}\n\n", name, content.trim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::get_help_topic;

    #[test]
    fn json_format_topic_exists_and_covers_key_concepts() {
        let content = get_help_topic("json-format").expect("json-format topic should exist");
        assert!(
            content.contains("\"result\""),
            "should document result field"
        );
        assert!(content.contains("--json"), "should document --json flag");
        assert!(content.contains("\"story\""), "should document story field");
        assert!(
            content.contains("\"stories\""),
            "should document stories field"
        );
        assert!(
            content.contains("\"summary\""),
            "should document summary field"
        );
        assert!(content.contains("\"graph\""), "should document graph field");
        assert!(
            content.contains("\"issues\""),
            "should document issues field"
        );
        assert!(
            content.contains("\"message\""),
            "should document message field"
        );
        assert!(content.contains("exit_code"), "should document exit codes");
    }

    #[test]
    fn json_format_topic_listed() {
        let topics = super::list_topics();
        assert!(
            topics.contains(&"json-format"),
            "json-format should appear in topic list"
        );
    }

    // ================================================================
    // compact_reference() contract tests
    // ================================================================

    #[test]
    fn compact_reference_under_3000_chars() {
        let text = super::compact_reference();
        assert!(
            text.len() < 3000,
            "compact_reference must stay under 3000 chars (documented contract), got {} chars",
            text.len()
        );
    }

    #[test]
    fn compact_reference_between_40_and_100_lines() {
        let text = super::compact_reference();
        let line_count = text.lines().count();
        assert!(
            line_count >= 40,
            "compact_reference should have at least 40 lines, got {line_count}"
        );
        assert!(
            line_count <= 100,
            "compact_reference should have at most 100 lines, got {line_count}"
        );
    }

    #[test]
    fn compact_reference_contains_all_section_headers() {
        let text = super::compact_reference();
        assert!(text.contains("LIFECYCLE"), "must have LIFECYCLE section");
        assert!(text.contains("QUERY"), "must have QUERY section");
        assert!(text.contains("METADATA"), "must have METADATA section");
        assert!(text.contains("BULK"), "must have BULK section");
        assert!(
            text.contains("PROJECT MANAGEMENT"),
            "must have PROJECT MANAGEMENT section"
        );
        assert!(
            text.contains("GLOBAL FLAGS"),
            "must have GLOBAL FLAGS section"
        );
        assert!(
            text.contains("WORKFLOW TIPS"),
            "must have WORKFLOW TIPS section"
        );
    }

    #[test]
    fn compact_reference_contains_critical_commands() {
        // These commands are essential for the LLM workflow and must
        // survive any future edits to the compact reference.
        let text = super::compact_reference();
        for cmd in &[
            "story project new",
            "story new",
            "story show",
            "story move",
            "story list",
            "story next",
            "story load-context",
            "story comment",
            "story assign",
            "story prioritize",
            "story decompose",
            "story handoff",
            "story commit-sync",
            "story doctor",
            "--json",
        ] {
            assert!(
                text.contains(cmd),
                "compact_reference must contain '{cmd}' for LLM workflow"
            );
        }
    }

    #[test]
    fn compact_reference_does_not_reference_mcp() {
        let text = super::compact_reference();
        assert!(
            !text.contains("MCP"),
            "compact_reference must not mention MCP"
        );
        assert!(
            !text.contains("mcp"),
            "compact_reference must not mention mcp"
        );
    }

    // ================================================================
    // all_topics_text() contract tests
    // ================================================================

    #[test]
    fn all_topics_text_does_not_include_alias_topics() {
        let text = super::all_topics_text();
        // Aliases should be excluded from the full dump to avoid duplication.
        // We check that the "## awaits" header does NOT appear (aliases are
        // redirects to canonical topics).
        let aliases = ["awaits", "context", "is", "link", "priority", "sync-git"];
        for alias in &aliases {
            let header = format!("## {alias}\n");
            assert!(
                !text.contains(&header),
                "all_topics_text should skip alias topic '{alias}'"
            );
        }
    }

    #[test]
    fn all_topics_text_includes_canonical_topics() {
        let text = super::all_topics_text();
        for topic in &[
            "project",
            "new",
            "list",
            "next",
            "show",
            "move",
            "decompose",
        ] {
            assert!(
                text.contains(&format!("## {topic}")),
                "all_topics_text must include canonical topic '{topic}'"
            );
        }
    }

    #[test]
    fn all_topics_text_does_not_reference_mcp() {
        let text = super::all_topics_text();
        assert!(
            !text.contains("mcp-config"),
            "all_topics_text must not reference mcp-config"
        );
        // Note: the string "MCP" could appear generically in docs,
        // but "mcp-config" is the specific command that was removed.
    }
}
