use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HooksAction {
    Install,
    Uninstall,
    List,
    Test { event_type: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphMode {
    Overview,
    CriticalPath,
    BlockedBy(String),
    ParallelGroups,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhaseAction {
    List,
    Show { phase: String },
    Add { id: String, phase: String },
    Remove { id: String },
    Create { phase: String, title: Option<String> },
}

pub const HELP_TEXT: &str = r#"story - CLI-first issue tracker for AI agents

Usage:
  story init [--prefix <PREFIX>] [--no-agents-md]
  story new <title> [--state <slug>]
  story tui                                           (interactive terminal UI)
  story member add "<name <email>>"
  story member add -g <github-handle>
  story state add <state-slug> --super OPEN|CLOSED [--role active]
  story state remove <state-slug>
  story list [--state <slug>] [--assignee <id|handle>] [--flagged] [--priority <levels>]
             [--label <labels>] [--created-after <date>] [--updated-after <date>]
             [--blocked] [--ready] [--stale <duration>] [--phase <N>]
  story next [--count <n>] [--phase <N>]
  story summary
  story report [--html]
  story search <query>
  story import [<file>]
  story export
  story decompose <file> [--dry-run]     (markdown or YAML)
  story decompose --stdin [--dry-run]
  story import-project <file>
  story load-context [--format markdown|json]
  story handoff [--since <duration>]
  story phase list
  story phase show <N>
  story phase add <id> <N>
  story phase remove <id>
  story phase create <N> ["<title>"]
  story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
  story doctor [--fix]
  story mcp-config [--install <provider>] [--uninstall <provider>] [--uninstall-all]
  story mcp-config [--scope project]
  story hooks install|uninstall|list|test <event_type>
  story commit-sync [--since <duration>]
  story github-sync [<id>] [--dry-run]
  story scaffold agents-md|claude-md|cursor-rules
  story help <command>
  story plugin install|uninstall <target>
  story show <id>
  story comment <id> "<text>"
  story assign <id> <member-id|handle>
  story move <id> <state-slug> ["<comment>"]
  story block <id> "<reason>"
  story unblock <id>
  story prioritize <id> <critical|high|medium|low|none>
  story label <id> <labels-csv>
  story unlabel <id> <labels-csv>
  story reopen <id>
  story delete <id> "<reason>"
  story set <id> [--title "<title>"] [--state <slug>] [--priority <level>]
                  [--assignee <member>] [--labels "<csv>"] [--blocked "<reason>"]
                  [--unblocked] [--json "<json>"]
  story relate <a> <relationship-type> <b>
  story unrelate <a> <relationship-type> <b>
  story link <a> <relationship-type> <b>
  story unlink <a> <relationship-type> <b>

Global options:
  --json      Emit structured JSON
  --quiet     Suppress success output
  --no-hooks  Suppress event hook execution
  -h, --help
"#;

#[derive(Clone, Debug)]
pub struct CliOptions {
    pub json: bool,
    pub quiet: bool,
    pub no_hooks: bool,
    pub invocation: Invocation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberInput {
    Identity(String),
    Github(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Init {
        prefix: Option<String>,
        no_agents_md: bool,
    },
    New {
        title: String,
        state: Option<String>,
    },
    MemberAdd {
        input: MemberInput,
    },
    StateAdd {
        slug: String,
        superstate: String,
        role: Option<String>,
    },
    StateRemove {
        slug: String,
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
    Context {
        format: Option<String>,
    },
    Handoff {
        since: Option<String>,
    },
    Phase {
        action: PhaseAction,
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
    },
    Relate {
        a: String,
        relation: String,
        b: String,
        remove: bool,
    },
    McpConfig {
        scope: Option<String>,
        install: Option<String>,
        uninstall: Option<String>,
        uninstall_all: bool,
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
    Plugin {
        action: PluginAction,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginAction {
    Install { target: String },
    Uninstall { target: String },
}

pub fn split_global_flags(args: &[String]) -> (bool, bool, bool, Vec<String>) {
    let mut json = false;
    let mut quiet = false;
    let mut no_hooks = false;
    let mut filtered = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--no-hooks" => no_hooks = true,
            _ => filtered.push(arg.clone()),
        }
    }

    (json, quiet, no_hooks, filtered)
}

pub fn parse_invocation(args: &[String]) -> Result<Invocation, AppError> {
    if args.is_empty() {
        return Ok(Invocation::Help);
    }

    match args[0].as_str() {
        "-h" | "--help" => Ok(Invocation::Help),
        "help" => parse_help(args),
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
        "export" => Ok(Invocation::Export),
        "load-context" | "context" => parse_context(args),
        "phase" => parse_phase(args),
        "handoff" => parse_handoff(args),
        "graph" => parse_graph(args),
        "doctor" => parse_doctor(args),
        "mcp-config" => parse_mcp_config(args),
        "hooks" => parse_hooks(args),
        "scaffold" => parse_scaffold(args),
        "commit-sync" | "sync-git" => parse_commit_sync(args),
        "github-sync" => parse_github_sync(args),
        "plugin" => parse_plugin(args),
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
    let mut title_parts = Vec::new();
    let mut index = 1;
    let usage = "usage: story new <title> [--state <slug>]";
    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
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

fn parse_state(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            "usage: story state add <slug> --super OPEN|CLOSED | story state remove <slug>"
                .to_string(),
        ));
    }

    match args[1].as_str() {
        "add" => {
            let slug = args[2].clone();
            let mut superstate = None;
            let mut role = None;
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--super" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage(
                                "usage: story state add <slug> --super OPEN|CLOSED [--role active]".to_string(),
                            )
                        })?;
                        superstate = Some(value.clone());
                        index += 2;
                    }
                    token if token.starts_with("--super=") => {
                        superstate = Some(token.trim_start_matches("--super=").to_string());
                        index += 1;
                    }
                    "--role" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage(
                                "usage: story state add <slug> --super OPEN|CLOSED [--role active]".to_string(),
                            )
                        })?;
                        role = Some(value.clone());
                        index += 2;
                    }
                    token if token.starts_with("--role=") => {
                        role = Some(token.trim_start_matches("--role=").to_string());
                        index += 1;
                    }
                    _ => {
                        return Err(AppError::Usage(
                            "usage: story state add <slug> --super OPEN|CLOSED [--role active]".to_string(),
                        ));
                    }
                }
            }

            Ok(Invocation::StateAdd {
                slug,
                superstate: superstate.ok_or_else(|| {
                    AppError::Usage("usage: story state add <slug> --super OPEN|CLOSED [--role active]".to_string())
                })?,
                role,
            })
        }
        "remove" => Ok(Invocation::StateRemove {
            slug: args[2].clone(),
        }),
        _ => Err(AppError::Usage(
            "usage: story state add <slug> --super OPEN|CLOSED | story state remove <slug>"
                .to_string(),
        )),
    }
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
    let mut index = 1;
    let usage = "usage: story list [--state <slug>] [--assignee <id>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready] [--stale <duration>] [--phase <N>]";

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
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(usage.to_string())
                })?;
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
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(usage.to_string())
                })?;
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

fn parse_context(args: &[String]) -> Result<Invocation, AppError> {
    let mut format = None;
    let mut index = 1;
    let usage = "usage: story load-context [--format markdown|json]";
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(usage.to_string())
                })?;
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
    let usage = "usage: story phase list|show <N>|add <id> <N>|remove <id>|create <N> [\"<title>\"]";
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
                .ok_or_else(|| {
                    AppError::Usage("usage: story phase remove <id>".to_string())
                })?
                .clone();
            Ok(Invocation::Phase {
                action: PhaseAction::Remove { id },
            })
        }
        "create" => {
            let phase = args
                .get(2)
                .ok_or_else(|| {
                    AppError::Usage(
                        "usage: story phase create <N> [\"<title>\"]".to_string(),
                    )
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

fn parse_mcp_config(args: &[String]) -> Result<Invocation, AppError> {
    let mut scope = None;
    let mut install = None;
    let mut uninstall = None;
    let mut uninstall_all = false;
    let mut index = 1;
    let usage = "usage: story mcp-config [--install <provider>] [--uninstall <provider>] [--uninstall-all] [--scope project]";
    while index < args.len() {
        match args[index].as_str() {
            "--scope" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                scope = Some(value.clone());
                index += 2;
            }
            "--install" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                install = Some(value.clone());
                index += 2;
            }
            "--uninstall" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                uninstall = Some(value.clone());
                index += 2;
            }
            "--uninstall-all" => {
                uninstall_all = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    // Mutual exclusivity: at most one of --install, --uninstall, --uninstall-all, --scope
    let flag_count = scope.is_some() as u8
        + install.is_some() as u8
        + uninstall.is_some() as u8
        + uninstall_all as u8;
    if flag_count > 1 {
        return Err(AppError::Usage(usage.to_string()));
    }
    Ok(Invocation::McpConfig {
        scope,
        install,
        uninstall,
        uninstall_all,
    })
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
    if args.len() < 2 {
        return Ok(Invocation::Help);
    }
    Ok(Invocation::HelpTopic {
        topic: args[1].clone(),
    })
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

fn looks_like_story_id(s: &str) -> bool {
    // Story IDs are PREFIX-DIGITS (e.g., SH-1, API-42).
    // Reject bare words that are clearly not IDs so typos like
    // "story mcp-config" on an old binary produce a clear error.
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
    Ok(Invocation::Show { id: args[1].clone() })
}

fn parse_comment(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage("usage: story comment <id> \"<text>\"".to_string()));
    }
    Ok(Invocation::Comment {
        id: args[1].clone(),
        text: join_tokens(&args[2..]),
    })
}

fn parse_assign(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage("usage: story assign <id> <member>".to_string()));
    }
    Ok(Invocation::Assign {
        id: args[1].clone(),
        member: join_tokens(&args[2..]),
    })
}

fn parse_move(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage("usage: story move <id> <state> [\"<comment>\"]".to_string()));
    }
    Ok(Invocation::SetState {
        id: args[1].clone(),
        state: args[2].clone(),
        comment: if args.len() > 3 { Some(join_tokens(&args[3..])) } else { None },
    })
}

fn parse_block(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage("usage: story block <id> \"<reason>\"".to_string()));
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
    Ok(Invocation::ClearAwaiting { id: args[1].clone() })
}

fn parse_prioritize(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage("usage: story prioritize <id> <level>".to_string()));
    }
    Ok(Invocation::SetPriority {
        id: args[1].clone(),
        priority: args[2].clone(),
    })
}

fn parse_label(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage("usage: story label <id> <labels-csv>".to_string()));
    }
    let add: Vec<String> = args[2].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add,
        remove: Vec::new(),
    })
}

fn parse_unlabel(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage("usage: story unlabel <id> <labels-csv>".to_string()));
    }
    let remove: Vec<String> = args[2].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add: Vec::new(),
        remove,
    })
}

fn parse_reopen_verb(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage("usage: story reopen <id>".to_string()));
    }
    Ok(Invocation::Reopen { id: args[1].clone() })
}

fn parse_delete_verb(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage("usage: story delete <id> \"<reason>\"".to_string()));
    }
    let reason = join_tokens(&args[2..]);
    if reason.is_empty() {
        return Err(AppError::Usage("usage: story delete <id> \"<reason>\"".to_string()));
    }
    Ok(Invocation::Delete { id: args[1].clone(), reason })
}

fn parse_relate(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 4 {
        return Err(AppError::Usage("usage: story relate <a> <relationship-type> <b>".to_string()));
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
        return Err(AppError::Usage("usage: story unrelate <a> <relationship-type> <b>".to_string()));
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
        return Err(AppError::Usage("usage: story set <id> [--field value ...]".to_string()));
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
    let mut index = 2;
    let usage = "usage: story set <id> [--title \"<title>\"] [--state <slug>] [--priority <level>] [--assignee <member>] [--labels \"<csv>\"] [--blocked \"<reason>\"] [--unblocked] [--json \"<json>\"]";

    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                title = Some(value.clone());
                index += 2;
            }
            "--state" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--priority" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--assignee" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                assignee = Some(value.clone());
                index += 2;
            }
            "--labels" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels = Some(value.clone());
                index += 2;
            }
            "--blocked" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                blocked = Some(value.clone());
                index += 2;
            }
            "--unblocked" => {
                unblocked = true;
                index += 1;
            }
            "--json" => {
                let value = args.get(index + 1).ok_or_else(|| AppError::Usage(usage.to_string()))?;
                json = Some(value.clone());
                index += 2;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }

    if title.is_none() && state.is_none() && priority.is_none() && assignee.is_none()
        && labels.is_none() && blocked.is_none() && !unblocked && json.is_none() {
        return Err(AppError::Usage("no fields specified. Usage: story set <id> --<field> <value> ...".to_string()));
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
    })
}

fn join_tokens(tokens: &[String]) -> String {
    tokens.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{Invocation, parse_invocation};

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
}
