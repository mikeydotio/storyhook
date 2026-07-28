use std::env;
use std::process;

use storyhook::cli::{self, Invocation, WebAction};
use storyhook::invoke::{InvokeRequest, Invoker, LegacyInvoker, StoreInvoker};
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

    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let error = storyhook::error::AppError::Storage(format!(
                "failed to resolve current directory: {error}"
            ));
            fail(&error, json);
        }
    };

    // The work goes through the Invoker seam; `json` and `quiet` never do,
    // because rendering is this process's job no matter where the answer
    // came from.
    let request = InvokeRequest::new(invocation).no_hooks(no_hooks);
    let result = match selected_backend() {
        Backend::Legacy => LegacyInvoker::new(&cwd).invoke(request),
        Backend::Local => match open_store() {
            Ok(store) => StoreInvoker::new(&store, &cwd)
                .hook_depth(hook_depth())
                .invoke(request),
            Err(error) => Err(error),
        },
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

/// Which stack serves this invocation.
///
/// The strangler's switch. Both backends answer the same
/// `Response`/`AppError` envelope, so selecting between them changes where the
/// work happens and not what the answer means — which is the property the whole
/// port is built to preserve, and the property `STORYHOOK_INVOKER=local` exists
/// to *test*: the integration suite can be run end to end against the store
/// long before the store is what anybody's data is in.
enum Backend {
    /// `.storyhook/` in the repository — today's default.
    Legacy,
    /// The global store, in this process. Named `local` because the daemon
    /// wave adds a third backend that is the same store over a socket, and
    /// `local` is what distinguishes them.
    Local,
}

/// Reads `STORYHOOK_INVOKER`, defaulting to the legacy stack.
///
/// An unrecognised value is a *failure*, not a fallback: a typo that silently
/// ran the legacy path would make a green store-leg run mean nothing at all.
fn selected_backend() -> Backend {
    match env::var("STORYHOOK_INVOKER").as_deref() {
        Ok("local") => Backend::Local,
        Ok("legacy") | Err(_) => Backend::Legacy,
        Ok(other) => {
            eprintln!("error: STORYHOOK_INVOKER must be `legacy` or `local`, not `{other}`");
            process::exit(2);
        }
    }
}

/// Opens and migrates the global store.
fn open_store() -> Result<storyhook::store::SqliteStore, storyhook::error::AppError> {
    use storyhook::store::Store as _;
    let store = storyhook::store::SqliteStore::open(storyhook::paths::store_path()?)?;
    store.migrate()?;
    Ok(store)
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
