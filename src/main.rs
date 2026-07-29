use std::env;
use std::process;

use storyhook::cli::{self, DaemonAction, Invocation, WebAction};
use storyhook::invoke::{HttpInvoker, InvokeRequest, Invoker, StoreInvoker};
use storyhook::output;

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

    // `tui` is dispatched here, ahead of parsing, so `story tui --help` would
    // launch the interactive UI instead of explaining it; the help request
    // falls through to the parser, which answers it like any other verb's.
    if raw_args.first().is_some_and(|arg| arg == "tui") && !cli::is_help_request(&raw_args) {
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

    let (flags, filtered_args) = cli::split_global_flags(&raw_args);
    let json = flags.json;

    let invocation = match cli::parse_invocation(&filtered_args) {
        Ok(invocation) => invocation,
        Err(error) => fail(&error, json),
    };

    // Foreground daemon mode: `story daemon --serve` (and its `story web
    // --serve` alias). Runs the daemon in this process — what the background
    // spawner execs and what a launchd agent runs — so it never returns.
    if let Some(port) = foreground_serve_port(&invocation) {
        let environment = match storyhook::env::Environment::from_process() {
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

    // Everything storyhook reads from outside itself, resolved once and passed
    // down. Nothing below this line calls `env::var` for a path or a clock.
    let environment = match storyhook::env::Environment::from_process() {
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
    let result = if run_locally(&flags, &request.invocation) {
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
/// * **Pure functions of compiled-in text.** `--help` and `--version` are
///   answered from a string constant. Starting a background process to print
///   one — which is what a first-ever `story --help` would do — is a bad first
///   impression and buys nothing.
fn never_through_the_daemon(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Daemon { .. }
            | Invocation::Web { .. }
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
