use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphMode {
    Overview,
    CriticalPath,
    BlockedBy(String),
    ParallelGroups,
}

pub const HELP_TEXT: &str = r#"story - CLI-first issue tracker for AI agents

Usage:
  story init [--prefix <PREFIX>]
  story new <title>
  story member add "<name <email>>"
  story member add -g <github-handle>
  story state add <state-slug> --super OPEN|CLOSED
  story state remove <state-slug>
  story list [--state <slug>] [--assignee <id|handle>] [--flagged] [--priority <levels>]
             [--label <labels>] [--created-after <date>] [--updated-after <date>]
             [--blocked] [--ready]
  story next [--count <n>]
  story summary
  story search <query>
  story import [<file>]
  story export
  story import-project <file>
  story context [--format markdown|json]
  story handoff [--since <duration>]
  story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
  story doctor [--fix]
  story <id>
  story <id> "<comment>"
  story <id> assign <member-id|handle>
  story <id> is <state-slug> ["<comment>"]
  story <id> awaits "<reason>"
  story <id> awaits --clear
  story <id> priority <critical|high|medium|low|none>
  story <id> label <labels-csv>
  story <id> label --remove <labels-csv>
  story <id> reopen
  story <a> <relationship> <b> [--remove]

Global options:
  --json    Emit structured JSON
  --quiet   Suppress success output
  -h, --help
"#;

#[derive(Clone, Debug)]
pub struct CliOptions {
    pub json: bool,
    pub quiet: bool,
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
    },
    New {
        title: String,
    },
    MemberAdd {
        input: MemberInput,
    },
    StateAdd {
        slug: String,
        superstate: String,
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
    },
    Search {
        query: String,
    },
    Next {
        count: usize,
    },
    Summary,
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
    Import {
        file: Option<String>,
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
    Graph {
        mode: GraphMode,
    },
    Relate {
        a: String,
        relation: String,
        b: String,
        remove: bool,
    },
}

pub fn split_global_flags(args: &[String]) -> (bool, bool, Vec<String>) {
    let mut json = false;
    let mut quiet = false;
    let mut filtered = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--quiet" => quiet = true,
            _ => filtered.push(arg.clone()),
        }
    }

    (json, quiet, filtered)
}

pub fn parse_invocation(args: &[String]) -> Result<Invocation, AppError> {
    if args.is_empty() {
        return Ok(Invocation::Help);
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(Invocation::Help),
        "init" => parse_init(args),
        "new" => parse_new(args),
        "member" => parse_member(args),
        "state" => parse_state(args),
        "list" => parse_list(args),
        "next" => parse_next(args),
        "summary" => Ok(Invocation::Summary),
        "search" => parse_search(args),
        "import" => parse_import(args),
        "import-project" => parse_import_project(args),
        "export" => Ok(Invocation::Export),
        "context" => parse_context(args),
        "handoff" => parse_handoff(args),
        "graph" => parse_graph(args),
        "doctor" => parse_doctor(args),
        _ => parse_story(args),
    }
}

fn parse_init(args: &[String]) -> Result<Invocation, AppError> {
    let mut prefix = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--prefix" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage("usage: story init [--prefix <PREFIX>]".to_string())
                })?;
                prefix = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(
                    "usage: story init [--prefix <PREFIX>]".to_string(),
                ));
            }
        }
    }
    Ok(Invocation::Init { prefix })
}

fn parse_new(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage("usage: story new <title>".to_string()));
    }
    Ok(Invocation::New {
        title: join_tokens(&args[1..]),
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
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--super" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage(
                                "usage: story state add <slug> --super OPEN|CLOSED".to_string(),
                            )
                        })?;
                        superstate = Some(value.clone());
                        index += 2;
                    }
                    token if token.starts_with("--super=") => {
                        superstate = Some(token.trim_start_matches("--super=").to_string());
                        index += 1;
                    }
                    _ => {
                        return Err(AppError::Usage(
                            "usage: story state add <slug> --super OPEN|CLOSED".to_string(),
                        ));
                    }
                }
            }

            Ok(Invocation::StateAdd {
                slug,
                superstate: superstate.ok_or_else(|| {
                    AppError::Usage("usage: story state add <slug> --super OPEN|CLOSED".to_string())
                })?,
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
    let mut index = 1;
    let usage = "usage: story list [--state <slug>] [--assignee <id>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready]";

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
    })
}

fn parse_next(args: &[String]) -> Result<Invocation, AppError> {
    let mut count = 1;
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            "--count" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage("usage: story next [--count <n>]".to_string())
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
            _ => {
                return Err(AppError::Usage(
                    "usage: story next [--count <n>]".to_string(),
                ));
            }
        }
    }

    Ok(Invocation::Next { count })
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
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage("usage: story context [--format markdown|json]".to_string())
                })?;
                format = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(
                    "usage: story context [--format markdown|json]".to_string(),
                ));
            }
        }
    }
    Ok(Invocation::Context { format })
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

fn parse_story(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() == 1 {
        return Ok(Invocation::Show {
            id: args[0].clone(),
        });
    }

    if args.len() >= 3 && args[1] == "assign" {
        return Ok(Invocation::Assign {
            id: args[0].clone(),
            member: join_tokens(&args[2..]),
        });
    }

    if args.len() >= 3 && args[1] == "is" {
        return Ok(Invocation::SetState {
            id: args[0].clone(),
            state: args[2].clone(),
            comment: if args.len() > 3 {
                Some(join_tokens(&args[3..]))
            } else {
                None
            },
        });
    }

    if args.len() >= 2 && args[1] == "awaits" {
        if args.len() == 3 && args[2] == "--clear" {
            return Ok(Invocation::ClearAwaiting {
                id: args[0].clone(),
            });
        }

        if args.len() >= 3 {
            let awaiting = join_tokens(&args[2..]);
            if awaiting.is_empty() {
                return Err(AppError::Usage(
                    "usage: story <id> awaits \"<reason>\" | story <id> awaits --clear".to_string(),
                ));
            }

            return Ok(Invocation::SetAwaiting {
                id: args[0].clone(),
                awaiting,
            });
        }

        return Err(AppError::Usage(
            "usage: story <id> awaits \"<reason>\" | story <id> awaits --clear".to_string(),
        ));
    }

    if args.len() == 3 && args[1] == "priority" {
        return Ok(Invocation::SetPriority {
            id: args[0].clone(),
            priority: args[2].clone(),
        });
    }

    if args.len() >= 3 && args[1] == "label" {
        if args[2] == "--remove" {
            if args.len() < 4 {
                return Err(AppError::Usage(
                    "usage: story <id> label --remove <labels-csv>".to_string(),
                ));
            }
            let remove: Vec<String> = args[3]
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Ok(Invocation::SetLabels {
                id: args[0].clone(),
                add: Vec::new(),
                remove,
            });
        }
        let add: Vec<String> = args[2]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        return Ok(Invocation::SetLabels {
            id: args[0].clone(),
            add,
            remove: Vec::new(),
        });
    }

    if args.len() == 2 && args[1] == "reopen" {
        return Ok(Invocation::Reopen {
            id: args[0].clone(),
        });
    }

    if args.len() >= 3 && crate::domain::is_relation_input(&args[1]) {
        let remove = args[3..].iter().any(|arg| arg == "--remove");
        return Ok(Invocation::Relate {
            a: args[0].clone(),
            relation: args[1].clone(),
            b: args[2].clone(),
            remove,
        });
    }

    Ok(Invocation::Comment {
        id: args[0].clone(),
        text: join_tokens(&args[1..]),
    })
}

fn join_tokens(tokens: &[String]) -> String {
    tokens.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{Invocation, parse_invocation};

    #[test]
    fn routes_state_change_form() {
        let invocation = parse_invocation(&[
            "SH-1".to_string(),
            "is".to_string(),
            "in-progress".to_string(),
            "note".to_string(),
        ])
        .unwrap();
        assert!(matches!(invocation, Invocation::SetState { .. }));
    }
}
