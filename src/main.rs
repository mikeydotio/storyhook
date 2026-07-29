use std::env;
use std::process;

use storyhook::cli::{self, DaemonAction, Invocation, WebAction};
use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
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

    let (json, quiet, no_hooks, filtered_args) = cli::split_global_flags(&raw_args);

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

    refuse_retired_backend(json);

    // Everything storyhook reads from outside itself, resolved once and passed
    // down. Nothing below this line calls `env::var` for a path or a clock.
    let environment = match storyhook::env::Environment::from_process() {
        Ok(environment) => environment,
        Err(error) => fail(&error, json),
    };

    // The work goes through the Invoker seam; `json` and `quiet` never do,
    // because rendering is this process's job no matter where the answer
    // came from.
    let request = InvokeRequest::new(invocation).no_hooks(no_hooks);
    let result = match storyhook::invoke::open_store(&environment) {
        Ok(store) => StoreInvoker::new(&store, &cwd, environment.clone())
            .hook_depth(hook_depth())
            .invoke(request),
        Err(error) => Err(error),
    };

    match result {
        Ok(response) => {
            let rendered = output::render_response(&response, json, quiet);
            if !rendered.is_empty() {
                print!("{rendered}");
            }
        }
        Err(error) => fail(&error, json),
    }
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

/// Refuses `STORYHOOK_INVOKER=legacy`, loudly.
///
/// The variable was the strangler's switch while two stacks existed. There is
/// one now, and a variable that silently did nothing would be worse than no
/// variable at all: anybody who still has `legacy` exported has a shell — or a
/// script, or a CI job — that believes it is reading `.storyhook/`, and the one
/// useful thing storyhook can do is say where the data went.
///
/// `local` is still accepted, and is now what every invocation does anyway.
fn refuse_retired_backend(json: bool) {
    match env::var("STORYHOOK_INVOKER").as_deref() {
        Ok("local") | Err(_) => {}
        Ok(other) => {
            let error = storyhook::error::AppError::Usage(format!(
                "STORYHOOK_INVOKER=`{other}` is not a storyhook backend. Story data lives in \
                 storyhook's own store now, not in `.storyhook/`; there is no legacy backend to \
                 select. Unset the variable, and run `story migrate` in any repository whose \
                 `.storyhook/` directory has not been imported yet."
            ));
            fail(&error, json);
        }
    }
}

/// How deep inside an event hook this process is running.
///
/// A hook shells out to `story`, and the child must not fire the hook that
/// spawned it. `event_hooks` sets the variable on the child; anything it cannot
/// parse counts as "inside a hook", because the safe answer to an ambiguous
/// depth is to fire nothing.
fn hook_depth() -> u32 {
    match env::var("STORYHOOK_HOOK_DEPTH") {
        Ok(raw) => raw.trim().parse().unwrap_or(1),
        Err(_) => 0,
    }
}
