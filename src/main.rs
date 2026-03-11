use std::env;
use std::process;

use story::app;
use story::cli::{self, CliOptions};
use story::output;

fn main() {
    let raw_args = env::args().skip(1).collect::<Vec<_>>();
    let (json, quiet, filtered_args) = cli::split_global_flags(&raw_args);

    let invocation = match cli::parse_invocation(&filtered_args) {
        Ok(invocation) => invocation,
        Err(error) => {
            print!("{}", output::render_error(&error, json));
            process::exit(error.exit_code());
        }
    };

    let options = CliOptions {
        json,
        quiet,
        invocation,
    };

    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let error = story::error::AppError::Storage(format!(
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
