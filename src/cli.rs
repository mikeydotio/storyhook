use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::normalize_labels;
use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HooksAction {
    Install,
    Uninstall,
    List,
    Test { event_type: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphMode {
    Overview,
    CriticalPath,
    BlockedBy(String),
    ParallelGroups,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseAction {
    List,
    Show {
        phase: String,
    },
    Add {
        id: String,
        phase: String,
    },
    Remove {
        id: String,
    },
    Create {
        phase: String,
        title: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeAction {
    List,
    Add {
        slug: String,
        description: Option<String>,
        emoji: Option<String>,
    },
    Set {
        slug: String,
        description: Option<String>,
        /// `--no-description`, which clears rather than sets.
        clear_description: bool,
        emoji: Option<String>,
        /// `--no-emoji`, which clears rather than sets.
        clear_emoji: bool,
    },
    Remove {
        slug: String,
    },
}

/// The `story state …` subcommands, grouped the same way [`TypeAction`]
/// groups `story type …`.
///
/// Values stay as the raw strings the user typed; `app::run` parses and
/// validates them, like every other invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateAction {
    List,
    Add {
        slug: String,
        superstate: String,
        role: Option<String>,
        description: Option<String>,
    },
    Set {
        slug: String,
        superstate: Option<String>,
        /// `--role active` sets the role, `--role none` clears it, absent
        /// leaves it alone. `none` is unambiguous because `active` is the
        /// only role the tool recognizes.
        role: Option<String>,
        description: Option<String>,
        /// `--no-description`, which clears rather than sets.
        clear_description: bool,
        /// `--move-stories-to <slug>`: where open stories go when this edit
        /// reclassifies the state they are sitting in.
        move_stories_to: Option<String>,
    },
    Remove {
        slug: String,
        move_stories_to: Option<String>,
    },
    Reorder {
        order: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpicAction {
    List,
    Show { id: String },
    Create { title: String },
    Add { epic_id: String, story_id: String },
}

pub const HELP_TEXT: &str = r#"story - CLI-first issue tracker for AI agents

Usage:
  story project new --prefix <PREFIX> [--name <NAME>] [--attach <PATH> | --no-attach]
                    [--no-agents-md]                (no flags at a terminal: it asks)
  story project link origin [URL] | link checkout [PATH]
  story project unlink origin [URL] | unlink checkout
  story project delete [--force]                   (delete a project and its stories)
  story project list                               (every project storyhook knows)
  story project settings list|get|set|unset        (this project's settings)
  story new <title> [--state <slug>] [--type <slug>] [--description <text>]
                    [--priority <level>] [--assignee <member>] [--label <name> ...]
  story tui                                           (interactive terminal UI)
  story web start [--port <PORT>]                  (start web dashboard)
  story web stop                                   (stop web dashboard)
  story web open                                   (open the dashboard in your browser)
  story web address                                (copy the dashboard URL to the clipboard)
  story member add "<name <email>>"
  story member add -g <github-handle>
  story state list
  story state add <state-slug> --super OPEN|CLOSED [--role active]
                               [--description "<text>"]
  story state set <state-slug> [--super OPEN|CLOSED] [--role active|none]
                               [--description "<text>"] [--no-description]
                               [--move-stories-to <state-slug>]
  story state remove <state-slug> [--move-stories-to <state-slug>]
  story state reorder <state-slug,state-slug,...>   (board column order)
  story list [--state <slug>] [--assignee <id|handle>] [--flagged] [--priority <levels>]
             [--label <labels>] [--created-after <date>] [--updated-after <date>]
             [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>]
  story next [--count <n>] [--phase <N>]
  story summary
  story report [--html]
  story search <query>
  story import [<file>]
  story export
  story decompose <file> [--dry-run]     (markdown or YAML)
  story decompose --stdin [--dry-run]
  story import-project <file>
  story migrate [<path>] [--dry-run]               (move a .storyhook tree into the store)
  story store new <path>                           (create an empty store beside the default one)
  story load-context [--format markdown|json]
  story handoff [--since <duration>]
  story phase list
  story phase show <N>
  story phase add <id> <N>
  story phase remove <id>
  story phase create <N> ["<title>"]
  story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
  story doctor [--fix]
  story update [--check] [--force]                 (self-update the story binary)
  story hooks install|uninstall|list|test <event_type>
  story commit-sync [--since <duration>]
  story github-sync [<id>] [--dry-run] [--resolve local|remote]
  story scaffold agents-md|claude-md|cursor-rules
  story help [<command>] [--compact] [--all]
  story plugin install|uninstall <target>
  story show <id>
  story comment <id> "<text>"
  story assign <id> <member-id|handle>
  story move <id> <state-slug> [--if-state <expected>] ["<comment>"]
  story block <id> "<reason>"
  story unblock <id>
  story prioritize <id> <critical|high|medium|low|none>
  story label <id> <labels-csv>
  story unlabel <id> <labels-csv>
  story reopen <id> [--force]
  story delete <id> "<reason>"
  story purge <id> [--force]                       (permanently remove a deleted story)
  story set <id> [--title "<title>"] [--state <slug>] [--priority <level>]
                  [--assignee <member>] [--labels "<csv>"] [--blocked "<reason>"]
                  [--unblocked] [--json "<json>"] [--type <slug>]
                  [--description "<text>"]
  story relate <a> <relationship-type> <b>
  story unrelate <a> <relationship-type> <b>
  story link <a> <relationship-type> <b>
  story unlink <a> <relationship-type> <b>
  story type list
  story type add <slug> [--description "<text>"] [--emoji <glyph>]
  story type set <slug> [--description "<text>"] [--no-description]
                        [--emoji <glyph>] [--no-emoji]
  story type remove <slug>
  story epic list
  story epic show <id>
  story epic create "<title>"
  story epic add <epic-id> <story-id>

Story ids:
  Everywhere <id> appears above, both forms name the same story: the canonical
  `SH-5`, and the bare number `5` on its own. The number is read against the
  project the command is acting on, so `5` means nothing until that is settled —
  with no project, you get the same refusal every other command gives, not a
  missing story. An id carrying a *different* project's prefix is refused
  outright rather than resolved: `--project` decides which project you are in,
  and an id never overrides it.

Global options:
  --json          Emit structured JSON
  --quiet         Suppress success output
  --no-hooks      Suppress event hook execution
  --store-path <file>
                  Run against a named store rather than the default one. Also
                  spelled $STORYHOOK_STORE_PATH. One daemon serves one store, so
                  a command under this flag can neither read nor write any other.
  --project <slug>
                  Act on this project, whatever directory you are in. Also
                  spelled $STORYHOOK_PROJECT, which the flag beats. With neither,
                  storyhook uses the project this checkout belongs to, and
                  refuses rather than guessing when there is none.
                  `story project list` shows the slugs.
  --deadline <seconds>
                  Give up waiting on the daemon after this long, rather than
                  however long starting one and running the command could
                  otherwise take. The request is not cancelled: the daemon
                  finishes what it accepted, this process just stops waiting
                  for the answer. For a caller — a session hook, a script —
                  that cannot wait regardless of whether storyhook could.
  -h, --help
  -V, --version   Print the installed story version
"#;

/// One parsed command line: what to do, plus the three global flags.
///
/// Only `no_hooks` reaches the layer that executes the invocation — `json`
/// and `quiet` are rendering decisions, consumed by
/// [`crate::output::render_response`] after the work is done. That split is
/// why [`crate::invoke::InvokeRequest`], not this type, is what crosses a
/// process boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CliOptions {
    pub json: bool,
    pub quiet: bool,
    pub no_hooks: bool,
    pub invocation: Invocation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberInput {
    Identity(String),
    Github(String),
}

/// Every command storyhook can execute, fully parsed and validated for
/// *shape* — field values stay as the raw strings the user typed, because
/// interpreting them needs project data the parser cannot see.
///
/// This is the request half of the wire envelope (the response half is
/// [`crate::output::Response`]): it is what a client sends and a server
/// executes, so it must stay serializable and free of anything
/// process-local. Every field is currently a `String`, `bool`, `usize`,
/// `u16`, `PathBuf` or a collection of those.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Invocation {
    Help,
    Project {
        action: ProjectAction,
    },
    New {
        title: String,
        state: Option<String>,
        story_type: Option<String>,
        description: Option<String>,
        priority: Option<String>,
        labels: Option<Vec<String>>,
        assignee: Option<String>,
    },
    MemberAdd {
        input: MemberInput,
    },
    State {
        action: StateAction,
    },
    List {
        state: Option<String>,
        assignee: Option<String>,
        flagged: bool,
        priority: Option<String>,
        label: Option<String>,
        created_after: Option<String>,
        updated_after: Option<String>,
        blocked: bool,
        ready: bool,
        stale: Option<String>,
        phase: Option<String>,
        story_type: Option<String>,
    },
    Search {
        query: String,
    },
    Next {
        count: usize,
        phase: Option<String>,
    },
    Summary,
    Report {
        html: bool,
    },
    Doctor {
        fix: bool,
    },
    Show {
        id: String,
    },
    Comment {
        id: String,
        text: String,
    },
    Assign {
        id: String,
        member: String,
    },
    SetState {
        id: String,
        state: String,
        comment: Option<String>,
        if_state: Option<String>,
    },
    SetAwaiting {
        id: String,
        awaiting: String,
    },
    ClearAwaiting {
        id: String,
    },
    SetPriority {
        id: String,
        priority: String,
    },
    SetLabels {
        id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    Reopen {
        id: String,
        force: bool,
    },
    Delete {
        id: String,
        reason: String,
    },
    /// Remove a soft-deleted story permanently.
    ///
    /// The irreversible half of `delete`, and a verb of its own rather than a
    /// flag on it: a flag that turns a reversible act irreversible sits one
    /// keystroke away from the reversible one.
    Purge {
        id: String,
        /// Whether the confirmation has already been given. An unforced purge
        /// answers with `Response::ConfirmationRequired` and writes nothing.
        force: bool,
    },
    BulkUpdate {
        updates: Vec<(String, String)>,
    },
    Import {
        file: Option<String>,
    },
    Decompose {
        file: Option<String>,
        stdin: bool,
        dry_run: bool,
    },
    Export,
    ImportProject {
        file: String,
    },
    /// Move a legacy `.storyhook` tree into the store.
    ///
    /// Additive and one-way: it reads the tree, never writes to it, and refuses
    /// to run twice against one checkout. The legacy directory is left exactly
    /// as it was, because it is the operator's rollback.
    Migrate {
        /// The checkout holding `.storyhook`. `None` walks up from the working
        /// directory, so the command works from anywhere inside the repository.
        path: Option<String>,
        /// Report what would be imported and write nothing.
        dry_run: bool,
    },
    Context {
        format: Option<String>,
    },
    Handoff {
        since: Option<String>,
    },
    Phase {
        action: PhaseAction,
    },
    Type {
        action: TypeAction,
    },
    Epic {
        action: EpicAction,
    },
    Graph {
        mode: GraphMode,
    },
    SetFields {
        id: String,
        title: Option<String>,
        state: Option<String>,
        priority: Option<String>,
        assignee: Option<String>,
        labels: Option<String>,
        blocked: Option<String>,
        unblocked: bool,
        json: Option<String>,
        story_type: Option<String>,
        description: Option<String>,
    },
    Relate {
        a: String,
        relation: String,
        b: String,
        remove: bool,
    },
    Hooks {
        action: HooksAction,
    },
    Scaffold {
        kind: String,
    },
    CommitSync {
        since: Option<String>,
    },
    GithubSync {
        id: Option<String>,
        dry_run: bool,
        /// Which side of every conflict this run meets wins, if the caller has
        /// said. `None` is not "guess" — it is the reason the run refuses.
        resolve: Option<ConflictSide>,
        /// The initial-sync strategy, if the caller has said in advance.
        /// `None` on an unconfigured project means "ask" — SH-153's D2.
        strategy: Option<SetupStrategy>,
        /// The sync mode to save, if the caller has said in advance. Answers
        /// the same question as `strategy` and must be given together with
        /// it, or not at all.
        mode: Option<SetupMode>,
    },
    HelpTopic {
        topic: String,
    },
    HelpCompact,
    HelpAll,
    Plugin {
        action: PluginAction,
    },
    Web {
        action: WebAction,
    },
    /// The storyhook daemon: the one process that owns the store and serves
    /// everything that talks to it.
    Daemon {
        action: DaemonAction,
    },
    Store {
        action: StoreAction,
    },
    SessionStart,
    Update {
        check: bool,
        force: bool,
    },
    Version,
    /// Everything a long-lived client needs to render a project, in one
    /// round trip.
    ///
    /// Not reachable from the command line, and deliberately so: a shell user
    /// asking for the whole project already has `story list` and `story
    /// export`. This exists for clients that hold a *model* — the TUI, which
    /// rebuilds its board after every change, and the dashboard's resync after
    /// a dropped event stream. Without it those clients issue four or five
    /// reads per refresh and see a different instant in each one.
    ProjectSnapshot,
    /// Read or replace one story's raw event history.
    ///
    /// The TUI's undo primitive, and nothing else's: undo snapshots the exact
    /// log a story had before a mutation and puts it back afterwards, which is
    /// neither an append nor a compensating edit. It is here rather than in
    /// the TUI because the seam has to be the only way a client reaches
    /// project data — a client that keeps one filesystem call keeps all of
    /// them.
    ///
    /// Not reachable from the command line either. Reading a raw history is
    /// what `story export` is for, and rewriting one is not something a user
    /// should be able to ask for by accident.
    History {
        action: HistoryAction,
    },
}

/// The two halves of the undo primitive: snapshot a history, and put one back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryAction {
    /// Every event of one story, oldest first. An unknown story has an empty
    /// history rather than being an error: a client asking "what was there
    /// before?" about a story that was not there gets the honest answer.
    Read { id: String },
    /// Replaces one story's history with `events`.
    ///
    /// An **empty** `events` means "this story should not exist" — undoing a
    /// creation. Anything else is a verbatim replacement.
    Restore {
        id: String,
        events: Vec<crate::domain::StoryEvent>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginAction {
    Install { target: String },
    Uninstall { target: String },
}

/// Default TCP port for the web dashboard, used by both `story web start`
/// and the internal `story web --serve` daemon entrypoint.
pub const DEFAULT_WEB_PORT: u16 = 3456;

/// `story daemon …`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonAction {
    /// Run the daemon in this process, in the foreground. What the background
    /// spawner execs, and what a launchd agent runs.
    Serve {
        /// Bind this port instead of the environment's preferred one.
        port: Option<u16>,
    },
    /// Start a daemon in the background, if one is not already running.
    Start {
        /// Bind this port instead of the environment's preferred one.
        port: Option<u16>,
    },
    /// Ask the running daemon to shut down.
    Stop,
    /// Report whether one is running, and where.
    Status,
    /// Register a launchd agent so the daemon starts at login.
    Install,
    /// Remove that agent.
    Uninstall,
    /// Print the running daemon's bearer token (SH-50) — the value a caller
    /// puts in `X-Storyhook-Token` to reach `/api/v1/*` or the dashboard's
    /// dispatch endpoint from off-loopback.
    Token,
}

/// The `story project …` subcommands — a repository's whole lifecycle.
///
/// A verb group rather than loose top-level verbs because all of these are
/// about the *project* rather than about a story, and because the two that
/// change something need the same thing: a way to name a checkout other than
/// "wherever you happen to be standing".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectAction {
    /// Create a project, optionally attaching a checkout to it.
    ///
    /// The verb that replaced `init`, and differs from it in three ways and no
    /// others: the checkout is named by `--attach PATH` rather than by a
    /// positional nobody could tell from a name, `--no-attach` makes the
    /// filesystem opt-out sayable at all, and a prefix is *required* rather
    /// than silently defaulted to `SH` (SH-109).
    New(NewProjectRequest),
    /// Destroy a project and everything recorded against it.
    ///
    /// **No target of its own.** Which project this is comes from the ordinary
    /// selector — the working directory, `--project <slug>` or
    /// `STORYHOOK_PROJECT` — so it is answered against a resolved
    /// [`Ctx`](crate::service::Ctx) like [`Settings`](Self::Settings),
    /// [`Link`](Self::Link) and [`Unlink`](Self::Unlink).
    ///
    /// `deinit` took a bare word and had to decide whether it was a path or a
    /// slug. That guess is a fourth way of naming a project beside SH-116's
    /// three, and it is the one that could silently name the wrong one: a slug
    /// that happens to match a directory name resolved as the directory.
    Delete {
        /// Authorize the destruction without being asked.
        force: bool,
    },
    /// Every project the store knows, checkout or no checkout.
    List,
    /// This project: which one the ordinary selector resolved, and where its
    /// repo-side work runs.
    ///
    /// **The scoped singular to [`List`](Self::List)'s plural**, and the only
    /// machine-readable answer to "which project am I?" — a question nothing
    /// else in the CLI could answer, because `list` enumerates *every* project
    /// and is dispatched without a [`Ctx`](crate::service::Ctx) at all.
    ///
    /// Spelled `show` rather than `current` deliberately: there is no
    /// current-project state and no default (`tests/project_selection.rs`), so
    /// a verb whose name asserted stored state would be misread forever. It
    /// mirrors `story show <id>`, which is the scoped singular for a story.
    ///
    /// **It reports; it never judges.** A project with no linked checkout
    /// answers with `checkout: null` and exit 0, because this is the command
    /// someone runs to find out *why* a dispatch refused, and a diagnostic that
    /// refuses in the state being diagnosed is useless. The refusal belongs to
    /// the consumer that actually needs a directory — `story.sh dispatch`.
    Show,
    /// Attach one of this project's two optional git associations.
    Link(LinkTarget),
    /// Detach one.
    Unlink(UnlinkTarget),
    /// Read and write this project's settings.
    ///
    /// One of the arms of this family that names a project rather than
    /// creating, destroying or enumerating them — so it is answered against a
    /// resolved [`Ctx`](crate::service::Ctx) rather than by
    /// `dispatch_unscoped`. [`Link`](Self::Link) and [`Unlink`](Self::Unlink)
    /// are the others.
    Settings(SettingsAction),
}

/// What `story project new` was told, or that it was told nothing at all.
///
/// **The absence of every verb-local switch is itself the request.** A bare
/// `story project new` is somebody asking to be walked through it; the same
/// command with any switch on it is a script that has already decided. Nothing
/// else distinguishes the two, which is what makes the rule predictable from
/// either side — and it is the rule `main.rs::confirm()` already applies, so
/// the program has one rule about prompting rather than two.
///
/// [`Ask`](Self::Ask) is resolved into [`Stated`](Self::Stated) by the client,
/// which is the only process with a terminal. It is nonetheless a wire variant
/// rather than a client-side placeholder, because a request carrying it *can*
/// reach the dispatcher — a caller that went round `main.rs`, a hand-built
/// `InvokeRequest`, a future front-end — and the dispatcher must refuse it
/// loudly. The alternative, quietly supplying defaults for what it could not
/// ask about, is SH-109's silent `SH` wearing a new verb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewProjectRequest {
    /// No verb-local switch was given: ask, or refuse.
    Ask,
    /// Everything the verb needs, stated on the command line.
    Stated(NewProjectSpec),
}

/// A fully specified `story project new`.
///
/// Every field the verb needs is here and none of them is a promise the caller
/// did not make: [`prefix`](Self::prefix) is a `String` rather than an
/// `Option`, so "created without anybody choosing a prefix" is not a state this
/// type can hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewProjectSpec {
    /// What, if anything, the project is attached to.
    pub attach: Attach,
    /// The story-id prefix, already through
    /// [`domain::prefix::validate`](crate::domain::prefix::validate).
    pub prefix: String,
    /// The project's display name; `None` takes the attach target's basename.
    pub name: Option<String>,
    /// Skip generating `AGENTS.md`.
    pub no_agents_md: bool,
}

/// The checkout `story project new` attaches, or the decision not to.
///
/// One enum rather than `Option<String>` beside a `no_attach: bool`, because
/// those two fields can disagree — `--attach ./here --no-attach` is
/// representable in that shape and meaningless. Here the parser refuses the
/// contradiction once and nothing downstream can meet it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attach {
    /// The directory the client ran in — what `--attach` defaults to, and what
    /// a bare `story project new` does.
    Cwd,
    /// The named directory.
    ///
    /// Carried as text and left unresolved on purpose: a relative path resolves
    /// against the *client's* working directory, and over the daemon that is
    /// not this process's.
    Path(String),
    /// Nothing. The store record is written and no directory is touched,
    /// recorded or resolved.
    ///
    /// Deliberately **outside** SH-95's temp-store guard, because nothing is
    /// created at a path for that guard to judge — pinned as a recorded
    /// narrowing rather than left to be rediscovered as a hole.
    Nothing,
}

/// What `story project link` attaches.
///
/// **Two associations, and the asymmetry between them is the design.** An
/// origin is the *only* thing project selection ever consults; a checkout is
/// never consulted for resolution at all and answers a different question —
/// where this project's repo-side work runs. They are variants of one enum
/// because they share a verb, not because they are alike.
///
/// Not shared with [`UnlinkTarget`]: `unlink checkout` takes no argument,
/// because a project has at most one, and a single enum would have to spell
/// that as a field the parser accepts and the dispatcher throws away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// `link origin [URL]`.
    ///
    /// `None` means the origin *this directory's own repository* records. See
    /// [`origin_here`](crate::service::project::origin_here) for why "its own"
    /// is a condition rather than a description.
    Origin {
        /// The URL as the user typed it, unnormalized; `None` reads it from git.
        url: Option<String>,
    },
    /// `link checkout [PATH]`.
    ///
    /// Carried as text and left unresolved on purpose: a relative path resolves
    /// against the *client's* working directory, and over the daemon that is
    /// not this process's. `None` means that directory.
    Checkout {
        /// The directory, or `None` for the one the client ran in.
        path: Option<String>,
    },
}

/// What `story project unlink` detaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnlinkTarget {
    /// `unlink origin [URL]`, reading the working directory's origin when the
    /// URL is omitted, exactly as [`LinkTarget::Origin`] does.
    Origin {
        /// The URL as the user typed it; `None` reads it from git.
        url: Option<String>,
    },
    /// `unlink checkout` — no argument, because a project has at most one and
    /// naming it would be a second way of saying the same thing.
    Checkout,
}

/// The `story project settings …` forms.
///
/// Explicit subcommands rather than flags or a bare `key=value`, matching every
/// other verb group in this parser — and so that one missing shell word cannot
/// turn a read into a write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsAction {
    /// Every setting, with its effective value and where that value came from.
    List,
    /// One setting.
    Get {
        /// The dotted name, such as `sync.auto_transition`.
        key: String,
    },
    /// Write one setting.
    Set {
        /// The dotted name.
        key: String,
        /// The value, validated against the setting's kind.
        value: String,
    },
    /// Clear one setting, returning it to its default or to nothing.
    Unset {
        /// The dotted name.
        key: String,
    },
}

/// Which side of a github-sync conflict the caller has chosen.
///
/// The wire form of `github::conflict::Resolution`, and separate from it on
/// purpose: this enum is part of the request envelope and must exist in every
/// build, while the merge engine that acts on it lives behind the
/// `github-sync` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictSide {
    /// Keep what this machine has, and push it to GitHub.
    Local,
    /// Take what GitHub has.
    Remote,
}

/// The initial-sync strategy for a github-sync project that has never been
/// configured, stated up front rather than picked from a menu the daemon
/// cannot show (SH-153's D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SetupStrategy {
    ImportAll,
    MatchTitles,
    PushOnly,
    FutureOnly,
}

/// The sync mode a fresh setup should save, stated up front.
///
/// Not `github::sync_state::SyncMode`: that type lives behind the
/// `github-sync` feature, and this flag's value must exist in every build —
/// the same reason [`ConflictSide`] is not `github::conflict::Resolution`.
/// `auto` is unspellable on purpose: nothing implements it (SH-68), and
/// offering a mode nothing acts on is the defect `github::initial`'s own
/// `MODE_OPTIONS` already exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupMode {
    Manual,
    Off,
}

/// The `story store …` subcommands.
///
/// About *stores* rather than about anything inside one, which is what makes
/// them different from every other verb: `store new` names the store it creates,
/// so it must not resolve — let alone create — the ambient one on its way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreAction {
    /// Create an empty store at a path nothing else owns.
    New {
        /// Where to put it. Resolved against the client's working directory.
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebAction {
    Start { port: u16 },
    Stop,
    Status,
    Serve { port: u16 },
    Open,
    Address,
}

/// The global flags, and everything that is not one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalFlags {
    /// `--json`: render the machine-readable envelope.
    pub json: bool,
    /// `--quiet`: suppress the human rendering.
    pub quiet: bool,
    /// `--no-hooks`: do not fire the project's event hooks.
    pub no_hooks: bool,
    /// `--store-path <file>`: run this command against a named store.
    ///
    /// Global because it has to be: the store is resolved before a verb is
    /// dispatched, and a flag that only some commands honoured would be a flag
    /// that silently stops applying somewhere in the middle of a script.
    ///
    /// It names a **file**, not a directory, which is what makes it a different
    /// lever from `$STORYHOOK_DATA_DIR`. `main` publishes the resolved path into
    /// `$STORYHOOK_STORE_PATH` so that everything this invocation starts — the
    /// daemon, a git hook, a `story` a hook itself runs — agrees about which
    /// store it is in.
    pub store_path: Option<PathBuf>,
    /// `--project <slug>`: act on this project, whatever directory this is.
    ///
    /// Global for a different reason than [`Self::store_path`], and the
    /// difference is worth keeping straight. A store is a process-wide fact and
    /// is *published* to children; a project is a fact about one invocation and
    /// is deliberately **not**, because exporting it for every descendant would
    /// be the "current project" state SH-116 exists to abolish.
    ///
    /// What makes it global is the parser: [`split_global_flags`] runs before a
    /// verb is read, so one entry here reaches every verb at once — and, because
    /// the token never survives to the verb's own arguments, SH-62's
    /// fail-closed gate never has to be told about it.
    pub project: Option<String>,
    /// `--deadline <seconds>`: give up waiting on the daemon after this long,
    /// rather than however long `lifecycle::ensure` and the invoker's own
    /// exchange bound would otherwise allow (up to `SPAWN_LOCK_DEADLINE` +
    /// `SERVED_DEADLINE` — 150s).
    ///
    /// A flag, not an environment variable, and deliberately so: a caller that
    /// cannot wait — a session hook Claude Code will kill regardless — states
    /// that once, for the one invocation asking. `$STORYHOOK_EXCHANGE_DEADLINE_SECS`
    /// already shows the hazard the other shape has: a variable is set once
    /// and inherited by everything a shell starts afterward, so one `export`
    /// would silently abandon every write on the machine (SH-182).
    ///
    /// Expiry does not cancel the request: the daemon finishes whatever it
    /// accepted, and this process simply stops waiting for the answer and
    /// reports so. Applied in `main`, global because the bound has to cover
    /// the whole round trip, including starting a daemon that does not exist
    /// yet, which happens before a verb is dispatched.
    pub deadline: Option<std::time::Duration>,
}

/// Splits the global flags out of `args`, leaving the verb and its own
/// arguments behind.
///
/// Fallible only because `--store-path` takes a value: a flag whose value went
/// missing must not be dropped on the floor, because the command would then run
/// against a *different store* than the one the caller named.
pub fn split_global_flags(args: &[String]) -> Result<(GlobalFlags, Vec<String>), AppError> {
    let mut flags = GlobalFlags::default();
    let mut filtered = Vec::new();

    let mut i = 0;
    while i < args.len() {
        // A bare `--` ends option scanning for globals too. Without this the
        // terminator would be an escape that leaks: `story comment SH-1 --
        // --json is great` would still lose the word and still flip stdout to
        // an envelope, which is worse than having no escape at all. The
        // terminator itself is kept and removed later, so the verb parser can
        // still see where it was.
        if args[i] == "--" {
            filtered.extend_from_slice(&args[i..]);
            break;
        }
        match args[i].as_str() {
            "--json" => {
                // If --json is followed by a JSON object literal, treat it as a
                // subcommand-specific --json <value> (e.g. `story set SH-1 --json '{...}'`)
                // rather than the global JSON-output flag.
                if let Some(next) = args.get(i + 1)
                    && next.starts_with('{')
                {
                    filtered.push(args[i].clone());
                    filtered.push(next.clone());
                    i += 2;
                    continue;
                }
                flags.json = true;
            }
            "--quiet" => flags.quiet = true,
            "--no-hooks" => flags.no_hooks = true,
            "--store-path" => {
                let Some(value) = args.get(i + 1).filter(|value| !value.is_empty()) else {
                    return Err(AppError::Usage(
                        "--store-path needs the path of a store file, for example \
                         `--store-path /tmp/scratch/store.db`."
                            .to_string(),
                    ));
                };
                flags.store_path = Some(PathBuf::from(value));
                i += 2;
                continue;
            }
            other if other.starts_with("--store-path=") => {
                let value = other.trim_start_matches("--store-path=");
                if value.is_empty() {
                    return Err(AppError::Usage(
                        "--store-path= was given no path. It names a store file, for example \
                         `--store-path=/tmp/scratch/store.db`."
                            .to_string(),
                    ));
                }
                flags.store_path = Some(PathBuf::from(value));
            }
            "--project" => {
                let Some(value) = args.get(i + 1).filter(|value| !value.is_empty()) else {
                    return Err(AppError::Usage(
                        "--project needs the slug of a project, for example \
                         `--project storyhook`. `story project list` shows the slugs this \
                         machine's store has."
                            .to_string(),
                    ));
                };
                flags.project = Some(value.clone());
                i += 2;
                continue;
            }
            other if other.starts_with("--project=") => {
                let value = other.trim_start_matches("--project=");
                if value.is_empty() {
                    return Err(AppError::Usage(
                        "--project= was given no slug. It names a project, for example \
                         `--project=storyhook`. `story project list` shows the slugs this \
                         machine's store has."
                            .to_string(),
                    ));
                }
                flags.project = Some(value.to_string());
            }
            "--deadline" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(deadline_usage());
                };
                flags.deadline = Some(parse_deadline_secs(value)?);
                i += 2;
                continue;
            }
            other if other.starts_with("--deadline=") => {
                let value = other.trim_start_matches("--deadline=");
                flags.deadline = Some(parse_deadline_secs(value)?);
            }
            _ => filtered.push(args[i].clone()),
        }
        i += 1;
    }

    Ok((flags, filtered))
}

/// The error for `--deadline` given no value at all.
fn deadline_usage() -> AppError {
    AppError::Usage(
        "--deadline needs a number of seconds to wait for the daemon, for example \
         `--deadline 3`."
            .to_string(),
    )
}

/// Parses `--deadline`'s value: a non-negative whole number of seconds.
///
/// `0` is accepted rather than refused — it is a legitimate (if extreme)
/// request to abandon the very first wait, and refusing it would be one more
/// special case for a caller to work around. What it must not be is negative
/// or fractional: both parse as a `u64` failing, which folds them into the
/// same "not a number of seconds" message rather than needing their own.
fn parse_deadline_secs(value: &str) -> Result<std::time::Duration, AppError> {
    value.parse::<u64>().map(std::time::Duration::from_secs).map_err(|_| {
        AppError::Usage(format!(
            "--deadline=`{value}` is not a whole number of seconds, for example \
             `--deadline 3`."
        ))
    })
}

/// Whether `args` — a whole invocation, verb included — asks a verb to
/// explain itself rather than to run.
///
/// A `-h`/`--help` anywhere after the verb is a help request. It is never
/// data: no flag in this grammar takes a value that begins with `--` (see
/// [`parse_dash_flags`]), and a title, query, or comment that must literally
/// start with `--help` was never expressible in the position the verb reads
/// it from anyway.
pub fn is_help_request(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .any(|arg| arg == "-h" || arg == "--help")
}

/// The help a recognized verb answers a help request with: its own topic
/// when one exists, otherwise the general help.
fn help_for_verb(verb: &str) -> Invocation {
    let topic = match verb {
        "context" => "load-context",
        "sync-git" => "commit-sync",
        "link" => "relate",
        "unlink" => "unrelate",
        other => other,
    };
    match crate::help_topics::get_help_topic(topic) {
        Some(_) => Invocation::HelpTopic {
            topic: topic.to_string(),
        },
        None => Invocation::Help,
    }
}

/// Answers `story <verb> --help` before the verb's own parser ever sees the
/// token.
///
/// Verbs parse their own flags, so a rule applied per-verb is a rule some
/// verb will miss: `story new --help` read `--help` as the title and created
/// a story called `--help`, allocating an id, bumping `next-id`, and dirtying
/// a storyhook-tracked repo — in answer to the conventional way of asking
/// what a command does (SH-52). This is the one place ahead of all of them.
///
/// An unrecognized verb is left alone, so `story frobnicate --help` still
/// reports the unknown command rather than papering over a typo with usage
/// text. Recognition asks [`dispatch`] rather than consulting a second copy
/// of the verb list, which could fall out of step with the first.
fn verb_help_request(args: &[String]) -> Option<Invocation> {
    let verb = args.first()?.as_str();
    // `story help <topic> --help` is `parse_help`'s to answer.
    if verb == "help" || verb.starts_with('-') || !is_help_request(args) {
        return None;
    }
    verb_is_recognized(verb).then(|| help_for_verb(verb))
}

/// Whether `verb` names a command this binary has.
///
/// Asks [`dispatch`] rather than consulting a second copy of the verb list,
/// which could fall out of step with the first. Every caller wants the same
/// answer for the same reason — to decline to speak about a verb that does not
/// exist, so a typo is reported as an unknown *command* rather than being
/// answered with usage text or a complaint about one of its flags.
fn verb_is_recognized(verb: &str) -> bool {
    // `tui` is dispatched in main.rs before parsing ever happens, so
    // `dispatch` does not know it, but it is a verb like any other here.
    verb == "tui"
        || !matches!(
            dispatch(&[verb.to_string()]),
            Err(AppError::Usage(ref message)) if message.starts_with("unknown command")
        )
}

/// One long flag a verb accepts, and whether the token after it is its value.
///
/// `takes_value` exists so the gate stays a *necessary-condition* check: the
/// token after `--description` is that flag's value and is never judged, so
/// `story new t --description --odd` keeps working exactly as it does today.
/// The gate may only refuse what a parser would have swallowed; it may never
/// refuse what one would have accepted.
#[derive(Clone, Copy, Debug)]
struct Flag {
    name: &'static str,
    takes_value: bool,
}

/// A value-taking flag.
const fn value(name: &'static str) -> Flag {
    Flag {
        name,
        takes_value: true,
    }
}

/// A flag that stands alone.
const fn bare(name: &'static str) -> Flag {
    Flag {
        name,
        takes_value: false,
    }
}

/// The long flags one verb path accepts.
///
/// `subcommand` is `Some` only where two subcommands of the same verb accept
/// genuinely different flags (`state add` versus `state set`). Lookup tries the
/// two-token key first and falls back to the verb alone, which is what keeps
/// `story move SH-1 done --if-state x` working: `SH-1` is an argument, not a
/// subcommand, so no two-token entry matches and `move`'s own entry answers.
struct VerbFlags {
    verb: &'static str,
    subcommand: Option<&'static str>,
    flags: &'static [Flag],
}

/// Every long flag this CLI accepts, by verb path.
///
/// **This table fails closed.** A verb with no entry declares nothing, so every
/// flag-shaped token reaching it is refused. That is deliberate: forgetting to
/// declare a new verb's flags produces a loud, immediate error, where forgetting
/// a per-verb guard would silently re-inherit SH-62. Rot in the loud direction
/// is recoverable; rot in the quiet direction is this defect.
///
/// Two verbs cannot be checked against their own help text, because their help
/// names no flags at all — see `UNDISCOVERABLE` in `tests/unknown_flag_sweep.rs`.
static VERB_FLAGS: &[VerbFlags] = &[
    VerbFlags {
        verb: "new",
        subcommand: None,
        flags: &[
            value("state"),
            value("type"),
            value("description"),
            value("priority"),
            value("assignee"),
            value("label"),
            value("labels"),
        ],
    },
    VerbFlags {
        verb: "list",
        subcommand: None,
        flags: &[
            value("state"),
            value("assignee"),
            value("priority"),
            value("label"),
            value("created-after"),
            value("updated-after"),
            value("stale"),
            value("phase"),
            value("type"),
            bare("flagged"),
            bare("blocked"),
            bare("ready"),
        ],
    },
    VerbFlags {
        verb: "next",
        subcommand: None,
        flags: &[value("count"), value("phase")],
    },
    VerbFlags {
        verb: "set",
        subcommand: None,
        flags: &[
            value("title"),
            value("state"),
            value("priority"),
            value("assignee"),
            value("labels"),
            value("blocked"),
            value("json"),
            value("type"),
            value("description"),
            bare("unblocked"),
        ],
    },
    VerbFlags {
        verb: "move",
        subcommand: None,
        flags: &[value("if-state")],
    },
    VerbFlags {
        verb: "reopen",
        subcommand: None,
        flags: &[bare("force")],
    },
    VerbFlags {
        verb: "purge",
        subcommand: None,
        flags: &[bare("force")],
    },
    VerbFlags {
        verb: "report",
        subcommand: None,
        flags: &[bare("html")],
    },
    VerbFlags {
        verb: "doctor",
        subcommand: None,
        flags: &[bare("fix")],
    },
    VerbFlags {
        verb: "update",
        subcommand: None,
        flags: &[bare("check"), bare("force")],
    },
    VerbFlags {
        verb: "handoff",
        subcommand: None,
        flags: &[value("since")],
    },
    VerbFlags {
        verb: "commit-sync",
        subcommand: None,
        flags: &[value("since")],
    },
    VerbFlags {
        verb: "sync-git",
        subcommand: None,
        flags: &[value("since")],
    },
    VerbFlags {
        verb: "github-sync",
        subcommand: None,
        flags: &[
            bare("dry-run"),
            value("resolve"),
            value("strategy"),
            value("mode"),
        ],
    },
    VerbFlags {
        verb: "decompose",
        subcommand: None,
        flags: &[bare("stdin"), bare("dry-run")],
    },
    VerbFlags {
        verb: "migrate",
        subcommand: None,
        flags: &[bare("dry-run")],
    },
    VerbFlags {
        verb: "load-context",
        subcommand: None,
        flags: &[value("format")],
    },
    VerbFlags {
        verb: "context",
        subcommand: None,
        flags: &[value("format")],
    },
    VerbFlags {
        verb: "graph",
        subcommand: None,
        flags: &[
            bare("critical-path"),
            bare("parallel-groups"),
            value("blocked-by"),
        ],
    },
    VerbFlags {
        verb: "help",
        subcommand: None,
        flags: &[bare("compact"), bare("all")],
    },
    // `--serve` is a subcommand spelled as a flag: it is what the spawner
    // execs, never what a user types. Declared, or the daemon cannot start.
    VerbFlags {
        verb: "daemon",
        subcommand: None,
        flags: &[bare("serve"), value("port")],
    },
    VerbFlags {
        verb: "web",
        subcommand: None,
        flags: &[bare("serve"), value("port")],
    },
    VerbFlags {
        verb: "project",
        subcommand: Some("new"),
        flags: &[
            value("prefix"),
            value("name"),
            value("attach"),
            bare("no-attach"),
            bare("no-agents-md"),
        ],
    },
    VerbFlags {
        verb: "project",
        subcommand: Some("delete"),
        flags: &[bare("force")],
    },
    // The two retired verbs keep their entries, and this is the reason rather
    // than an oversight. Both are redirects now, and SH-62's gate runs *ahead*
    // of every parser — so without an entry declaring what each used to take,
    // `story project init --prefix AB` would be answered "unknown flag
    // `--prefix`" and the redirect naming `story project new` would never fire.
    // A redirect that only works for the flagless spelling is half a redirect.
    // Both entries go when the redirects do, at 3.0.0.
    VerbFlags {
        verb: "project",
        subcommand: Some("init"),
        flags: &[value("prefix"), value("name"), bare("no-agents-md")],
    },
    VerbFlags {
        verb: "project",
        subcommand: Some("deinit"),
        flags: &[bare("force")],
    },
    // Declared with no flags rather than left out. Under SH-62's fail-closed
    // rule both spellings refuse every flag-shaped token, so the behaviour is
    // identical — but an entry says "this verb takes no flags" where an absence
    // says nothing, and the next person to add one will look here first.
    VerbFlags {
        verb: "project",
        subcommand: Some("link"),
        flags: &[],
    },
    VerbFlags {
        verb: "project",
        subcommand: Some("unlink"),
        flags: &[],
    },
    VerbFlags {
        verb: "project",
        subcommand: Some("show"),
        flags: &[],
    },
    VerbFlags {
        verb: "member",
        subcommand: Some("add"),
        flags: &[value("github")],
    },
    VerbFlags {
        verb: "type",
        subcommand: Some("add"),
        flags: &[value("description"), value("emoji")],
    },
    VerbFlags {
        verb: "type",
        subcommand: Some("set"),
        flags: &[
            value("description"),
            bare("no-description"),
            value("emoji"),
            bare("no-emoji"),
        ],
    },
    VerbFlags {
        verb: "state",
        subcommand: Some("add"),
        flags: &[value("super"), value("role"), value("description")],
    },
    VerbFlags {
        verb: "state",
        subcommand: Some("set"),
        flags: &[
            value("super"),
            value("role"),
            value("description"),
            bare("no-description"),
            value("move-stories-to"),
        ],
    },
    VerbFlags {
        verb: "state",
        subcommand: Some("remove"),
        flags: &[value("move-stories-to")],
    },
];

/// Whether `token` is shaped like a long flag rather than like data.
///
/// Shape, not prefix, and the difference is the whole reason free text
/// survives this gate. A token containing whitespace is never flag-shaped, so a
/// quoted title or comment — `story new "--fix the ingest path"` — arrives as
/// one argv element with a space in it and is data, untouched. `---` and a bare
/// `--` are likewise not flag-shaped: the first is a rule line someone pasted,
/// the second is the end-of-options terminator.
fn is_flag_shaped(token: &str) -> bool {
    if token.chars().any(char::is_whitespace) {
        return false;
    }
    let Some(rest) = token.strip_prefix("--") else {
        return false;
    };
    let name = rest.split_once('=').map_or(rest, |(name, _)| name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// The flags declared for this invocation's verb path, or `None` when the verb
/// declares nothing — which, because the table fails closed, means every
/// flag-shaped token is unknown.
fn declared_flags(args: &[String]) -> Option<&'static [Flag]> {
    let verb = args.first()?.as_str();
    let subcommand = args
        .get(1)
        .map(String::as_str)
        .filter(|token| !is_flag_shaped(token));

    subcommand
        .and_then(|sub| {
            VERB_FLAGS
                .iter()
                .find(|entry| entry.verb == verb && entry.subcommand == Some(sub))
        })
        .or_else(|| {
            VERB_FLAGS
                .iter()
                .find(|entry| entry.verb == verb && entry.subcommand.is_none())
        })
        .map(|entry| entry.flags)
}

/// Refuses a flag-shaped token the verb does not declare, before any parser can
/// read it as data.
///
/// This is the one place ahead of all of them, for the same reason
/// [`verb_help_request`] is: verbs parse their own flags, so a rule applied
/// per-verb is a rule some verb will miss. SH-52 was this defect for `--help`
/// and was fixed one token at a time; SH-62 is the rest of the flag space
/// arriving two waves later, with eight verbs measured writing junk silently
/// and four of them minting a durable object nobody asked for.
///
/// It runs **after** `verb_help_request`, so a help request still outranks a
/// complaint about a flag, and it declines to speak about an unrecognized verb,
/// so `story frobnicate --typo` still reports the unknown *command*.
fn reject_unknown_flags(args: &[String]) -> Result<(), AppError> {
    let Some(verb) = args.first() else {
        return Ok(());
    };
    // `story --help` and `story -V` are not verbs; `dispatch` answers them.
    if verb.starts_with('-') || !verb_is_recognized(verb) {
        return Ok(());
    }

    let declared = declared_flags(args).unwrap_or(&[]);
    let mut index = 1;
    while index < args.len() {
        let token = args[index].as_str();
        if token == "--" {
            return Ok(());
        }
        if !is_flag_shaped(token) {
            index += 1;
            continue;
        }
        let name = token
            .strip_prefix("--")
            .and_then(|rest| rest.split('=').next())
            .unwrap_or_default();
        let Some(flag) = declared.iter().find(|flag| flag.name == name) else {
            return Err(AppError::Usage(unknown_flag_message(args, token, declared)));
        };
        // `--flag=value` carries its value in the same token.
        index += if flag.takes_value && !token.contains('=') {
            2
        } else {
            1
        };
    }
    Ok(())
}

/// The refusal a user reads: the token, the verb's real flags, and how to say
/// it if it was text all along.
///
/// The escape advice is deliberately not a ready-made command. Half the verbs
/// this can fire on take no positional argument at all, so a synthesized
/// `story doctor "--typo …"` would be an example that does not work — worse
/// than no example, because the reader would try it.
fn unknown_flag_message(args: &[String], token: &str, declared: &[Flag]) -> String {
    let path = args
        .iter()
        .take(2)
        .take_while(|word| !is_flag_shaped(word))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let known = if declared.is_empty() {
        format!("`story {path}` takes no flags.")
    } else {
        format!(
            "`story {path}` takes: {}",
            declared
                .iter()
                .map(|flag| format!("--{}", flag.name))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    format!(
        "unknown flag `{token}` for `story {path}`.\n\n{known}\nIf `{token}` was meant as text \
         rather than a flag, quote it as one argument, or put it after a `--`."
    )
}

pub fn parse_invocation(args: &[String]) -> Result<Invocation, AppError> {
    if args.is_empty() {
        return Ok(Invocation::Help);
    }

    if let Some(help) = verb_help_request(args) {
        return Ok(help);
    }

    reject_unknown_flags(args)?;

    dispatch(&strip_terminator(args))
}

/// Removes the first bare `--`, so no verb parser ever sees the terminator.
///
/// Only the first: a second `--` is data, which is what `git` does and what a
/// title made of dashes needs.
fn strip_terminator(args: &[String]) -> Vec<String> {
    match args.iter().position(|arg| arg == "--") {
        Some(at) => {
            let mut kept = args.to_vec();
            kept.remove(at);
            kept
        }
        None => args.to_vec(),
    }
}

/// Routes an invocation to its verb's parser. Pure: it inspects `args` and
/// builds an [`Invocation`], which is what lets [`verb_help_request`] use it
/// to ask whether a verb exists.
fn dispatch(args: &[String]) -> Result<Invocation, AppError> {
    match args[0].as_str() {
        "-h" | "--help" => Ok(Invocation::Help),
        "-V" | "--version" => Ok(Invocation::Version),
        "help" => parse_help(args),
        "update" => parse_update(args),
        // Not left to fall through to `unknown command`. Five years of
        // documents, this repo's own plugin skill, and every agent that has
        // ever seen storyhook all say `story init`; the least useful thing to
        // tell any of them is that no such command exists.
        "init" => Err(AppError::Usage(
            "`story init` is now `story project new`.\n\nThe project verbs moved into one \
             group: `story project new`, `story project list`, `story project delete`.\n\n  \
             story project new --prefix <PREFIX>"
                .to_string(),
        )),
        "project" => parse_project(args),
        "new" => parse_new(args),
        "member" => parse_member(args),
        "state" => parse_state(args),
        "list" => parse_list(args),
        "next" => parse_next(args),
        "summary" => Ok(Invocation::Summary),
        "report" => parse_report(args),
        "search" => parse_search(args),
        "import" => parse_import(args),
        "decompose" => parse_decompose(args),
        "import-project" => parse_import_project(args),
        "migrate" => parse_migrate(args),
        // Deleted rather than redirected-and-kept: `link checkout` is strictly
        // more capable. `relink` needed a pointer file in the directory it was
        // pointed at, which is precisely what a checkout that has been moved,
        // renamed or freshly cloned may not have; `link checkout` records the
        // path against a project named the ordinary way and asks the directory
        // for nothing.
        "relink" => Err(AppError::Usage(
            "`story relink` is now `story project link checkout`.\n\nIt no longer reads a \
             pointer file, so it works for a checkout that never had one:\n\n  story --project \
             <SLUG> project link checkout <PATH>"
                .to_string(),
        )),
        "export" => Ok(Invocation::Export),
        "load-context" | "context" => parse_context(args),
        "phase" => parse_phase(args),
        "type" => parse_type(args),
        "epic" => parse_epic(args),
        "handoff" => parse_handoff(args),
        "graph" => parse_graph(args),
        "doctor" => parse_doctor(args),
        "hooks" => parse_hooks(args),
        "scaffold" => parse_scaffold(args),
        "commit-sync" | "sync-git" => parse_commit_sync(args),
        "github-sync" => parse_github_sync(args),
        "plugin" => parse_plugin(args),
        "web" => parse_web(args),
        "daemon" => parse_daemon(args),
        "store" => parse_store(args),
        "show" => parse_show(args),
        "comment" => parse_comment(args),
        "assign" => parse_assign(args),
        "move" => parse_move(args),
        "block" => parse_block(args),
        "unblock" => parse_unblock(args),
        "prioritize" => parse_prioritize(args),
        "label" => parse_label(args),
        "unlabel" => parse_unlabel(args),
        "reopen" => parse_reopen_verb(args),
        "delete" => parse_delete_verb(args),
        "purge" => parse_purge_verb(args),
        "set" => parse_set(args),
        "relate" | "link" => parse_relate(args),
        "unrelate" | "unlink" => parse_unrelate(args),
        "session-start" => Ok(Invocation::SessionStart),
        _ => Err(AppError::Usage(format!(
            "unknown command `{}`. Run `story --help` for usage.",
            args[0]
        ))),
    }
}

const PROJECT_USAGE: &str = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                             [--attach <PATH> | --no-attach] [--no-agents-md] | delete \
                             [--force] | show | list | link origin [URL]|checkout [PATH] | \
                             unlink origin [URL]|checkout | settings list|get|set|unset";

const PROJECT_SHOW_USAGE: &str = "usage: story project show\n\n`story project show` takes no \
                                  argument. It reports the project this directory resolves \
                                  to — name a different one with `--project <slug>`.";

const PROJECT_DELETE_USAGE: &str = "usage: story project delete [--force]\n\n`story project \
                                    delete` takes no positional argument. It destroys the \
                                    project this directory resolves to; name a different one \
                                    with `--project <slug>`.";

const PROJECT_NEW_USAGE: &str = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                                 [--attach <PATH> | --no-attach] [--no-agents-md]\n\nRun with no \
                                 flags at a terminal to be asked. `story project new` takes no \
                                 positional argument: name the project with --name and the \
                                 checkout with --attach.";

const PROJECT_LINK_USAGE: &str = "usage: story project link origin [URL] | story project link checkout [PATH]\n\nThese attach \
     *git* associations to a project. They are unrelated to `story link`, which is an alias for \
     `story relate` and joins one story to another.";

const PROJECT_UNLINK_USAGE: &str = "usage: story project unlink origin [URL] | story project unlink checkout\n\n`unlink \
     checkout` takes no path: a project has at most one. These are unrelated to `story unlink`, \
     which is an alias for `story unrelate`.";

const PROJECT_SETTINGS_USAGE: &str = "usage: story project settings list | get <key> | \
                                      set <key> <value> | unset <key>";

fn parse_project(args: &[String]) -> Result<Invocation, AppError> {
    let action = args
        .get(1)
        .ok_or_else(|| AppError::Usage(PROJECT_USAGE.to_string()))?;
    match action.as_str() {
        "new" => parse_project_new(args),
        "delete" => parse_project_delete(args),
        // Redirects, never `unknown command`. Being told where a command went
        // is the whole difference from being told it never existed, and 34
        // files and five years of documents say `story project init`. Kept for
        // the life of the 2.x line, removed at 3.0.0, and listed as commands
        // nowhere — a redirect is a signpost, not a surface.
        //
        // Not aliases, deliberately. An alias would keep the drive-by creation
        // shape alive under a new name, and that shape is the thing being
        // retired: a positional nobody could tell from a name, and a prefix
        // minted silently into every id the project will ever have.
        "init" => Err(AppError::Usage(
            "`story project init` is now `story project new`.\n\nIt takes no path: name the \
             checkout with `--attach <PATH>`, or `--no-attach` for a project with no checkout \
             here. `--prefix` is required — it is minted into every story id and cannot be \
             changed afterwards.\n\n  story project new --prefix <PREFIX> [--name <NAME>] \
             [--attach <PATH> | --no-attach]\n\nRun it with no flags at a terminal to be asked."
                .to_string(),
        )),
        "deinit" => Err(AppError::Usage(
            "`story project deinit` is now `story project delete`.\n\nIt takes no path or slug: \
             it destroys the project this directory resolves to, or the one named by `--project \
             <slug>`. It no longer deletes `.storyhook.toml` or `AGENTS.md` from any checkout.\n\n\
             \x20 story project delete [--force]"
                .to_string(),
        )),
        "list" => Ok(Invocation::Project {
            action: ProjectAction::List,
        }),
        // Length-checked, unlike `list` above: a trailing word here would most
        // likely be somebody reaching for a target this verb deliberately does
        // not take, and swallowing it silently would answer about a different
        // project than the one they named.
        "show" if args.len() == 2 => Ok(Invocation::Project {
            action: ProjectAction::Show,
        }),
        "show" => Err(AppError::Usage(PROJECT_SHOW_USAGE.to_string())),
        "link" => parse_project_link(args),
        "unlink" => parse_project_unlink(args),
        "settings" => parse_project_settings(args),
        _ => Err(AppError::Usage(PROJECT_USAGE.to_string())),
    }
}

/// `story project link origin [URL]` / `story project link checkout [PATH]`.
///
/// The optional word is taken positionally rather than by flag because it *is*
/// the object of the verb; a flag would make `link origin` read as though the
/// URL were an option on some other operation.
fn parse_project_link(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_LINK_USAGE.to_string());
    if args.len() > 4 {
        return Err(usage());
    }
    let value = args.get(3).cloned();
    let target = match args.get(2).ok_or_else(usage)?.as_str() {
        "origin" => LinkTarget::Origin { url: value },
        "checkout" => LinkTarget::Checkout { path: value },
        _ => return Err(usage()),
    };
    Ok(Invocation::Project {
        action: ProjectAction::Link(target),
    })
}

/// `story project unlink origin [URL]` / `story project unlink checkout`.
fn parse_project_unlink(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_UNLINK_USAGE.to_string());
    let target = match args.get(2).ok_or_else(usage)?.as_str() {
        "origin" if args.len() <= 4 => UnlinkTarget::Origin {
            url: args.get(3).cloned(),
        },
        // A path here is not ignored: a caller who typed one believes a project
        // has several checkouts and is about to be surprised by which one went.
        "checkout" if args.len() == 3 => UnlinkTarget::Checkout,
        _ => return Err(usage()),
    };
    Ok(Invocation::Project {
        action: ProjectAction::Unlink(target),
    })
}

/// `story project settings <list|get|set|unset> …`.
///
/// Every form takes a fixed number of words and nothing else. A trailing word
/// is refused rather than ignored: `settings set a b c` is more likely a
/// quoting mistake than a value the user meant to lose half of.
fn parse_project_settings(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_SETTINGS_USAGE.to_string());
    let word = |index: usize| args.get(index).cloned().ok_or_else(usage);

    let action = match args.get(2).ok_or_else(usage)?.as_str() {
        "list" if args.len() == 3 => SettingsAction::List,
        "get" if args.len() == 4 => SettingsAction::Get { key: word(3)? },
        "set" if args.len() == 5 => SettingsAction::Set {
            key: word(3)?,
            value: word(4)?,
        },
        "unset" if args.len() == 4 => SettingsAction::Unset { key: word(3)? },
        _ => return Err(usage()),
    };

    Ok(Invocation::Project {
        action: ProjectAction::Settings(action),
    })
}

/// `story project delete [--force]`.
///
/// **No positional.** A bare word here would be a fourth way of naming a
/// project, and the one `deinit` had was the ambiguous kind: it resolved a slug
/// that happened to match a directory name as the directory. The refusal names
/// `--project` instead of guessing.
fn parse_project_delete(args: &[String]) -> Result<Invocation, AppError> {
    let mut force = false;

    for arg in &args[2..] {
        match arg.as_str() {
            "--force" | "-f" => force = true,
            _ => return Err(AppError::Usage(PROJECT_DELETE_USAGE.to_string())),
        }
    }

    Ok(Invocation::Project {
        action: ProjectAction::Delete { force },
    })
}

/// `story project new [--prefix P] [--name N] [--attach PATH | --no-attach]
/// [--no-agents-md]`.
///
/// **No positional of any kind.** A bare word after `new` could be a name or a
/// path with equal plausibility, and `deinit` already demonstrates what happens
/// when a parser has to guess: it is refused, naming both flags, rather than
/// resolved by a rule the user would have to know.
///
/// The absence of *every* switch is the interactive request. Only when at least
/// one is present does `--prefix` become required, because at that point
/// nobody is going to be asked for it and the alternative is minting `SH-1` in
/// a project the user never named `SH`.
fn parse_project_new(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_NEW_USAGE.to_string());
    let mut attach: Option<String> = None;
    let mut no_attach = false;
    let mut prefix: Option<String> = None;
    let mut name = None;
    let mut no_agents_md = false;
    let mut index = 2;
    // The first switch seen, kept so a refusal can name what made this
    // invocation non-interactive. "Pass --prefix" is advice; "you passed
    // --name, so I will not ask" is a diagnosis.
    let mut trigger: Option<String> = None;

    let value = |index: usize, flag: &str| {
        args.get(index + 1)
            .cloned()
            .ok_or_else(|| AppError::Usage(format!("{flag} requires a value")))
    };

    while index < args.len() {
        let word = args[index].as_str();
        if trigger.is_none() {
            trigger = Some(word.to_string());
        }
        match word {
            "--attach" => {
                attach = Some(value(index, "--attach")?);
                index += 2;
            }
            "--no-attach" => {
                no_attach = true;
                index += 1;
            }
            "--prefix" => {
                prefix = Some(value(index, "--prefix")?);
                index += 2;
            }
            "--name" => {
                name = Some(value(index, "--name")?);
                index += 2;
            }
            "--no-agents-md" => {
                no_agents_md = true;
                index += 1;
            }
            _ => return Err(usage()),
        }
    }

    // Nothing was said, so nothing is assumed. The client decides whether it
    // can ask; a request that reaches the dispatcher still carrying this is
    // refused there rather than defaulted.
    if args.len() == 2 {
        return Ok(Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Ask),
        });
    }

    let attach = match (attach, no_attach) {
        (Some(_), true) => {
            return Err(AppError::Usage(
                "`--attach` and `--no-attach` contradict each other: pass one or neither."
                    .to_string(),
            ));
        }
        (Some(path), false) => Attach::Path(path),
        (None, true) => Attach::Nothing,
        (None, false) => Attach::Cwd,
    };
    let prefix = prefix.ok_or_else(|| {
        AppError::Usage(format!(
            "`story project new` needs `--prefix <PREFIX>`. You passed `{}`, and any switch \
             means this invocation is being driven by a script rather than a person, so nothing \
             will be asked. A prefix is minted into every story id this project ever creates and \
             cannot be changed afterwards, so it is not defaulted.\n\n{PROJECT_NEW_USAGE}",
            trigger.as_deref().unwrap_or("a switch"),
        ))
    })?;
    let prefix = crate::domain::prefix::validate(&prefix)?;

    Ok(Invocation::Project {
        action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
            attach,
            prefix,
            name,
            no_agents_md,
        })),
    })
}

fn parse_new(args: &[String]) -> Result<Invocation, AppError> {
    let mut state = None;
    let mut story_type = None;
    let mut description = None;
    let mut priority = None;
    let mut assignee = None;
    let mut labels: Vec<String> = Vec::new();
    let mut title_parts = Vec::new();
    let mut index = 1;
    let usage = "usage: story new <title> [--state <slug>] [--type <slug>] [--description <text>] [--priority <level>] [--assignee <member>] [--label <name> ...] [--labels <csv>]";
    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            "--description" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                description = Some(value.clone());
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--assignee" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                assignee = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels.push(value.clone());
                index += 2;
            }
            "--labels" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels.push(value.clone());
                index += 2;
            }
            _ => {
                title_parts.push(args[index].clone());
                index += 1;
            }
        }
    }
    if title_parts.is_empty() {
        return Err(AppError::Usage(usage.to_string()));
    }
    // `--label` and `--labels` are the same delimiter-splitting sink, whether
    // repeated (`--label a --label b`) or comma-joined (`--labels a,b`) or a
    // single value that itself carries a comma (`--label "a,b"`, the SH-164
    // repro) — every raw value collected above is split on `,` here rather
    // than only the ones that arrived via `--labels`.
    let labels = normalize_labels(labels);
    Ok(Invocation::New {
        title: title_parts.join(" "),
        state,
        story_type,
        description,
        priority,
        labels: if labels.is_empty() {
            None
        } else {
            Some(labels)
        },
        assignee,
    })
}

fn parse_member(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 || args[1] != "add" {
        return Err(AppError::Usage(
            "usage: story member add \"<name <email>>\" | story member add -g <github-handle>"
                .to_string(),
        ));
    }

    if args[2] == "-g" || args[2] == "--github" {
        let handle = args
            .get(3)
            .ok_or_else(|| {
                AppError::Usage("usage: story member add -g <github-handle>".to_string())
            })?
            .clone();
        return Ok(Invocation::MemberAdd {
            input: MemberInput::Github(handle),
        });
    }

    Ok(Invocation::MemberAdd {
        input: MemberInput::Identity(join_tokens(&args[2..])),
    })
}

const TYPE_USAGE: &str = "usage: story type list | story type add <slug> [...] | story type set <slug> [...] | story type remove <slug>";
const TYPE_ADD_USAGE: &str =
    "usage: story type add <slug> [--description \"<text>\"] [--emoji <glyph>]";
const TYPE_SET_USAGE: &str = "usage: story type set <slug> [--description \"<text>\"] [--no-description] [--emoji <glyph>] [--no-emoji]";
const TYPE_REMOVE_USAGE: &str = "usage: story type remove <slug>";

const STATE_USAGE: &str = "usage: story state list | story state add <slug> --super OPEN|CLOSED | story state set <slug> [...] | story state remove <slug> | story state reorder <slug,...>";
const STATE_ADD_USAGE: &str =
    "usage: story state add <slug> --super OPEN|CLOSED [--role active] [--description \"<text>\"]";
const STATE_SET_USAGE: &str = "usage: story state set <slug> [--super OPEN|CLOSED] [--role active|none] [--description \"<text>\"] [--no-description] [--move-stories-to <slug>]";
const STATE_REMOVE_USAGE: &str = "usage: story state remove <slug> [--move-stories-to <slug>]";
const STATE_REORDER_USAGE: &str = "usage: story state reorder <slug,slug,...>";

/// Splits `--flag value` / `--flag=value` / `--flag` into (name, value)
/// pairs. A value that itself starts with `--` is read as the next flag, so
/// a missing value is reported rather than silently swallowing the flag that
/// followed it; pass `--flag=--value` when a value really does start with
/// dashes.
fn parse_dash_flags(
    args: &[String],
    usage: &str,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    let mut flags = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let Some(rest) = args[index].strip_prefix("--") else {
            return Err(AppError::Usage(usage.to_string()));
        };
        if let Some((name, value)) = rest.split_once('=') {
            flags.push((name.to_string(), Some(value.to_string())));
            index += 1;
        } else {
            let value = args
                .get(index + 1)
                .filter(|next| !next.starts_with("--"))
                .cloned();
            index += if value.is_some() { 2 } else { 1 };
            flags.push((rest.to_string(), value));
        }
    }
    Ok(flags)
}

/// The value of a flag that requires one.
fn flag_value(value: Option<String>, flag: &str, usage: &str) -> Result<String, AppError> {
    value.ok_or_else(|| AppError::Usage(format!("--{flag} needs a value\n{usage}")))
}

fn parse_state(args: &[String]) -> Result<Invocation, AppError> {
    let subcommand = args
        .get(1)
        .ok_or_else(|| AppError::Usage(STATE_USAGE.to_string()))?;

    let action = match subcommand.as_str() {
        "list" => StateAction::List,

        "add" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_ADD_USAGE.to_string()))?;
            let mut superstate = None;
            let mut role = None;
            let mut description = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_ADD_USAGE)? {
                match flag.as_str() {
                    "super" => superstate = Some(flag_value(value, "super", STATE_ADD_USAGE)?),
                    "role" => role = Some(flag_value(value, "role", STATE_ADD_USAGE)?),
                    "description" => {
                        description = Some(flag_value(value, "description", STATE_ADD_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_ADD_USAGE.to_string())),
                }
            }
            StateAction::Add {
                slug,
                superstate: superstate
                    .ok_or_else(|| AppError::Usage(STATE_ADD_USAGE.to_string()))?,
                role,
                description,
            }
        }

        "set" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_SET_USAGE.to_string()))?;
            let mut superstate = None;
            let mut role = None;
            let mut description = None;
            let mut clear_description = false;
            let mut move_stories_to = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_SET_USAGE)? {
                match flag.as_str() {
                    "super" => superstate = Some(flag_value(value, "super", STATE_SET_USAGE)?),
                    "role" => role = Some(flag_value(value, "role", STATE_SET_USAGE)?),
                    "description" => {
                        description = Some(flag_value(value, "description", STATE_SET_USAGE)?)
                    }
                    "no-description" => clear_description = true,
                    "move-stories-to" => {
                        move_stories_to =
                            Some(flag_value(value, "move-stories-to", STATE_SET_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_SET_USAGE.to_string())),
                }
            }
            if description.is_some() && clear_description {
                return Err(AppError::Usage(
                    "--description and --no-description contradict each other".to_string(),
                ));
            }
            StateAction::Set {
                slug,
                superstate,
                role,
                description,
                clear_description,
                move_stories_to,
            }
        }

        "remove" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_REMOVE_USAGE.to_string()))?;
            let mut move_stories_to = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_REMOVE_USAGE)? {
                match flag.as_str() {
                    "move-stories-to" => {
                        move_stories_to =
                            Some(flag_value(value, "move-stories-to", STATE_REMOVE_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_REMOVE_USAGE.to_string())),
                }
            }
            StateAction::Remove {
                slug,
                move_stories_to,
            }
        }

        // Accepts both `reorder a,b,c` and `reorder a b c`, so the order can
        // be pasted from `story state list` output either way.
        "reorder" => {
            let order: Vec<String> = args[1..]
                .iter()
                .skip(1)
                .flat_map(|arg| arg.split(','))
                .map(str::trim)
                .filter(|slug| !slug.is_empty())
                .map(str::to_string)
                .collect();
            if order.is_empty() {
                return Err(AppError::Usage(STATE_REORDER_USAGE.to_string()));
            }
            StateAction::Reorder { order }
        }

        _ => return Err(AppError::Usage(STATE_USAGE.to_string())),
    };

    Ok(Invocation::State { action })
}

fn parse_list(args: &[String]) -> Result<Invocation, AppError> {
    let mut state = None;
    let mut assignee = None;
    let mut flagged = false;
    let mut priority = None;
    let mut label = None;
    let mut created_after = None;
    let mut updated_after = None;
    let mut blocked = false;
    let mut ready = false;
    let mut stale = None;
    let mut phase = None;
    let mut story_type = None;
    let mut index = 1;
    let usage = "usage: story list [--state <slug>] [--assignee <id>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>]";

    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--assignee" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                assignee = Some(value.clone());
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                label = Some(value.clone());
                index += 2;
            }
            "--created-after" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                created_after = Some(value.clone());
                index += 2;
            }
            "--updated-after" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                updated_after = Some(value.clone());
                index += 2;
            }
            "--flagged" => {
                flagged = true;
                index += 1;
            }
            "--blocked" => {
                blocked = true;
                index += 1;
            }
            "--ready" => {
                ready = true;
                index += 1;
            }
            "--stale" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                stale = Some(value.clone());
                index += 2;
            }
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                phase = Some(value.clone());
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }

    Ok(Invocation::List {
        state,
        assignee,
        flagged,
        priority,
        label,
        created_after,
        updated_after,
        blocked,
        ready,
        stale,
        phase,
        story_type,
    })
}

fn parse_next(args: &[String]) -> Result<Invocation, AppError> {
    let mut count = 1;
    let mut phase = None;
    let mut index = 1;
    let usage = "usage: story next [--count <n>] [--phase <N>]";

    while index < args.len() {
        match args[index].as_str() {
            "--count" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                count = value.parse::<usize>().map_err(|_| {
                    AppError::Usage("--count must be a positive integer".to_string())
                })?;
                if count == 0 {
                    return Err(AppError::Usage(
                        "--count must be a positive integer".to_string(),
                    ));
                }
                index += 2;
            }
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                phase = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }

    Ok(Invocation::Next { count, phase })
}

fn parse_report(args: &[String]) -> Result<Invocation, AppError> {
    let mut html = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--html" => {
                html = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage("usage: story report [--html]".to_string()));
            }
        }
    }
    Ok(Invocation::Report { html })
}

fn parse_search(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage("usage: story search <query>".to_string()));
    }
    Ok(Invocation::Search {
        query: join_tokens(&args[1..]),
    })
}

fn parse_import(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() > 2 {
        return Err(AppError::Usage("usage: story import [<file>]".to_string()));
    }
    let file = args.get(1).cloned();
    Ok(Invocation::Import { file })
}

fn parse_decompose(args: &[String]) -> Result<Invocation, AppError> {
    let mut file = None;
    let mut stdin = false;
    let mut dry_run = false;
    let mut index = 1;
    let usage = "usage: story decompose <file> [--dry-run] | story decompose --stdin [--dry-run]";

    while index < args.len() {
        match args[index].as_str() {
            "--stdin" => {
                stdin = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            _ if file.is_none() && !args[index].starts_with("--") => {
                file = Some(args[index].clone());
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }

    if file.is_none() && !stdin {
        return Err(AppError::Usage(usage.to_string()));
    }

    Ok(Invocation::Decompose {
        file,
        stdin,
        dry_run,
    })
}

fn parse_import_project(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            "usage: story import-project <file>".to_string(),
        ));
    }
    Ok(Invocation::ImportProject {
        file: args[1].clone(),
    })
}

/// `story migrate [<path>] [--dry-run]`.
///
/// The positional argument is optional because the common case is running it
/// from inside the repository being migrated; when it is absent the invocation
/// carries `None` and the dispatcher walks up from the working directory.
fn parse_migrate(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story migrate [<path>] [--dry-run]";
    let mut path = None;
    let mut dry_run = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            value if !value.starts_with('-') && path.is_none() => {
                path = Some(value.to_string());
                index += 1;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::Migrate { path, dry_run })
}

fn parse_context(args: &[String]) -> Result<Invocation, AppError> {
    let mut format = None;
    let mut index = 1;
    let usage = "usage: story load-context [--format markdown|json]";
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                format = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    Ok(Invocation::Context { format })
}

fn validate_phase_number(s: &str) -> Result<(), AppError> {
    s.parse::<u32>()
        .map_err(|_| AppError::Validation(format!("phase must be a positive integer, got `{s}`")))
        .and_then(|n| {
            if n == 0 {
                Err(AppError::Validation("phase must be >= 1".to_string()))
            } else {
                Ok(())
            }
        })
}

fn parse_phase(args: &[String]) -> Result<Invocation, AppError> {
    let usage =
        "usage: story phase list|show <N>|add <id> <N>|remove <id>|create <N> [\"<title>\"]";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match args[1].as_str() {
        "list" => Ok(Invocation::Phase {
            action: PhaseAction::List,
        }),
        "show" => {
            let phase = args
                .get(2)
                .ok_or_else(|| AppError::Usage("usage: story phase show <N>".to_string()))?
                .clone();
            validate_phase_number(&phase)?;
            Ok(Invocation::Phase {
                action: PhaseAction::Show { phase },
            })
        }
        "add" => {
            if args.len() < 4 {
                return Err(AppError::Usage(
                    "usage: story phase add <id> <N>".to_string(),
                ));
            }
            validate_phase_number(&args[3])?;
            Ok(Invocation::Phase {
                action: PhaseAction::Add {
                    id: args[2].clone(),
                    phase: args[3].clone(),
                },
            })
        }
        "remove" => {
            let id = args
                .get(2)
                .ok_or_else(|| AppError::Usage("usage: story phase remove <id>".to_string()))?
                .clone();
            Ok(Invocation::Phase {
                action: PhaseAction::Remove { id },
            })
        }
        "create" => {
            let phase = args
                .get(2)
                .ok_or_else(|| {
                    AppError::Usage("usage: story phase create <N> [\"<title>\"]".to_string())
                })?
                .clone();
            validate_phase_number(&phase)?;
            let title = if args.len() > 3 {
                Some(args[3..].join(" "))
            } else {
                None
            };
            Ok(Invocation::Phase {
                action: PhaseAction::Create { phase, title },
            })
        }
        _ => Err(AppError::Usage(usage.to_string())),
    }
}

fn parse_type(args: &[String]) -> Result<Invocation, AppError> {
    let subcommand = args
        .get(1)
        .ok_or_else(|| AppError::Usage(TYPE_USAGE.to_string()))?;

    let action = match subcommand.as_str() {
        "list" => TypeAction::List,

        "add" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_ADD_USAGE.to_string()))?;
            let mut description = None;
            let mut emoji = None;
            for (flag, value) in parse_dash_flags(&args[3..], TYPE_ADD_USAGE)? {
                match flag.as_str() {
                    "description" => {
                        description = Some(flag_value(value, "description", TYPE_ADD_USAGE)?)
                    }
                    "emoji" => emoji = Some(flag_value(value, "emoji", TYPE_ADD_USAGE)?),
                    _ => return Err(AppError::Usage(TYPE_ADD_USAGE.to_string())),
                }
            }
            TypeAction::Add {
                slug,
                description,
                emoji,
            }
        }

        "set" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_SET_USAGE.to_string()))?;
            let mut description = None;
            let mut clear_description = false;
            let mut emoji = None;
            let mut clear_emoji = false;
            for (flag, value) in parse_dash_flags(&args[3..], TYPE_SET_USAGE)? {
                match flag.as_str() {
                    "description" => {
                        description = Some(flag_value(value, "description", TYPE_SET_USAGE)?)
                    }
                    "no-description" => clear_description = true,
                    "emoji" => emoji = Some(flag_value(value, "emoji", TYPE_SET_USAGE)?),
                    "no-emoji" => clear_emoji = true,
                    _ => return Err(AppError::Usage(TYPE_SET_USAGE.to_string())),
                }
            }
            if description.is_some() && clear_description {
                return Err(AppError::Usage(
                    "--description and --no-description contradict each other".to_string(),
                ));
            }
            if emoji.is_some() && clear_emoji {
                return Err(AppError::Usage(
                    "--emoji and --no-emoji contradict each other".to_string(),
                ));
            }
            TypeAction::Set {
                slug,
                description,
                clear_description,
                emoji,
                clear_emoji,
            }
        }

        "remove" => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_REMOVE_USAGE.to_string()))?;
            TypeAction::Remove { slug }
        }

        _ => return Err(AppError::Usage(TYPE_USAGE.to_string())),
    };

    Ok(Invocation::Type { action })
}

fn parse_epic(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story epic list|show <id>|create \"<title>\"|add <epic-id> <story-id>";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match args[1].as_str() {
        "list" => Ok(Invocation::Epic {
            action: EpicAction::List,
        }),
        "show" => {
            let id = args
                .get(2)
                .ok_or_else(|| AppError::Usage("usage: story epic show <id>".to_string()))?
                .clone();
            Ok(Invocation::Epic {
                action: EpicAction::Show { id },
            })
        }
        "create" => {
            if args.len() < 3 {
                return Err(AppError::Usage(
                    "usage: story epic create \"<title>\"".to_string(),
                ));
            }
            let title = join_tokens(&args[2..]);
            if title.is_empty() {
                return Err(AppError::Usage(
                    "usage: story epic create \"<title>\"".to_string(),
                ));
            }
            Ok(Invocation::Epic {
                action: EpicAction::Create { title },
            })
        }
        "add" => {
            if args.len() < 4 {
                return Err(AppError::Usage(
                    "usage: story epic add <epic-id> <story-id>".to_string(),
                ));
            }
            Ok(Invocation::Epic {
                action: EpicAction::Add {
                    epic_id: args[2].clone(),
                    story_id: args[3].clone(),
                },
            })
        }
        _ => Err(AppError::Usage(usage.to_string())),
    }
}

fn parse_handoff(args: &[String]) -> Result<Invocation, AppError> {
    let mut since = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--since" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage("usage: story handoff [--since <duration>]".to_string())
                })?;
                since = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(
                    "usage: story handoff [--since <duration>]".to_string(),
                ));
            }
        }
    }
    Ok(Invocation::Handoff { since })
}

fn parse_graph(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() == 1 {
        return Ok(Invocation::Graph {
            mode: GraphMode::Overview,
        });
    }
    match args[1].as_str() {
        "--critical-path" => Ok(Invocation::Graph {
            mode: GraphMode::CriticalPath,
        }),
        "--blocked-by" => {
            let id = args.get(2).ok_or_else(|| {
                AppError::Usage("usage: story graph --blocked-by <id>".to_string())
            })?;
            Ok(Invocation::Graph {
                mode: GraphMode::BlockedBy(id.clone()),
            })
        }
        "--parallel-groups" => Ok(Invocation::Graph {
            mode: GraphMode::ParallelGroups,
        }),
        _ => Err(AppError::Usage(
            "usage: story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]"
                .to_string(),
        )),
    }
}

fn parse_doctor(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() == 1 {
        return Ok(Invocation::Doctor { fix: false });
    }

    if args.len() == 2 && args[1] == "--fix" {
        return Ok(Invocation::Doctor { fix: true });
    }

    Err(AppError::Usage("usage: story doctor [--fix]".to_string()))
}

fn parse_update(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story update [--check] [--force]";
    let mut check = false;
    let mut force = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--check" => {
                check = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    if check && force {
        return Err(AppError::Usage(format!(
            "{usage} (--check and --force are mutually exclusive)"
        )));
    }
    Ok(Invocation::Update { check, force })
}

fn parse_hooks(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage(
            "usage: story hooks install|uninstall|list|test <event_type>".to_string(),
        ));
    }
    match args[1].as_str() {
        "install" => Ok(Invocation::Hooks {
            action: HooksAction::Install,
        }),
        "uninstall" => Ok(Invocation::Hooks {
            action: HooksAction::Uninstall,
        }),
        "list" => Ok(Invocation::Hooks {
            action: HooksAction::List,
        }),
        "test" => {
            let event_type = args.get(2).ok_or_else(|| {
                AppError::Usage("usage: story hooks test <event_type>".to_string())
            })?;
            Ok(Invocation::Hooks {
                action: HooksAction::Test {
                    event_type: event_type.clone(),
                },
            })
        }
        other => Err(AppError::Usage(format!("unknown hooks action: {other}"))),
    }
}

fn parse_scaffold(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            "usage: story scaffold agents-md|claude-md|cursor-rules".to_string(),
        ));
    }
    let kind = args[1].clone();
    if kind != "agents-md" && kind != "claude-md" && kind != "cursor-rules" {
        return Err(AppError::Usage(
            "usage: story scaffold agents-md|claude-md|cursor-rules".to_string(),
        ));
    }
    Ok(Invocation::Scaffold { kind })
}

fn parse_commit_sync(args: &[String]) -> Result<Invocation, AppError> {
    let mut since = None;
    let mut index = 1;
    let usage = "usage: story commit-sync [--since <duration>]";
    while index < args.len() {
        match args[index].as_str() {
            "--since" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                since = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    Ok(Invocation::CommitSync { since })
}

fn parse_github_sync(args: &[String]) -> Result<Invocation, AppError> {
    let mut id = None;
    let mut dry_run = false;
    let mut resolve = None;
    let mut strategy = None;
    let mut mode = None;
    let mut index = 1;
    let usage = "usage: story github-sync [<id>] [--dry-run] [--resolve local|remote] \
                 [--strategy import-all|match-titles|push-only|future-only] \
                 [--mode manual|off]";
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--resolve" => {
                let side = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(format!("--resolve needs `local` or `remote`\n{usage}"))
                })?;
                resolve = Some(match side.as_str() {
                    "local" => ConflictSide::Local,
                    "remote" => ConflictSide::Remote,
                    other => {
                        return Err(AppError::Usage(format!(
                            "--resolve takes `local` or `remote`, not `{other}`\n{usage}"
                        )));
                    }
                });
                index += 2;
            }
            "--strategy" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(format!(
                        "--strategy needs import-all, match-titles, push-only or future-only\n{usage}"
                    ))
                })?;
                strategy = Some(match value.as_str() {
                    "import-all" => SetupStrategy::ImportAll,
                    "match-titles" => SetupStrategy::MatchTitles,
                    "push-only" => SetupStrategy::PushOnly,
                    "future-only" => SetupStrategy::FutureOnly,
                    other => {
                        return Err(AppError::Usage(format!(
                            "--strategy takes import-all, match-titles, push-only or \
                             future-only, not `{other}`\n{usage}"
                        )));
                    }
                });
                index += 2;
            }
            "--mode" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(format!("--mode needs `manual` or `off`\n{usage}"))
                })?;
                mode = Some(match value.as_str() {
                    "manual" => SetupMode::Manual,
                    "off" => SetupMode::Off,
                    "auto" => {
                        return Err(AppError::Usage(format!(
                            "--mode auto is not implemented -- storyhook never syncs on its \
                             own (SH-68)\n{usage}"
                        )));
                    }
                    other => {
                        return Err(AppError::Usage(format!(
                            "--mode takes `manual` or `off`, not `{other}`\n{usage}"
                        )));
                    }
                });
                index += 2;
            }
            arg if looks_like_story_id(arg) => {
                id = Some(arg.to_string());
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    // `--resolve` without an `<id>`, `--strategy`/`--mode` given alone or on an
    // already-configured project: none of these are refused here. The rule
    // lives in `github::run_sync_with`, which is the one gate every door passes
    // through — this parser, the dashboard, the TUI and a hand-built
    // `InvokeRequest`. A copy here would be a second place for it to drift out
    // of.
    Ok(Invocation::GithubSync {
        id,
        dry_run,
        resolve,
        strategy,
        mode,
    })
}

fn parse_help(args: &[String]) -> Result<Invocation, AppError> {
    let flags: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| a.starts_with("--"))
        .map(|a| a.as_str())
        .collect();
    let positional: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .map(|a| a.as_str())
        .collect();

    let has_compact = flags.contains(&"--compact");
    let has_all = flags.contains(&"--all");

    // If both flags given, --compact wins (no crash)
    if has_compact {
        return Ok(Invocation::HelpCompact);
    }
    if has_all {
        return Ok(Invocation::HelpAll);
    }

    if let Some(topic) = positional.first() {
        return Ok(Invocation::HelpTopic {
            topic: topic.to_string(),
        });
    }

    Ok(Invocation::Help)
}

fn parse_plugin(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story plugin install|uninstall <target>".to_string(),
        ));
    }
    match args[1].as_str() {
        "install" => Ok(Invocation::Plugin {
            action: PluginAction::Install {
                target: args[2].clone(),
            },
        }),
        "uninstall" => Ok(Invocation::Plugin {
            action: PluginAction::Uninstall {
                target: args[2].clone(),
            },
        }),
        other => Err(AppError::Usage(format!(
            "unknown plugin action: {other}. Usage: story plugin install|uninstall <target>"
        ))),
    }
}

/// `story daemon start|stop|status|install|uninstall|--serve`.
fn parse_store(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story store new <path>";
    let action = match args.get(1).map(String::as_str) {
        Some("new") => match args.get(2) {
            Some(path) if !path.is_empty() && !path.starts_with('-') => {
                StoreAction::New { path: path.clone() }
            }
            _ => {
                return Err(AppError::Usage(format!(
                    "{usage}\n\n`store new` names the file to create, for example \
                     `story store new /tmp/scratch/store.db`."
                )));
            }
        },
        _ => return Err(AppError::Usage(usage.to_string())),
    };
    if args.len() > 3 {
        return Err(AppError::Usage(usage.to_string()));
    }
    Ok(Invocation::Store { action })
}

fn parse_daemon(args: &[String]) -> Result<Invocation, AppError> {
    let usage =
        "usage: story daemon start [--port <PORT>] | stop | status | install | uninstall | token";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let action = match args[1].as_str() {
        "start" => DaemonAction::Start {
            port: parse_port_flag(&args[2..], usage)?,
        },
        // Spelled as a flag rather than a subcommand because it is not one a
        // user runs: it is what the spawner execs, and what a launchd agent
        // runs, and both of those are storyhook talking to itself.
        "--serve" => DaemonAction::Serve {
            port: parse_port_flag(&args[2..], usage)?,
        },
        "stop" => DaemonAction::Stop,
        "status" => DaemonAction::Status,
        "install" => DaemonAction::Install,
        "uninstall" => DaemonAction::Uninstall,
        "token" => DaemonAction::Token,
        _ => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::Daemon { action })
}

/// An optional trailing `--port <PORT>`, refusing anything else.
///
/// Port 0 is accepted here and nowhere else in the CLI: it means "let the kernel
/// choose", which is exactly what a test harness wants and what a user never
/// does deliberately.
fn parse_port_flag(rest: &[String], usage: &str) -> Result<Option<u16>, AppError> {
    match rest {
        [] => Ok(None),
        [flag, value] if flag == "--port" => value
            .parse::<u16>()
            .map(Some)
            .map_err(|_| AppError::Usage(format!("invalid port: {value}"))),
        [flag] if flag == "--port" => Err(AppError::Usage("--port requires a value".to_string())),
        _ => Err(AppError::Usage(usage.to_string())),
    }
}

fn parse_web(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story web start [--port <PORT>] | stop | status | open | address";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }

    match args[1].as_str() {
        "start" => {
            let mut port: u16 = DEFAULT_WEB_PORT;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--port" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage("--port requires a value".to_string())
                        })?;
                        port = value
                            .parse::<u16>()
                            .map_err(|_| AppError::Usage(format!("invalid port: {value}")))?;
                        if port == 0 {
                            return Err(AppError::Usage("invalid port: 0".to_string()));
                        }
                        index += 2;
                    }
                    _ => return Err(AppError::Usage(usage.to_string())),
                }
            }
            Ok(Invocation::Web {
                action: WebAction::Start { port },
            })
        }
        "stop" => Ok(Invocation::Web {
            action: WebAction::Stop,
        }),
        "status" => Ok(Invocation::Web {
            action: WebAction::Status,
        }),
        "open" => Ok(Invocation::Web {
            action: WebAction::Open,
        }),
        "address" => Ok(Invocation::Web {
            action: WebAction::Address,
        }),
        "--serve" => {
            // Internal: story web --serve --port N
            let mut port: u16 = DEFAULT_WEB_PORT;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--port" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage("--port requires a value".to_string())
                        })?;
                        port = value
                            .parse::<u16>()
                            .map_err(|_| AppError::Usage(format!("invalid port: {value}")))?;
                        index += 2;
                    }
                    _ => return Err(AppError::Usage(usage.to_string())),
                }
            }
            Ok(Invocation::Web {
                action: WebAction::Serve { port },
            })
        }
        _ => Err(AppError::Usage(usage.to_string())),
    }
}

/// Whether `s` is *shaped* like a story id: `PREFIX-DIGITS` (`SH-1`, `API-42`)
/// or a bare number (`5`).
///
/// Shape only, and that is all it can be. Which project a prefix belongs to,
/// and whether a number names a story that exists, need the selected project's
/// prefix and the store — neither of which a parser has. Those are
/// [`StoryRef::classify`](crate::store::StoryRef)'s questions, asked once the
/// project is determined.
///
/// One caller: [`parse_github_sync`], which is the only parser that has to tell
/// an id positional from a typo'd flag. It is deliberately *looser* than the id
/// grammar — `007` and `0` pass here — because every other verb takes its
/// positional unjudged and reports `story `007` not found` from the store. A
/// parser that were stricter would make one verb answer a usage error where the
/// rest answer not-found, which is exactly the inconsistency SH-118 found: this
/// predicate required a hyphen, so `story github-sync 1` exited 2 with a usage
/// line while `story show 1` exited 3.
fn looks_like_story_id(s: &str) -> bool {
    if let Some(pos) = s.find('-') {
        let prefix = &s[..pos];
        let suffix = &s[pos + 1..];
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphanumeric())
            && !suffix.is_empty()
            && suffix.chars().all(|c| c.is_ascii_digit())
    } else {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
    }
}

fn parse_show(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage("usage: story show <id>".to_string()));
    }
    Ok(Invocation::Show {
        id: args[1].clone(),
    })
}

fn parse_comment(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story comment <id> \"<text>\"".to_string(),
        ));
    }
    Ok(Invocation::Comment {
        id: args[1].clone(),
        text: join_tokens(&args[2..]),
    })
}

fn parse_assign(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story assign <id> <member>".to_string(),
        ));
    }
    Ok(Invocation::Assign {
        id: args[1].clone(),
        member: join_tokens(&args[2..]),
    })
}

fn parse_move(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story move <id> <state> [--if-state <expected>] [\"<comment>\"]";
    if args.len() < 3 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let id = args[1].clone();
    let state = args[2].clone();

    // `--if-state` is recognized only as the literal token immediately
    // following <state> (args[3]) — never scanned for anywhere else in the
    // trailing args. What that protects against is an unrelated `--if-state`
    // substring inside an unquoted multi-word comment being silently spliced
    // out mid-comment and mistaken for a real CAS guard. That reasoning is
    // unchanged and this position-pinning stays.
    //
    // What *has* changed (SH-62) is the sentence this comment used to carry
    // next: that a comment beginning with `--` must never fail as an
    // unrecognized flag, because comments have always been unrestricted free
    // text. The council that settled SH-62 overturned that in the letter and
    // kept it in the spirit, and the distinction is worth stating here because
    // this is where a reader will come looking.
    //
    // A flag-shaped token is now refused ahead of every parser — but *shape*,
    // not prefix: a token containing whitespace is never flag-shaped. So a real
    // comment (`story move SH-1 done "--sprint-23 wrapped"`) is one argv
    // element with spaces in it and is still unrestricted free text, exactly as
    // before. Only the unquoted single-token form (`… done --sprint-23`) now
    // errors, and it does so naming the token and offering `--`. A repo-wide
    // search for that form found one hit: SH-62's own defect description.
    let (if_state, comment_start) = if args.get(3).map(String::as_str) == Some("--if-state") {
        let value = args
            .get(4)
            .ok_or_else(|| AppError::Usage("--if-state requires a value".to_string()))?;
        (Some(value.clone()), 5)
    } else {
        (None, 3)
    };

    let comment = if comment_start < args.len() {
        Some(join_tokens(&args[comment_start..]))
    } else {
        None
    };

    Ok(Invocation::SetState {
        id,
        state,
        comment,
        if_state,
    })
}

fn parse_block(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story block <id> \"<reason>\"".to_string(),
        ));
    }
    Ok(Invocation::SetAwaiting {
        id: args[1].clone(),
        awaiting: join_tokens(&args[2..]),
    })
}

fn parse_unblock(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage("usage: story unblock <id>".to_string()));
    }
    Ok(Invocation::ClearAwaiting {
        id: args[1].clone(),
    })
}

fn parse_prioritize(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            "usage: story prioritize <id> <level>".to_string(),
        ));
    }
    Ok(Invocation::SetPriority {
        id: args[1].clone(),
        priority: args[2].clone(),
    })
}

fn parse_label(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            "usage: story label <id> <labels-csv>".to_string(),
        ));
    }
    let add = normalize_labels([&args[2]]);
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add,
        remove: Vec::new(),
    })
}

fn parse_unlabel(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            "usage: story unlabel <id> <labels-csv>".to_string(),
        ));
    }
    let remove = normalize_labels([&args[2]]);
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add: Vec::new(),
        remove,
    })
}

fn parse_reopen_verb(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story reopen <id> [--force]";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let id = args[1].clone();
    let mut force = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--force" => force = true,
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::Reopen { id, force })
}

/// `story purge <id> [--force]`.
///
/// Shaped like `reopen`'s parser rather than `delete`'s: the trailing words are
/// not free text, so an unexpected one is a mistake rather than a reason.
fn parse_purge_verb(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story purge <id> [--force]";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let id = args[1].clone();
    let mut force = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--force" => force = true,
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::Purge { id, force })
}

fn parse_delete_verb(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story delete <id> \"<reason>\"".to_string(),
        ));
    }
    let reason = join_tokens(&args[2..]);
    if reason.is_empty() {
        return Err(AppError::Usage(
            "usage: story delete <id> \"<reason>\"".to_string(),
        ));
    }
    Ok(Invocation::Delete {
        id: args[1].clone(),
        reason,
    })
}

fn parse_relate(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 4 {
        return Err(AppError::Usage(
            "usage: story relate <a> <relationship-type> <b>".to_string(),
        ));
    }
    Ok(Invocation::Relate {
        a: args[1].clone(),
        relation: args[2].clone(),
        b: args[3].clone(),
        remove: false,
    })
}

fn parse_unrelate(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 4 {
        return Err(AppError::Usage(
            "usage: story unrelate <a> <relationship-type> <b>".to_string(),
        ));
    }
    Ok(Invocation::Relate {
        a: args[1].clone(),
        relation: args[2].clone(),
        b: args[3].clone(),
        remove: true,
    })
}

fn parse_set(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story set <id> [--field value ...]".to_string(),
        ));
    }
    let id = args[1].clone();
    let mut title = None;
    let mut state = None;
    let mut priority = None;
    let mut assignee = None;
    let mut labels = None;
    let mut blocked = None;
    let mut unblocked = false;
    let mut json = None;
    let mut story_type = None;
    let mut description = None;
    let mut index = 2;
    let usage = "usage: story set <id> [--title \"<title>\"] [--state <slug>] [--priority <level>] [--assignee <member>] [--labels \"<csv>\"] [--blocked \"<reason>\"] [--unblocked] [--json \"<json>\"] [--type <slug>] [--description \"<text>\"]";

    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                title = Some(value.clone());
                index += 2;
            }
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--assignee" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                assignee = Some(value.clone());
                index += 2;
            }
            "--labels" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels = Some(value.clone());
                index += 2;
            }
            "--blocked" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                blocked = Some(value.clone());
                index += 2;
            }
            "--unblocked" => {
                unblocked = true;
                index += 1;
            }
            "--json" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                json = Some(value.clone());
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            "--description" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                description = Some(value.clone());
                index += 2;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }

    if title.is_none()
        && state.is_none()
        && priority.is_none()
        && assignee.is_none()
        && labels.is_none()
        && blocked.is_none()
        && !unblocked
        && json.is_none()
        && story_type.is_none()
        && description.is_none()
    {
        return Err(AppError::Usage(
            "no fields specified. Usage: story set <id> --<field> <value> ...".to_string(),
        ));
    }

    Ok(Invocation::SetFields {
        id,
        title,
        state,
        priority,
        assignee,
        labels,
        blocked,
        unblocked,
        json,
        story_type,
        description,
    })
}

fn join_tokens(tokens: &[String]) -> String {
    tokens.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{EpicAction, Invocation, TypeAction, looks_like_story_id, parse_invocation};

    /// The defect SH-118 left behind: `github-sync` was the one verb whose id
    /// positional had to contain a hyphen, so a bare integer fell through to
    /// the "unknown token" arm and became a *usage* error — exit 2 — while
    /// every other verb accepted the token and answered from the store.
    #[test]
    fn github_sync_takes_a_bare_integer_as_its_id() {
        let invocation = parse_invocation(&["github-sync".to_string(), "5".to_string()]).unwrap();
        match invocation {
            Invocation::GithubSync { id, .. } => assert_eq!(id.as_deref(), Some("5")),
            other => panic!("expected a GithubSync invocation, got {other:?}"),
        }
    }

    /// …and the token still has to look like an id, so a typo is still refused
    /// rather than silently becoming one.
    #[test]
    fn github_sync_still_refuses_a_token_that_is_no_kind_of_id() {
        for token in ["--dry-runn", "nonsense", "-5", "5x"] {
            assert!(
                parse_invocation(&["github-sync".to_string(), token.to_string()]).is_err(),
                "`story github-sync {token}` should still be a usage error"
            );
        }
    }

    #[test]
    fn the_id_shape_predicate_covers_both_forms() {
        for id in ["SH-1", "API-42", "1", "5", "007", "0"] {
            assert!(looks_like_story_id(id), "`{id}` is shaped like an id");
        }
        for not_id in ["", "-", "SH-", "-1", "SH-x", "5x", "nonsense", "--dry-run"] {
            assert!(
                !looks_like_story_id(not_id),
                "`{not_id}` is not shaped like an id"
            );
        }
    }

    #[test]
    fn routes_move_command() {
        let invocation = parse_invocation(&[
            "move".to_string(),
            "SH-1".to_string(),
            "in-progress".to_string(),
        ])
        .unwrap();
        assert!(matches!(invocation, Invocation::SetState { .. }));
    }

    #[test]
    fn routes_show_command() {
        let invocation = parse_invocation(&["show".to_string(), "SH-1".to_string()]).unwrap();
        assert!(matches!(invocation, Invocation::Show { .. }));
    }

    #[test]
    fn unknown_command_errors() {
        let result = parse_invocation(&["SH-1".to_string(), "is".to_string(), "done".to_string()]);
        assert!(result.is_err());
    }

    // --- Type subcommand tests ---

    #[test]
    fn type_list() {
        let inv = parse_invocation(&["type".to_string(), "list".to_string()]).unwrap();
        assert!(matches!(
            inv,
            Invocation::Type {
                action: TypeAction::List
            }
        ));
    }

    #[test]
    fn type_add_slug_only() {
        let inv =
            parse_invocation(&["type".to_string(), "add".to_string(), "bug".to_string()]).unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "bug");
                assert_eq!(description, None);
                assert_eq!(emoji, None);
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_add_with_description() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "add".to_string(),
            "epic".to_string(),
            "--description".to_string(),
            "A large body of work".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "epic");
                assert_eq!(description.as_deref(), Some("A large body of work"));
                assert_eq!(emoji, None);
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_add_with_emoji() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "add".to_string(),
            "epic".to_string(),
            "--emoji".to_string(),
            "📚".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "epic");
                assert_eq!(description, None);
                assert_eq!(emoji.as_deref(), Some("📚"));
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_set_description_and_emoji() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--description".to_string(),
            "A defect".to_string(),
            "--emoji".to_string(),
            "🐞".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Set {
                        slug,
                        description,
                        clear_description,
                        emoji,
                        clear_emoji,
                    },
            } => {
                assert_eq!(slug, "bug");
                assert_eq!(description.as_deref(), Some("A defect"));
                assert!(!clear_description);
                assert_eq!(emoji.as_deref(), Some("🐞"));
                assert!(!clear_emoji);
            }
            other => panic!("expected Type::Set, got {:?}", other),
        }
    }

    #[test]
    fn type_set_no_emoji_clears_it() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--no-emoji".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Set {
                        emoji, clear_emoji, ..
                    },
            } => {
                assert_eq!(emoji, None);
                assert!(clear_emoji);
            }
            other => panic!("expected Type::Set, got {:?}", other),
        }
    }

    #[test]
    fn type_set_emoji_and_no_emoji_contradict() {
        let result = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--emoji".to_string(),
            "🐞".to_string(),
            "--no-emoji".to_string(),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn type_remove() {
        let inv = parse_invocation(&["type".to_string(), "remove".to_string(), "bug".to_string()])
            .unwrap();
        match inv {
            Invocation::Type {
                action: TypeAction::Remove { slug },
            } => {
                assert_eq!(slug, "bug");
            }
            other => panic!("expected Type::Remove, got {:?}", other),
        }
    }

    #[test]
    fn type_no_subcommand_errors() {
        let result = parse_invocation(&["type".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_unknown_subcommand_errors() {
        let result = parse_invocation(&["type".to_string(), "rename".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_add_missing_slug_errors() {
        let result = parse_invocation(&["type".to_string(), "add".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_remove_missing_slug_errors() {
        let result = parse_invocation(&["type".to_string(), "remove".to_string()]);
        assert!(result.is_err());
    }

    // --- Epic subcommand tests ---

    #[test]
    fn epic_list() {
        let inv = parse_invocation(&["epic".to_string(), "list".to_string()]).unwrap();
        assert!(matches!(
            inv,
            Invocation::Epic {
                action: EpicAction::List
            }
        ));
    }

    #[test]
    fn epic_show() {
        let inv = parse_invocation(&["epic".to_string(), "show".to_string(), "SH-1".to_string()])
            .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Show { id },
            } => {
                assert_eq!(id, "SH-1");
            }
            other => panic!("expected Epic::Show, got {:?}", other),
        }
    }

    #[test]
    fn epic_create() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "create".to_string(),
            "My Epic Title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Create { title },
            } => {
                assert_eq!(title, "My Epic Title");
            }
            other => panic!("expected Epic::Create, got {:?}", other),
        }
    }

    #[test]
    fn epic_create_multi_word() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "create".to_string(),
            "My".to_string(),
            "Epic".to_string(),
            "Title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Create { title },
            } => {
                assert_eq!(title, "My Epic Title");
            }
            other => panic!("expected Epic::Create, got {:?}", other),
        }
    }

    #[test]
    fn epic_add() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "add".to_string(),
            "SH-1".to_string(),
            "SH-2".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Add { epic_id, story_id },
            } => {
                assert_eq!(epic_id, "SH-1");
                assert_eq!(story_id, "SH-2");
            }
            other => panic!("expected Epic::Add, got {:?}", other),
        }
    }

    #[test]
    fn epic_no_subcommand_errors() {
        let result = parse_invocation(&["epic".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_unknown_subcommand_errors() {
        let result = parse_invocation(&["epic".to_string(), "rename".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_show_missing_id_errors() {
        let result = parse_invocation(&["epic".to_string(), "show".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_create_missing_title_errors() {
        let result = parse_invocation(&["epic".to_string(), "create".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_add_missing_story_id_errors() {
        let result = parse_invocation(&["epic".to_string(), "add".to_string(), "SH-1".to_string()]);
        assert!(result.is_err());
    }

    // --- --type flag on new/list/set ---

    #[test]
    fn new_with_type_flag() {
        let inv = parse_invocation(&[
            "new".to_string(),
            "My story".to_string(),
            "--type".to_string(),
            "bug".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::New {
                title,
                state,
                story_type,
                ..
            } => {
                assert_eq!(title, "My story");
                assert_eq!(state, None);
                assert_eq!(story_type.as_deref(), Some("bug"));
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_with_state_and_type() {
        let inv = parse_invocation(&[
            "new".to_string(),
            "My story".to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--type".to_string(),
            "epic".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::New {
                title,
                state,
                story_type,
                ..
            } => {
                assert_eq!(title, "My story");
                assert_eq!(state.as_deref(), Some("open"));
                assert_eq!(story_type.as_deref(), Some("epic"));
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_without_type_flag() {
        let inv = parse_invocation(&["new".to_string(), "My story".to_string()]).unwrap();
        match inv {
            Invocation::New { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn list_with_type_flag() {
        let inv = parse_invocation(&["list".to_string(), "--type".to_string(), "epic".to_string()])
            .unwrap();
        match inv {
            Invocation::List { story_type, .. } => {
                assert_eq!(story_type.as_deref(), Some("epic"));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn list_without_type_flag() {
        let inv = parse_invocation(&["list".to_string()]).unwrap();
        match inv {
            Invocation::List { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn set_with_type_flag() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--type".to_string(),
            "bug".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields { id, story_type, .. } => {
                assert_eq!(id, "SH-1");
                assert_eq!(story_type.as_deref(), Some("bug"));
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    #[test]
    fn set_with_type_and_other_fields() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--title".to_string(),
            "New title".to_string(),
            "--type".to_string(),
            "feature".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields {
                id,
                title,
                story_type,
                ..
            } => {
                assert_eq!(id, "SH-1");
                assert_eq!(title.as_deref(), Some("New title"));
                assert_eq!(story_type.as_deref(), Some("feature"));
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    #[test]
    fn set_without_type_flag() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--title".to_string(),
            "New title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    mod flag_shape {
        use super::super::{VERB_FLAGS, declared_flags, is_flag_shaped, reject_unknown_flags};

        fn argv(words: &[&str]) -> Vec<String> {
            words.iter().map(|word| word.to_string()).collect()
        }

        /// The shape rule, case by case. Each row is a decision the gate makes
        /// before any verb sees the token.
        #[test]
        fn shape_distinguishes_a_flag_from_data() {
            for flag in [
                "--typo",
                "--dry-run",
                "--state=todo",
                "--x",
                "--a1",
                "--json={}",
            ] {
                assert!(is_flag_shaped(flag), "`{flag}` is shaped like a flag");
            }

            for data in [
                // The constraint-(a) case: quoted prose is one argv token with
                // a space in it, so it can never be mistaken for a flag.
                "--fix the ingest path",
                "-- leading terminator then text",
                // The terminator itself, and a pasted rule line.
                "--",
                "---",
                "-----",
                // Single dashes are out of scope by the council's decision.
                "-h",
                "-5",
                "-",
                "-typo",
                // Not a flag name: must start with a letter.
                "--1st",
                "---dash",
                "--=value",
                // Ordinary data.
                "SH-1",
                "todo",
                "",
            ] {
                assert!(!is_flag_shaped(data), "`{data}` is data, not a flag");
            }
        }

        /// The reported defect, at the level the gate decides it.
        #[test]
        fn an_undeclared_flag_is_refused() {
            let error = reject_unknown_flags(&argv(&["new", "--typo", "x"]))
                .expect_err("`story new --typo x` must be refused");
            let message = error.to_string();
            assert!(message.contains("--typo"), "names the token: {message}");
            assert!(message.contains("story new"), "names the verb: {message}");
            assert!(
                message.contains("--priority"),
                "lists the verb's real flags: {message}"
            );
            assert!(
                message.contains("--"),
                "offers the terminator escape: {message}"
            );
        }

        /// Every currently-valid invocation still parses. The gate may only
        /// reject what a parser would have swallowed.
        #[test]
        fn a_declared_flag_is_left_alone() {
            for invocation in [
                vec!["new", "A title", "--priority", "high"],
                vec!["new", "A title", "--labels", "a,b", "--type", "bug"],
                vec!["list", "--ready", "--state", "todo"],
                vec!["move", "SH-1", "done", "--if-state", "in-progress"],
                vec!["set", "SH-1", "--title", "New", "--unblocked"],
                vec!["state", "add", "review", "--super", "OPEN"],
                vec!["state", "set", "review", "--no-description"],
                vec!["state", "remove", "review", "--move-stories-to", "todo"],
                // Both retired verbs, and their entries are what makes the
                // redirect — rather than a complaint about a flag — what a
                // user meets. See the comment on `VERB_FLAGS`.
                vec!["project", "init", "--prefix", "AB", "--no-agents-md"],
                vec!["project", "deinit", "--force"],
                vec!["project", "new", "--prefix", "AB", "--no-agents-md"],
                vec!["project", "delete", "--force"],
                vec!["member", "add", "--github", "someone"],
                vec!["type", "add", "spike", "--description", "text"],
                vec!["graph", "--blocked-by", "SH-1"],
                vec!["help", "--compact"],
                vec!["decompose", "--stdin", "--dry-run"],
                // Subcommands spelled as flags, which the spawner execs.
                vec!["daemon", "--serve", "--port", "0"],
                vec!["web", "--serve", "--port", "0"],
                vec!["daemon", "start", "--port", "3456"],
            ] {
                reject_unknown_flags(&argv(&invocation))
                    .unwrap_or_else(|error| panic!("`story {}`: {error}", invocation.join(" ")));
            }
        }

        /// A flag's value is that flag's business, never the gate's — so the
        /// one flag in this grammar whose value may begin with dashes keeps
        /// working.
        #[test]
        fn the_token_after_a_value_taking_flag_is_never_judged() {
            reject_unknown_flags(&argv(&["new", "t", "--description", "--odd"]))
                .expect("a value-taking flag's value is not judged");
            reject_unknown_flags(&argv(&["new", "t", "--labels", "--weird"]))
                .expect("likewise for --labels");
        }

        /// `--` ends option scanning, which is what makes a dash-leading value
        /// expressible at all.
        #[test]
        fn a_terminator_ends_the_scan() {
            reject_unknown_flags(&argv(&["new", "--", "--typo", "x"]))
                .expect("everything after `--` is data");
            reject_unknown_flags(&argv(&["comment", "SH-1", "--", "--json", "is", "great"]))
                .expect("likewise in a comment");
        }

        /// The gate declines to speak about a verb that does not exist, so a
        /// mistyped command still reports the command rather than its flags.
        #[test]
        fn an_unrecognized_verb_is_left_to_dispatch() {
            reject_unknown_flags(&argv(&["frobnicate", "--typo"]))
                .expect("an unknown command is dispatch's to report");
            reject_unknown_flags(&argv(&["--help"])).expect("a global flag is not a verb");
            reject_unknown_flags(&argv(&["-V"])).expect("nor is a version request");
        }

        /// Fail-closed: a verb with no table entry refuses every flag-shaped
        /// token rather than inheriting the defect this gate exists to remove.
        #[test]
        fn a_verb_that_declares_nothing_refuses_every_flag() {
            for invocation in [
                vec!["comment", "SH-1", "--typo"],
                vec!["block", "SH-1", "--typo"],
                vec!["delete", "SH-1", "--typo"],
                vec!["label", "SH-1", "--typo"],
                vec!["epic", "create", "--typo"],
                vec!["search", "--typo"],
                vec!["show", "--typo"],
            ] {
                let refused = reject_unknown_flags(&argv(&invocation));
                assert!(
                    refused.is_err(),
                    "`story {}` must be refused",
                    invocation.join(" ")
                );
            }
        }

        /// Every entry in the table is reachable: a two-token entry must name a
        /// verb that exists, so a typo in the table is caught here rather than
        /// by a flag silently ceasing to work.
        #[test]
        fn every_table_entry_names_a_real_verb() {
            for entry in VERB_FLAGS {
                let mut path = vec![entry.verb.to_string()];
                if let Some(sub) = entry.subcommand {
                    path.push(sub.to_string());
                }
                let found = declared_flags(&path).expect("the entry resolves");
                assert_eq!(
                    found.len(),
                    entry.flags.len(),
                    "`story {}` resolved to a different entry than its own",
                    path.join(" ")
                );
            }
        }

        /// A second token that is an *argument* must not be read as a
        /// subcommand — the bug that would silently disarm `move`'s only flag.
        #[test]
        fn an_argument_in_the_subcommand_slot_falls_back_to_the_verb() {
            let flags = declared_flags(&argv(&["move", "SH-1", "done"])).expect("move declares");
            assert!(flags.iter().any(|flag| flag.name == "if-state"));
        }
    }

    /// `story github-sync --resolve …` — SH-152's answer to a conflict.
    mod github_sync_resolve {
        use super::super::{ConflictSide, Invocation, parse_invocation};

        fn parse(args: &[&str]) -> Result<Invocation, crate::error::AppError> {
            parse_invocation(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        }

        #[test]
        fn a_sync_with_no_resolution_is_the_ordinary_case() {
            assert_eq!(
                parse(&["github-sync"]).expect("parses"),
                Invocation::GithubSync {
                    id: None,
                    dry_run: false,
                    resolve: None,
                    strategy: None,
                    mode: None,
                }
            );
        }

        #[test]
        fn both_sides_parse_against_a_named_story() {
            for (word, side) in [
                ("local", ConflictSide::Local),
                ("remote", ConflictSide::Remote),
            ] {
                assert_eq!(
                    parse(&["github-sync", "SH-1", "--resolve", word]).expect("parses"),
                    Invocation::GithubSync {
                        id: Some("SH-1".to_string()),
                        dry_run: false,
                        resolve: Some(side),
                        strategy: None,
                        mode: None,
                    }
                );
            }
        }

        /// The parser's job is the *shape*. Whether a resolution without a
        /// story is allowed is a rule about the sync, and it lives in
        /// `github::run_sync_with` so that the dashboard and a hand-built
        /// request meet it too — `tests/github_sync_conflicts.rs` is where the
        /// refusal is pinned.
        #[test]
        fn a_resolution_without_a_story_parses_and_is_refused_deeper_down() {
            assert_eq!(
                parse(&["github-sync", "--resolve", "local"]).expect("parses"),
                Invocation::GithubSync {
                    id: None,
                    dry_run: false,
                    resolve: Some(ConflictSide::Local),
                    strategy: None,
                    mode: None,
                }
            );
        }

        #[test]
        fn a_side_that_is_not_a_side_is_refused_by_name() {
            let error =
                parse(&["github-sync", "SH-1", "--resolve", "theirs"]).expect_err("refuses");
            assert!(error.to_string().contains("theirs"), "{error}");
        }

        #[test]
        fn a_resolution_with_nothing_after_it_says_what_it_wanted() {
            let error = parse(&["github-sync", "SH-1", "--resolve"]).expect_err("refuses");
            let message = error.to_string();
            assert!(message.contains("local"), "{message}");
            assert!(message.contains("remote"), "{message}");
        }
    }

    /// `story github-sync --strategy … --mode …` — SH-153's D2. The parser's
    /// job is the shape; whether the pair is allowed together, alone, or on an
    /// already-configured project is `github::run_sync_with`'s rule, same as
    /// `--resolve`'s.
    mod github_sync_setup_flags {
        use super::super::{Invocation, SetupMode, SetupStrategy, parse_invocation};

        fn parse(args: &[&str]) -> Result<Invocation, crate::error::AppError> {
            parse_invocation(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        }

        #[test]
        fn every_strategy_word_parses() {
            for (word, strategy) in [
                ("import-all", SetupStrategy::ImportAll),
                ("match-titles", SetupStrategy::MatchTitles),
                ("push-only", SetupStrategy::PushOnly),
                ("future-only", SetupStrategy::FutureOnly),
            ] {
                assert_eq!(
                    parse(&["github-sync", "--strategy", word, "--mode", "manual"])
                        .expect("parses"),
                    Invocation::GithubSync {
                        id: None,
                        dry_run: false,
                        resolve: None,
                        strategy: Some(strategy),
                        mode: Some(SetupMode::Manual),
                    }
                );
            }
        }

        #[test]
        fn both_mode_words_parse() {
            for (word, mode) in [("manual", SetupMode::Manual), ("off", SetupMode::Off)] {
                assert_eq!(
                    parse(&["github-sync", "--strategy", "future-only", "--mode", word])
                        .expect("parses"),
                    Invocation::GithubSync {
                        id: None,
                        dry_run: false,
                        resolve: None,
                        strategy: Some(SetupStrategy::FutureOnly),
                        mode: Some(mode),
                    }
                );
            }
        }

        /// `auto` is refused by name, not silently accepted and dropped —
        /// nothing implements it (SH-68).
        #[test]
        fn mode_auto_is_refused_by_name() {
            let error = parse(&["github-sync", "--strategy", "future-only", "--mode", "auto"])
                .expect_err("refuses");
            assert!(error.to_string().contains("SH-68"), "{error}");
        }

        #[test]
        fn an_unknown_strategy_word_is_refused_by_name() {
            let error = parse(&["github-sync", "--strategy", "guess", "--mode", "manual"])
                .expect_err("refuses");
            assert!(error.to_string().contains("guess"), "{error}");
        }

        #[test]
        fn an_unknown_mode_word_is_refused_by_name() {
            let error = parse(&[
                "github-sync",
                "--strategy",
                "future-only",
                "--mode",
                "guess",
            ])
            .expect_err("refuses");
            assert!(error.to_string().contains("guess"), "{error}");
        }

        /// The parser accepts either flag alone: whether that pairing is
        /// allowed is `run_sync_with`'s rule, pinned in
        /// `tests/github_sync_setup.rs`.
        #[test]
        fn either_flag_alone_parses_and_is_refused_deeper_down() {
            assert_eq!(
                parse(&["github-sync", "--strategy", "future-only"]).expect("parses"),
                Invocation::GithubSync {
                    id: None,
                    dry_run: false,
                    resolve: None,
                    strategy: Some(SetupStrategy::FutureOnly),
                    mode: None,
                }
            );
            assert_eq!(
                parse(&["github-sync", "--mode", "manual"]).expect("parses"),
                Invocation::GithubSync {
                    id: None,
                    dry_run: false,
                    resolve: None,
                    strategy: None,
                    mode: Some(SetupMode::Manual),
                }
            );
        }
    }
}
