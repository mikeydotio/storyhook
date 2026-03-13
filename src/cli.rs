use crate::error::AppError;

pub const HELP_TEXT: &str = r#"story - CLI-first issue tracker for AI agents

Usage:
  story init
  story new <title>
  story member add "<name <email>>"
  story member add -g <github-handle>
  story state add <state-slug> --super OPEN|CLOSED
  story state remove <state-slug>
  story list [--state <slug>] [--assignee <id|handle>] [--flagged]
  story doctor [--fix]
  story <id>
  story <id> "<comment>"
  story <id> assign <member-id|handle>
  story <id> is <state-slug> ["<comment>"]
  story <id> awaits "<reason>"
  story <id> awaits --clear
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
    Init,
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
        "doctor" => parse_doctor(args),
        _ => parse_story(args),
    }
}

fn parse_init(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 1 {
        return Err(AppError::Usage("usage: story init".to_string()));
    }
    Ok(Invocation::Init)
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
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(
                        "usage: story list [--state <slug>] [--assignee <id|handle>] [--flagged]"
                            .to_string(),
                    )
                })?;
                state = Some(value.clone());
                index += 2;
            }
            "--assignee" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(
                        "usage: story list [--state <slug>] [--assignee <id|handle>] [--flagged]"
                            .to_string(),
                    )
                })?;
                assignee = Some(value.clone());
                index += 2;
            }
            "--flagged" => {
                flagged = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(
                    "usage: story list [--state <slug>] [--assignee <id|handle>] [--flagged]"
                        .to_string(),
                ));
            }
        }
    }

    Ok(Invocation::List {
        state,
        assignee,
        flagged,
    })
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
