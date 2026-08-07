use std::env;
use std::process;

use storyhook::cli::{self, DaemonAction, Invocation, StoreAction, WebAction};
use storyhook::invoke::{HttpInvoker, InvokeRequest, Invoker};
use storyhook::output::{self, Response};

/// Reports `error` on the stream its consumer reads, and exits with the
/// error's code.
///
/// Plain-text diagnostics go to stderr, so a caller can suppress them
/// (`story move ... --quiet 2>/dev/null || true`, which storyhook's own
/// post-merge hook uses) without them landing in the middle of unrelated
/// output.
///
/// `--json` is deliberately different: there, stdout is a machine-readable
/// result channel carrying exactly one self-describing document per run, and
/// the error envelope is the result the caller asked for. See
/// `tests/cli_error_streams.rs` for the contract, and
/// plugin/claude-code/bin/story.sh, whose CAS claim reads a conflict envelope
/// off stdout.
fn fail(error: &storyhook::error::AppError, json: bool) -> ! {
    let rendered = output::render_error(error, json);
    if json {
        print!("{rendered}");
    } else {
        eprint!("{rendered}");
    }
    process::exit(error.exit_code());
}

fn main() {
    // First, before anything — including argument parsing, which decides
    // nothing this depends on.
    //
    // A daemon outlives the client that started it and holds that client's
    // environment for its whole life, and since SH-114 the daemon is the process
    // that runs every git storyhook runs. One `GIT_DIR` exported in one shell
    // therefore decided what every later command on the machine saw, in every
    // repository (SH-160). Scrubbing here rather than at the spawn covers the
    // launchd agent and a hand-run `story daemon --serve` as well, because all
    // three routes come through this function — and it cleans what the daemon
    // hands on to a user's event hooks, which no funnel inside `src/` can reach.
    //
    // Unconditional rather than gated to `--serve`: a client has no legitimate
    // use for an inherited git environment either, every request already carries
    // its own working directory, and a gate is a conditional somebody can get
    // wrong. See `env::git_env` for the safety argument, which turns on this
    // call site being single-threaded.
    storyhook::env::git_env::scrub_this_process();

    let raw_args = env::args().skip(1).collect::<Vec<_>>();

    // Global flags come off first, before anything looks at a verb, because
    // `--store-path` decides which store *every* branch below resolves —
    // including `tui`, which never reaches the parser.
    //
    // `--json` is read by hand once, and only for the window in which parsing
    // the flags is what failed: a caller who asked for an envelope should get
    // one even when the thing that went wrong was the flag list itself.
    let json = raw_args.iter().any(|arg| arg == "--json");
    let (flags, filtered_args) = match cli::split_global_flags(&raw_args) {
        Ok(split) => split,
        Err(error) => fail(&error, json),
    };
    let json = flags.json;
    publish_store_path(flags.store_path.as_deref(), json);

    // `tui` is dispatched here, ahead of parsing, so `story tui --help` would
    // launch the interactive UI instead of explaining it; the help request
    // falls through to the parser, which answers it like any other verb's.
    if filtered_args.first().is_some_and(|arg| arg == "tui")
        && !cli::is_help_request(&filtered_args)
    {
        let cwd = env::current_dir().unwrap_or_else(|e| {
            eprintln!("error: failed to resolve current directory: {e}");
            process::exit(1);
        });
        if let Err(e) = storyhook::tui::run(&cwd) {
            eprintln!("error: {e}");
            process::exit(e.exit_code());
        }
        return;
    }

    let invocation = match cli::parse_invocation(&filtered_args) {
        Ok(invocation) => invocation,
        Err(error) => fail(&error, json),
    };

    // Foreground daemon mode: `story daemon --serve` (and its `story web
    // --serve` alias). Runs the daemon in this process — what the background
    // spawner execs and what a launchd agent runs — so it never returns.
    if let Some(port) = foreground_serve_port(&invocation) {
        let environment =
            match storyhook::env::Environment::from_process(flags.store_path.as_deref()) {
                Ok(environment) => environment,
                Err(error) => fail(&error, json),
            };
        let environment = match port {
            Some(port) => {
                environment.daemon_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
            }
            None => environment,
        };
        let result = storyhook::invoke::open_store(&environment)
            .and_then(|store| storyhook::daemon::lifecycle::run(&store, &environment));
        if let Err(e) = result {
            // The client that started this process is waiting on a portfile it
            // is never going to get, and this is the only process that knows
            // why. Recorded before the message is printed, because stderr here
            // is a log file nothing parses — see `Environment::daemon_failure`.
            storyhook::daemon::lifecycle::record_startup_failure(&environment, &e);
            eprintln!("error: {e}");
            process::exit(e.exit_code());
        }
        return;
    }

    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let error = storyhook::error::AppError::Storage(format!(
                "failed to resolve current directory: {error}"
            ));
            fail(&error, json);
        }
    };

    // A bare `story project new` is a request to be asked, and this process is
    // the only one that can answer it. Resolved here, before anything opens a
    // store: a cancellation must not have started a daemon.
    let invocation = match ask_about_a_new_project(invocation, &cwd, json) {
        Asked::Proceed(invocation) => *invocation,
        // The questionnaire has already said so on the stream it was given;
        // saying it again here would be the second owner of one message.
        Asked::Cancelled => return,
    };

    // `store new` names the store it creates, and is the one command that must
    // not resolve the ambient one first — see `invoke::create_store`.
    if let Invocation::Store {
        action: StoreAction::New { path },
    } = &invocation
    {
        match storyhook::invoke::create_store(&cwd, path) {
            Ok(response) => {
                let rendered = output::render_response(&response, json, flags.quiet);
                if !rendered.is_empty() {
                    print!("{rendered}");
                }
                return;
            }
            Err(error) => fail(&error, json),
        }
    }

    // A command that needs no store must not open one, and this is the line
    // that guarantees it — see `invoke::needs_no_store`. Below here every path
    // reaches `open_store`, whose failure took down `story daemon stop`, which
    // is the first step of the remedy a damaged store prints (SH-149).
    if storyhook::invoke::needs_no_store(&invocation) {
        match storyhook::invoke::dispatch_without_store(invocation) {
            Ok(response) => {
                let rendered = output::render_response(&response, json, flags.quiet);
                if !rendered.is_empty() {
                    print!("{rendered}");
                }
                return;
            }
            Err(error) => fail(&error, json),
        }
    }

    // Everything storyhook reads from outside itself, resolved once and passed
    // down. Nothing below this line calls `env::var` for a path or a clock.
    let environment = match storyhook::env::Environment::from_process(flags.store_path.as_deref()) {
        Ok(environment) => environment,
        Err(error) => fail(&error, json),
    };

    // The work goes through the Invoker seam; `json` and `quiet` never do,
    // because rendering is this process's job no matter where the answer
    // came from.
    // A command that reads standard input has it read *here*, before anything
    // is dispatched. This process is the one with a terminal; a daemon has no
    // way to reach it.
    let piped = if storyhook::invoke::reads_stdin(&invocation) {
        match read_stdin() {
            Ok(content) => Some(content),
            Err(error) => fail(&error, json),
        }
    } else {
        None
    };
    // The caller's GitHub credential, read here for exactly the reason the
    // piped stdin above is: this is the process that belongs to the person who
    // typed the command. The daemon's environment is a snapshot of whichever
    // client happened to start it, so reading it there answered a stranger's
    // shell — telling a caller who had exported a token that it was unset, and
    // handing the daemon's own to a caller who had not (SH-153).
    //
    // Asked per-invocation rather than read unconditionally: `story list` has
    // no business carrying a credential, and `needs_github_token` is exhaustive
    // over `Invocation` so that a later verb needing one cannot silently get
    // `None`.
    let github_token = if storyhook::invoke::needs_github_token(&invocation) {
        match env::var(storyhook::domain::secret::GITHUB_TOKEN_VAR) {
            // A blank value is a different mistake from an absent one, and it
            // is refused here rather than becoming a puzzling 401 from GitHub
            // two network calls later.
            Ok(raw) => match storyhook::domain::secret::GithubToken::new(raw) {
                Ok(token) => Some(token),
                Err(error) => fail(&error, json),
            },
            // Absent. The refusal belongs where the work runs, so that the
            // dashboard and a hand-built request meet it too.
            Err(_) => None,
        }
    } else {
        None
    };

    // The two sources are collapsed here, in the only process that can see
    // both: `$STORYHOOK_PROJECT` belongs to the caller's shell, and a daemon's
    // environment is its own. Applying precedence once, at the one site that
    // has both values, is what stops a second layer re-deciding it — the same
    // reason `hook_depth` travels in the request rather than being read from
    // the process the work happens in.
    let selector = storyhook::api::wire::ProjectSelector::resolve(
        flags.project.as_deref(),
        env::var("STORYHOOK_PROJECT").ok().as_deref(),
    );
    // Read before the invocation is moved into the request. See
    // `invoke::failure_is_silent`: `story session-start` answers `{}` rather
    // than putting a diagnosis it cannot avoid into a model's context window.
    let silent_on_failure = storyhook::invoke::failure_is_silent(&invocation);
    let request = InvokeRequest::new(invocation)
        .no_hooks(flags.no_hooks)
        .stdin(piped)
        .project(selector)
        .github_token(github_token);
    let depth = storyhook::event_hooks::depth_from_env();
    // **The CLI's only door.** There was a second — `--local`, which built a
    // `StoreInvoker` here and ran the work in this process — and it is gone
    // (SH-114). `StoreInvoker` survives as the *executor* both remaining
    // callers use: `api/rpc.rs`, which is the daemon running the work this
    // request is about to travel to, and `tui/app.rs`.
    let run = |request: InvokeRequest| {
        HttpInvoker::new(environment.clone(), &cwd)
            .hook_depth(depth)
            .invoke(request)
    };

    // A destructive command answers with what it *would* destroy rather than
    // doing it. Confirming happens here, in the process that has a terminal:
    // the work may run in a daemon, which has no way to reach the user at all.
    let result = match run(request.clone()) {
        Ok(Response::ConfirmationRequired(plan)) => match confirm(&plan, json, flags.quiet) {
            Confirmed::Yes => run(request.forced()),
            Confirmed::No => {
                println!("cancelled; nothing was changed");
                return;
            }
            Confirmed::CannotAsk(error) => Err(error),
        },
        other => other,
    };

    // The silence is applied *here*, around the invoker rather than inside it,
    // and that placement is the whole of it. `HttpInvoker` must keep failing
    // loud — SH-114 established that, and the git hooks depend on their own
    // shell redirection rather than on the CLI going quiet. But the failure this
    // covers is raised by `daemon::lifecycle::ensure` *before* a daemon exists
    // to be asked, so there is no in-daemon layer that could answer it: only the
    // client can.
    let result = match result {
        Err(_) if silent_on_failure => Ok(Response::RawJson(
            storyhook::service::session::SILENT.to_string(),
        )),
        other => other,
    };

    match result {
        Ok(response) => {
            let rendered = output::render_response(&response, json, flags.quiet);
            if !rendered.is_empty() {
                print!("{rendered}");
            }
        }
        Err(error) => fail(&error, json),
    }
}

/// Publishes `--store-path` into `$STORYHOOK_STORE_PATH`, canonicalized.
///
/// **A flag only the first `Environment` knew about would be a flag that stops
/// applying the moment anything re-resolves** — `story daemon status`, the TUI,
/// the git hook a command fires, the daemon this run spawns. `--store-path`
/// means "this invocation, and everything it starts, is in that store", and the
/// only way to say that to a child process is a variable it inherits.
///
/// Canonicalized here so that every reader agrees on one spelling and therefore
/// on one daemon; an unresolvable path fails now, in the process that can
/// explain it, rather than inside a daemon nobody is watching.
///
/// # Safety
///
/// `set_var` is unsound only in the presence of concurrent readers. This runs in
/// `main` before storyhook has started a thread, opened a store or spawned
/// anything, which is the one place in the program where that is guaranteed.
fn publish_store_path(flag: Option<&std::path::Path>, json: bool) {
    let Some(flag) = flag else { return };
    match storyhook::env::canonical_ish(flag) {
        Ok(canonical) => unsafe { env::set_var("STORYHOOK_STORE_PATH", &canonical) },
        Err(error) => fail(&error, json),
    }
}

/// What [`ask_about_a_new_project`] decided.
enum Asked {
    /// Carry on with this invocation — either the original, or a bare `project
    /// new` filled in by the questionnaire.
    ///
    /// Boxed because the other variant is a unit: an `Invocation` is large
    /// enough that carrying one inline would make every `Asked` that size.
    Proceed(Box<Invocation>),
    /// The user declined. Nothing has been created and the exit code is 0.
    Cancelled,
}

/// Turns a bare `story project new` into a fully stated one, by asking.
///
/// **The client is the only process that can do this.** The work runs in a
/// daemon with no terminal and no way to reach one, so the conversation happens
/// here and travels as an ordinary, fully specified request — the same shape a
/// script would have sent. `main.rs` owns the `IsTerminal` decision and the two
/// streams; `service::questionnaire` owns every word of the conversation and
/// never names stdin, which is what keeps the interactive seam a single site.
///
/// Two cases cannot be asked at all, and both are refusals quoting a working
/// non-interactive command rather than assumptions either way:
///
/// * **`--json`.** The contract is one self-describing document on stdout, and
///   a prompt corrupts it for every scripted caller.
/// * **No terminal.** A pipeline, a CI job, an agent. Defaulting a prefix here
///   would be SH-109's silent `SH` wearing a new verb, in the one place nobody
///   would ever notice.
fn ask_about_a_new_project(invocation: Invocation, cwd: &std::path::Path, json: bool) -> Asked {
    use std::io::IsTerminal;
    use storyhook::cli::{NewProjectRequest, ProjectAction};
    use storyhook::service::questionnaire::{self, Answered, Surroundings};

    if !matches!(
        invocation,
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Ask)
        }
    ) {
        return Asked::Proceed(Box::new(invocation));
    }

    let refuse = |why: &str| -> ! {
        fail(
            &storyhook::error::AppError::Validation(format!(
                "`story project new` needs somebody to ask, and {why}.\n\nState the answers \
                 instead — `--prefix` is the only required one:\n\n  story project new --prefix \
                 <PREFIX> [--name <NAME>] [--no-attach]"
            )),
            json,
        )
    };
    if json {
        refuse("--json cannot carry a prompt");
    }
    if !std::io::stdin().is_terminal() {
        refuse("there is no terminal here");
    }

    let surroundings = Surroundings {
        dir: cwd.to_path_buf(),
        origin: storyhook::service::project::origin_of(cwd).map(|url| url.raw().to_string()),
        agents_md_exists: cwd.join(storyhook::service::project::AGENTS_MD).is_file(),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stderr();
    match questionnaire::ask(&surroundings, &mut input, &mut out) {
        Ok(Answered::Create(spec)) => Asked::Proceed(Box::new(Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Stated(*spec)),
        })),
        Ok(Answered::Cancelled) => Asked::Cancelled,
        Err(error) => fail(&error, json),
    }
}

/// The answer to a confirmation prompt.
enum Confirmed {
    Yes,
    No,
    /// There is no terminal to ask, or asking would corrupt the output.
    CannotAsk(storyhook::error::AppError),
}

/// Asks the user to confirm a destructive plan by typing the token it names.
///
/// A typed token rather than `[y/N]`, because the weight of the gate should
/// match the weight of the act: one keystroke is right for "reopen this deleted
/// story" and wrong for "erase every event this project has".
///
/// Two cases cannot be asked at all, and both are refusals naming `--force`
/// rather than assumptions either way:
///
/// * **`--json`.** The contract is one self-describing document on stdout, and
///   a prompt written there corrupts it for every scripted caller.
/// * **No terminal.** A pipeline, a CI job, a hook. Defaulting to "no" would be
///   safe and silent, which is how a script appears to work for months while
///   never doing the thing it was written to do.
///
/// `--quiet` deliberately does *not* suppress the warning. It suppresses
/// successful output, and this is a question.
fn confirm(plan: &storyhook::output::ConfirmationPlan, json: bool, quiet: bool) -> Confirmed {
    use std::io::{IsTerminal, Write};

    let token = plan.token();

    // The refusal carries the *whole* plan, not just the headline counts. A
    // caller being told to re-run with --force is being asked to authorize
    // something sight-unseen otherwise, and the detail is the part they cannot
    // reconstruct — which checkouts are known and which AGENTS.md is being kept
    // rather than removed, or which stories still claim an edge into the one
    // about to go.
    let refuse = |why: &str| {
        Confirmed::CannotAsk(storyhook::error::AppError::Validation(format!(
            "this would permanently delete `{token}`, and {why}.\n\n{}\nRe-run with --force to \
             confirm.",
            storyhook::output::render_confirmation_plan(plan),
        )))
    };
    if json {
        return refuse("--json cannot carry a prompt");
    }
    if !std::io::stdin().is_terminal() {
        return refuse("there is no terminal to confirm at");
    }

    let _ = quiet;
    eprint!("{}", storyhook::output::render_confirmation_plan(plan));
    eprint!("Type `{token}` to confirm: ");
    let _ = std::io::stderr().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Confirmed::No;
    }
    if answer.trim() == token {
        Confirmed::Yes
    } else {
        Confirmed::No
    }
}

/// This process's standard input, read to the end.
fn read_stdin() -> Result<String, storyhook::error::AppError> {
    use std::io::Read as _;
    let mut buffer = String::new();
    std::io::stdin()
        .read_to_string(&mut buffer)
        .map_err(|e| storyhook::error::AppError::Storage(format!("failed to read stdin: {e}")))?;
    Ok(buffer)
}

/// The port a foreground `--serve` was asked to bind, if this invocation is one.
///
/// `Some(None)` means "serve, on whatever port the environment prefers";
/// `Some(Some(port))` names one. Both spellings land here so that there is
/// exactly one place in the program where the daemon is started in the
/// foreground.
fn foreground_serve_port(invocation: &Invocation) -> Option<Option<u16>> {
    match invocation {
        Invocation::Daemon {
            action: DaemonAction::Serve { port },
        } => Some(*port),
        Invocation::Web {
            action: WebAction::Serve { port },
        } => Some(Some(*port)),
        _ => None,
    }
}
