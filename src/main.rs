use std::env;
use std::process;

use storyhook::app;
use storyhook::cli::{self, CliOptions, Invocation, WebAction};
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

    if raw_args.first().is_some_and(|arg| arg == "tui") {
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

    // Web server foreground mode: `story web --serve --port N`. Runs the
    // HTTP server directly (used by the daemon spawner), against the
    // registry at its default location — the one piece of storyhook state
    // that lives outside any single repo's `.storyhook/`.
    if let Invocation::Web {
        action: WebAction::Serve { port },
    } = invocation
    {
        let registry_path = match storyhook::registry::default_registry_path() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("error: {e}");
                process::exit(e.exit_code());
            }
        };
        if let Err(e) = storyhook::web::start_server(&registry_path, port) {
            eprintln!("error: {e}");
            process::exit(e.exit_code());
        }
        return;
    }

    let options = CliOptions {
        json,
        quiet,
        no_hooks,
        invocation,
    };

    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let error = storyhook::error::AppError::Storage(format!(
                "failed to resolve current directory: {error}"
            ));
            fail(&error, json);
        }
    };

    match app::run(&cwd, options.clone()) {
        Ok(response) => {
            let rendered = output::render_response(&response, options.json, options.quiet);
            if !rendered.is_empty() {
                print!("{rendered}");
            }
        }
        Err(error) => fail(&error, options.json),
    }
}
