use std::env;
use std::process;

use storyhook::app;
use storyhook::cli::{self, CliOptions, Invocation, WebAction};
use storyhook::output;

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
        Err(error) => {
            print!("{}", output::render_error(&error, json));
            process::exit(error.exit_code());
        }
    };

    // Web server foreground mode: `story web --serve --port N --root /path`
    // Runs the HTTP server directly (used by the daemon spawner).
    if let Invocation::Web {
        action: WebAction::Serve { port, ref root },
    } = invocation
    {
        if let Err(e) = storyhook::web::start_server(root, port) {
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
            print!("{}", output::render_error(&error, json));
            process::exit(error.exit_code());
        }
    };

    match app::run(&cwd, options.clone()) {
        Ok(response) => {
            let rendered = output::render_response(&response, options.json, options.quiet);
            if !rendered.is_empty() {
                print!("{rendered}");
            }
        }
        Err(error) => {
            print!("{}", output::render_error(&error, options.json));
            process::exit(error.exit_code());
        }
    }
}
