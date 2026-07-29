use serde::{Deserialize, Serialize};

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
  story init [--prefix <PREFIX>] [--no-agents-md]
  story new <title> [--state <slug>] [--type <slug>] [--description <text>]
                    [--priority <level>] [--assignee <member>] [--label <name> ...]
  story tui                                           (interactive terminal UI)
  story web start [--port <PORT>]                  (start web dashboard)
  story web stop                                   (stop web dashboard)
  story web open                                   (open the dashboard in your browser)
  story web address                                (copy the dashboard URL to the clipboard)
  story web register [<PATH>] [--name <NAME>]      (add a repo to the dashboard)
  story web deregister <ID|PATH>                   (remove a repo from the dashboard)
  story web list                                   (list registered repos)
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
  story github-sync [<id>] [--dry-run]
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
  story set <id> [--title "<title>"] [--state <slug>] [--priority <level>]
                  [--assignee <member>] [--labels "<csv>"] [--blocked "<reason>"]
                  [--unblocked] [--json "<json>"] [--type <slug>]
                  [--description "<text>"]
  story relate <a> <relationship-type> <b>
  story unrelate <a> <relationship-type> <b>
  story link <a> <relationship-type> <b>
  story unlink <a> <relationship-type> <b>
  story type list
  story type add <slug> [--description "<text>"]
  story type remove <slug>
  story epic list
  story epic show <id>
  story epic create "<title>"
  story epic add <epic-id> <story-id>

Global options:
  --json          Emit structured JSON
  --quiet         Suppress success output
  --no-hooks      Suppress event hook execution
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
    Init {
        prefix: Option<String>,
        no_agents_md: bool,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebAction {
    Start {
        port: u16,
    },
    Stop,
    Status,
    Serve {
        port: u16,
    },
    Register {
        path: std::path::PathBuf,
        name: Option<String>,
    },
    Deregister {
        target: String,
    },
    List,
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
    /// `--local`: run in this process rather than through the daemon.
    ///
    /// A first-class, permanent mode rather than an escape hatch. A git hook
    /// that spawned a daemon inside `prepare-commit-msg` would be hostile, and
    /// a CI job that started one for a single command would be paying for
    /// something it never uses again. SQLite's WAL is designed for exactly this
    /// — several processes on one database — so `--local` is not a lesser path,
    /// it is the same services with one less hop.
    pub local: bool,
}

pub fn split_global_flags(args: &[String]) -> (GlobalFlags, Vec<String>) {
    let mut flags = GlobalFlags::default();
    let mut filtered = Vec::new();

    let mut i = 0;
    while i < args.len() {
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
            "--local" => flags.local = true,
            _ => filtered.push(args[i].clone()),
        }
        i += 1;
    }

    (flags, filtered)
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
    // `tui` is dispatched in main.rs before parsing ever happens, so
    // `dispatch` does not know it, but it is a verb like any other here.
    let recognized = verb == "tui"
        || !matches!(
            dispatch(&[args[0].clone()]),
            Err(AppError::Usage(ref message)) if message.starts_with("unknown command")
        );
    recognized.then(|| help_for_verb(verb))
}

pub fn parse_invocation(args: &[String]) -> Result<Invocation, AppError> {
    if args.is_empty() {
        return Ok(Invocation::Help);
    }

    if let Some(help) = verb_help_request(args) {
        return Ok(help);
    }

    dispatch(args)
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
        "init" => parse_init(args),
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

fn parse_init(args: &[String]) -> Result<Invocation, AppError> {
    let mut prefix = None;
    let mut no_agents_md = false;
    let mut index = 1;
    let usage = "usage: story init [--prefix <PREFIX>] [--no-agents-md]";
    while index < args.len() {
        match args[index].as_str() {
            "--prefix" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                prefix = Some(value.clone());
                index += 2;
            }
            "--no-agents-md" => {
                no_agents_md = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    Ok(Invocation::Init {
        prefix,
        no_agents_md,
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
                labels.extend(
                    value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
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
    let usage = "usage: story type list | story type add <slug> [--description \"<text>\"] | story type remove <slug>";
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }

    match args[1].as_str() {
        "list" => Ok(Invocation::Type {
            action: TypeAction::List,
        }),
        "add" => {
            let slug = args
                .get(2)
                .ok_or_else(|| {
                    AppError::Usage(
                        "usage: story type add <slug> [--description \"<text>\"]".to_string(),
                    )
                })?
                .clone();
            let mut description = None;
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--description" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage(
                                "usage: story type add <slug> [--description \"<text>\"]"
                                    .to_string(),
                            )
                        })?;
                        description = Some(value.clone());
                        index += 2;
                    }
                    _ => {
                        return Err(AppError::Usage(
                            "usage: story type add <slug> [--description \"<text>\"]".to_string(),
                        ));
                    }
                }
            }
            Ok(Invocation::Type {
                action: TypeAction::Add { slug, description },
            })
        }
        "remove" => {
            let slug = args
                .get(2)
                .ok_or_else(|| AppError::Usage("usage: story type remove <slug>".to_string()))?
                .clone();
            Ok(Invocation::Type {
                action: TypeAction::Remove { slug },
            })
        }
        _ => Err(AppError::Usage(usage.to_string())),
    }
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
    let mut index = 1;
    let usage = "usage: story github-sync [<id>] [--dry-run]";
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
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
    Ok(Invocation::GithubSync { id, dry_run })
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
fn parse_daemon(args: &[String]) -> Result<Invocation, AppError> {
    let usage = "usage: story daemon start [--port <PORT>] | stop | status | install | uninstall";
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
    let usage = "usage: story web start [--port <PORT>] | stop | status | open | address | \
                  register [PATH] [--name <NAME>] | deregister <ID|PATH> | list";
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
        "register" => {
            let mut path = std::path::PathBuf::from(".");
            let mut name = None;
            let mut index = 2;
            if let Some(next) = args.get(2)
                && !next.starts_with("--")
            {
                path = std::path::PathBuf::from(next);
                index = 3;
            }
            while index < args.len() {
                match args[index].as_str() {
                    "--name" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage("--name requires a value".to_string())
                        })?;
                        name = Some(value.clone());
                        index += 2;
                    }
                    _ => return Err(AppError::Usage(usage.to_string())),
                }
            }
            Ok(Invocation::Web {
                action: WebAction::Register { path, name },
            })
        }
        "deregister" => {
            let target = args
                .get(2)
                .ok_or_else(|| AppError::Usage(usage.to_string()))?
                .clone();
            Ok(Invocation::Web {
                action: WebAction::Deregister { target },
            })
        }
        "list" => Ok(Invocation::Web {
            action: WebAction::List,
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

fn looks_like_story_id(s: &str) -> bool {
    // Story IDs are PREFIX-DIGITS (e.g., SH-1, API-42).
    // Reject bare words that are clearly not IDs so typos like
    // unknown hyphenated commands produce a clear error.
    if let Some(pos) = s.find('-') {
        let prefix = &s[..pos];
        let suffix = &s[pos + 1..];
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphanumeric())
            && !suffix.is_empty()
            && suffix.chars().all(|c| c.is_ascii_digit())
    } else {
        false
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
    // trailing args. A token-by-token flag loop over free-text comment
    // content is what previously let a comment beginning with `--` fail as
    // an "unrecognized flag" (a real backward-compatibility break: comments
    // have always been unrestricted free text), and let an unrelated
    // `--if-state` substring inside an unquoted multi-word comment be
    // silently spliced out mid-comment and mistaken for a real CAS guard.
    // Pinning the flag to one unambiguous position means every other
    // trailing token, anywhere, is comment prose with zero restrictions —
    // restoring the pre-existing `join_tokens(&args[3..])` guarantee for
    // any caller not opting into `--if-state`.
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
    let add: Vec<String> = args[2]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
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
    let remove: Vec<String> = args[2]
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
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
    use super::{EpicAction, Invocation, TypeAction, parse_invocation};

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
                action: TypeAction::Add { slug, description },
            } => {
                assert_eq!(slug, "bug");
                assert_eq!(description, None);
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
                action: TypeAction::Add { slug, description },
            } => {
                assert_eq!(slug, "epic");
                assert_eq!(description.as_deref(), Some("A large body of work"));
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
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
}
