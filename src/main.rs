use std::env;
use std::process;

use storyhook::cli::{self, DaemonAction, Invocation, StoreAction, WebAction};
use storyhook::invoke::{HttpInvoker, InvokeRequest, Invoker, StoreInvoker};
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

    refuse_unknown_backend(json);

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
    // way to reach it, and a `--local` run gets the same content by the same
    // route rather than by a second code path.
    let piped = if storyhook::invoke::reads_stdin(&invocation) {
        match read_stdin() {
            Ok(content) => Some(content),
            Err(error) => fail(&error, json),
        }
    } else {
        None
    };
    let request = InvokeRequest::new(invocation)
        .no_hooks(flags.no_hooks)
        .stdin(piped);
    let depth = storyhook::event_hooks::depth_from_env();
    let run = |request: InvokeRequest| {
        if run_locally(&flags, &request.invocation) {
            match storyhook::invoke::open_store(&environment) {
                Ok(store) => StoreInvoker::new(&store, &cwd, environment.clone())
                    .hook_depth(depth)
                    .invoke(request),
                Err(error) => Err(error),
            }
        } else {
            HttpInvoker::new(environment.clone(), &cwd)
                .hook_depth(depth)
                .invoke(request)
        }
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

/// Whether this command runs in this process rather than through the daemon.
///
/// `--local`, or `STORYHOOK_INVOKER=local` — the same mode, spelled for a flag
/// and for an environment that a whole script inherits. It is a **permanent,
/// documented mode**, not a fallback: git hooks and CI want it, because
/// spawning a daemon inside `prepare-commit-msg` is hostile and a CI job that
/// starts one for a single command pays for something it never reuses. SQLite's
/// WAL is built for several processes on one database, so this is the same
/// services with one hop fewer.
///
/// What it is *not* is automatic. A daemon that cannot be reached is an error
/// naming this flag, never a silent switch to it — a silent fallback is how a
/// dashboard goes stale while every command keeps succeeding, which is the
/// failure shape this whole rearchitecture exists to remove.
fn run_locally(flags: &cli::GlobalFlags, invocation: &Invocation) -> bool {
    flags.local
        || matches!(env::var("STORYHOOK_INVOKER").as_deref(), Ok("local"))
        || never_through_the_daemon(invocation)
}

/// Commands that must run in this process whatever the invoker is set to.
///
/// Three families, for three different reasons:
///
/// * **The daemon's own lifecycle.** `story daemon stop` asking the daemon to
///   run `story daemon stop` is circular, and auto-spawn makes it worse: the
///   command that stops a daemon would start one first. Found by
///   `make test-daemon`, where it did not fail — it hung.
/// * **Self-update.** `story update` replaces the binary the daemon is running.
///   Doing that from inside that daemon is asking it to saw the branch off.
/// * **Store creation.** `story store new` is about a store that does not exist
///   yet; there is no daemon for it to be dispatched to, and the daemon for
///   some *other* store has no business creating files on its behalf.
/// * **Pure functions of compiled-in text.** `--help` and `--version` are
///   answered from a string constant. Starting a background process to print
///   one — which is what a first-ever `story --help` would do — is a bad first
///   impression and buys nothing.
fn never_through_the_daemon(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Daemon { .. }
            | Invocation::Web { .. }
            | Invocation::Store { .. }
            | Invocation::Update { .. }
            | Invocation::Help
            | Invocation::HelpTopic { .. }
            | Invocation::HelpCompact
            | Invocation::HelpAll
            | Invocation::Version
    )
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

/// Refuses a `STORYHOOK_INVOKER` value that names no backend, loudly.
///
/// Two are real: `daemon` (the default, and what an unset variable means) and
/// `local`. `legacy` was the strangler's switch while two stacks existed, and
/// anybody who still has it exported has a shell — or a script, or a CI job —
/// that believes it is reading `.storyhook/`. A variable that silently did
/// nothing would be worse than no variable at all.
fn refuse_unknown_backend(json: bool) {
    match env::var("STORYHOOK_INVOKER").as_deref() {
        Ok("local") | Ok("daemon") | Err(_) => {}
        Ok(other) => {
            let error = storyhook::error::AppError::Usage(format!(
                "STORYHOOK_INVOKER=`{other}` is not a storyhook backend. The choices are \
                 `daemon` (the default) and `local`, which runs the command in this process. \
                 Story data lives in storyhook's own store, not in `.storyhook/`; run \
                 `story migrate` in any repository whose `.storyhook/` directory has not been \
                 imported yet."
            ));
            fail(&error, json);
        }
    }
}
