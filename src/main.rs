use std::env;
use std::process;

#[cfg(feature = "github-pr")]
use storyhook::cli::GithubAuthAction;
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
/// plugins/story/bin/story.sh, whose CAS claim reads a conflict envelope
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

/// Runs `invoke` on its own thread, and gives up waiting on it after
/// `deadline` — `--deadline`'s whole implementation.
///
/// # Abandoned, not cancelled
///
/// There is no way to interrupt an HTTP call already sent: the daemon may
/// commit the write it carried before this process stops waiting for the
/// reply, or after, and there is no third state to report. So this never
/// retries and never claims to know whether the command ran — the same
/// obligation `HttpInvoker::send`'s own "may or may not have run" message
/// keeps. The spawned thread is not joined: it is left to finish (or to sit
/// blocked in a wait this process no longer cares about) and dies with the
/// process, same as any other detached work here.
///
/// # Why a thread rather than a socket-level timeout
///
/// The wait this bounds is not one HTTP call, it is `lifecycle::ensure` (up
/// to `SPAWN_LOCK_DEADLINE`, 30s — starting a daemon, or queueing behind
/// whoever else is starting one) *and then* `HttpInvoker::send` (up to
/// `SERVED_DEADLINE`, 120s — the daemon actually running the command). A
/// deadline on the socket alone would leave the first of those two
/// unbounded, which is the exact defect SH-182 is about.
fn run_with_deadline(
    invoke: impl FnOnce() -> Result<Response, storyhook::error::AppError> + Send + 'static,
    deadline: std::time::Duration,
) -> Result<Response, storyhook::error::AppError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(invoke());
    });
    rx.recv_timeout(deadline).unwrap_or_else(|_| {
        Err(storyhook::error::AppError::Storage(format!(
            "gave up after {}s waiting for storyhook, as --deadline asked. This command was \
             not cancelled: the daemon may still be running it, and storyhook will not repeat \
             it. Check with `story show` or `story list`, then try again if it did not.",
            deadline.as_secs(),
        )))
    })
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

    // And the credential, in the same window and for a sharper version of the
    // same reason (SH-153). `take_credentials` reads **and removes** in one
    // call: from here on the GitHub token exists only as this value, so no
    // child this process starts can inherit it — and the daemon, which would
    // otherwise hold it for life and hand it to every event-hook script, the
    // dashboard's dispatch child and `claude`, never sees it at all.
    //
    // Taken rather than scrubbed, and the two are not the same: `main` is also
    // the one process that legitimately *reads* this variable, so a bare scrub
    // beside the git one would leave the client with nothing to send. Reading
    // and removing together is what makes the ordering impossible to get wrong.
    let credentials = storyhook::env::secrets::take_credentials();

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

    // `mcp` is dispatched here for the same reason `tui` is just above: it
    // is a mode of this one binary, not an `Invocation` a daemon runs, so it
    // never reaches `parse_invocation`. It cannot be a separate binary
    // either — `daemon::lifecycle::DaemonInfo::is_this_binary` identifies a
    // usable daemon by its executable's path *and* mtime, so a second
    // binary would evict the daemon this one is using on every call, and be
    // evicted back on the next (`src/mcp/mod.rs`'s own doc comment).
    if filtered_args.first().is_some_and(|arg| arg == "mcp")
        && !cli::is_help_request(&filtered_args)
    {
        let cwd = env::current_dir().unwrap_or_else(|e| {
            eprintln!("error: failed to resolve current directory: {e}");
            process::exit(1);
        });
        let environment =
            match storyhook::env::Environment::from_process(flags.store_path.as_deref()) {
                Ok(environment) => environment,
                Err(error) => fail(&error, json),
            };
        // Resolved once, here, exactly as the ordinary command path below
        // resolves it — never re-read per tool call. Unlike `$STORYHOOK_PROJECT`
        // and `$STORYHOOK_ACTOR`, this is not something a tool call names
        // explicitly, because it does not vary within one process's life any
        // more than it does for a single ordinary command.
        let depth = storyhook::event_hooks::depth_from_env();
        let server = storyhook::mcp::McpServer::new(environment, cwd, depth);
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        if let Err(e) = server.run(stdin.lock(), stdout.lock()) {
            eprintln!("error: story mcp: {e}");
            process::exit(1);
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
            Some(port) => environment.daemon_port(port),
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

    // `story github-auth` touches the OS keychain — a machine-level resource
    // with no project — and `Login`'s masked prompt can only run where this
    // process has a terminal. Answered entirely here, before a store or even
    // a daemon round trip is ever considered; see
    // `invoke::dispatch_without_store`'s own refusal for what happens if a
    // `GithubAuth` invocation is ever dispatched any other way.
    if let Invocation::GithubAuth { action } = &invocation {
        #[cfg(feature = "github-pr")]
        let result = storyhook::env::Environment::from_process(flags.store_path.as_deref())
            .and_then(|environment| run_github_auth(action, &environment, json));
        #[cfg(not(feature = "github-pr"))]
        let result: Result<Response, storyhook::error::AppError> = {
            let _ = action;
            Err(storyhook::error::AppError::Usage(
                "github-auth requires the `github-pr` feature. Rebuild with: cargo install \
                 storyhook --features github-pr"
                    .to_string(),
            ))
        };
        match result {
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
    // Validated here rather than where it was taken, because a blank value is a
    // different mistake from an absent one and has to be reported with the
    // rendering the caller asked for — which was not known at the top of
    // `main`. An *absent* credential is not an error at all here: the refusal
    // belongs where the work runs, so that the dashboard and a hand-built
    // request meet it too.
    let github_token = if storyhook::invoke::needs_github_token(&invocation) {
        match credentials.github_token() {
            Ok(token) => token,
            Err(error) => fail(&error, json),
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
    // Read here for the same reason `$STORYHOOK_PROJECT` is: it belongs to the
    // caller's shell, and a daemon reading it directly would label every write
    // with whatever the process that happened to start it had exported
    // (SH-246). Refused rather than sanitized, and refused *before* the request
    // is built, so a malformed label costs the caller nothing but a fixable
    // error message — the alternative is a store that quietly holds something
    // the caller did not say.
    let actor = match env::var(storyhook::domain::provenance::ACTOR_VAR) {
        Ok(raw) => match storyhook::domain::provenance::ActorLabel::parse(&raw) {
            Ok(actor) => actor,
            Err(error) => fail(
                &storyhook::error::AppError::Validation(error.to_string()),
                json,
            ),
        },
        // A variable that is absent, or holds bytes this platform cannot
        // decode, is simply no declaration. Refusing on non-UTF-8 would fail
        // commands over a label nothing needs.
        Err(_) => None,
    };
    let request = InvokeRequest::new(invocation)
        .no_hooks(flags.no_hooks)
        .stdin(piped)
        .project(selector)
        .github_token(github_token)
        .actor(actor);
    let depth = storyhook::event_hooks::depth_from_env();
    // **The CLI's only door.** There was a second — `--local`, which built a
    // `StoreInvoker` here and ran the work in this process — and it is gone
    // (SH-114). `StoreInvoker` survives as the *executor* both remaining
    // callers use: `api/rpc.rs`, which is the daemon running the work this
    // request is about to travel to, and `tui/app.rs`.
    // `--deadline` bounds this call and this call alone: `confirm` below
    // blocks on a human at a terminal, not on storyhook, and is never
    // wrapped. Cloning `environment` and `cwd` here rather than moving them
    // is what lets `run` be called more than once — once for the first
    // attempt, again after a confirmation — each call getting a fresh,
    // independently timed invocation.
    let run = |request: InvokeRequest| {
        let environment = environment.clone();
        let cwd = cwd.clone();
        let invoke = move || {
            HttpInvoker::new(environment, &cwd)
                .hook_depth(depth)
                .invoke(request)
        };
        match flags.deadline {
            Some(deadline) => run_with_deadline(invoke, deadline),
            None => invoke(),
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

    // The silence is applied *here*, around the invoker rather than inside it,
    // and that placement is the whole of it. `HttpInvoker` must keep failing
    // loud — SH-114 established that, and the git hooks depend on their own
    // shell redirection rather than on the CLI going quiet. But the failure this
    // covers is raised by `daemon::lifecycle::ensure` *before* a daemon exists
    // to be asked, so there is no in-daemon layer that could answer it: only the
    // client can.
    //
    // `unavailable` rather than a bare `SILENT` (SH-182): the same window this
    // covers — a spawn-lock wait, or a `--deadline` this invocation set —
    // used to collapse into silence indistinguishable from "no project here",
    // which is what let a session start with no storyhook context and nobody
    // told. `unavailable` answers that only when there is nothing to say.
    let result = match result {
        Err(_) if silent_on_failure => Ok(Response::RawJson(
            storyhook::service::session::unavailable(&cwd),
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

/// Asks the user to confirm a plan — typing the token it names back for a
/// one-way door, or a plain `[y/N]` when the plan's own
/// [`requires_typed_confirmation`](storyhook::output::ConfirmationPlan::requires_typed_confirmation)
/// says one keystroke is enough.
///
/// The weight of the gate matches the weight of the act: a typed token is
/// right for "erase every event this project has" and wrong for "reopen this
/// deleted story" — the latter is undone by a plain `story delete` again, so
/// asking for more than one keystroke would be asking the terminal to prove
/// something the act itself does not require.
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

    // The refusal carries the *whole* plan, not just the headline counts. A
    // caller being told to re-run with --force is being asked to authorize
    // something sight-unseen otherwise, and the detail is the part they cannot
    // reconstruct — which checkouts are known and which AGENTS.md is being kept
    // rather than removed, or which stories still claim an edge into the one
    // about to go.
    let refuse = |why: &str| {
        Confirmed::CannotAsk(storyhook::error::AppError::Validation(format!(
            "{}, and {why}.\n\n{}\nRe-run with --force to confirm.",
            plan.headline(),
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

    let mut answer = String::new();
    let accepted = if plan.requires_typed_confirmation() {
        let token = plan.token();
        eprint!("Type `{token}` to confirm: ");
        let _ = std::io::stderr().flush();
        std::io::stdin().read_line(&mut answer).is_ok() && answer.trim() == token
    } else {
        eprint!("Reopen (undelete) this deleted story? [y/N] ");
        let _ = std::io::stderr().flush();
        std::io::stdin().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };

    if accepted {
        Confirmed::Yes
    } else {
        Confirmed::No
    }
}

/// Prompts for a GitHub Personal Access Token, for `story github-auth login`
/// (SH-212).
///
/// Two cases cannot be asked at all, and both are refusals — the same two
/// `why` clauses [`ask_about_a_new_project`] uses:
///
/// * **`--json`.** The contract is one self-describing document on stdout,
///   and a prompt corrupts it for every scripted caller.
/// * **No terminal.** A pipeline, a CI job, an agent. `login` grants the
///   daemon unattended access, so unlike an ordinary command there is no
///   silent, safe default to fall back to here — a script that wanted this
///   would have to say so as explicitly as a human typing the token does.
///
/// The consent banner prints every time, not only on a fresh grant: `login`
/// is also how an existing token gets rotated, and the grant it describes is
/// exactly what is about to be renewed.
#[cfg(feature = "github-pr")]
fn ask_github_token(
    json: bool,
) -> Result<storyhook::domain::secret::GithubToken, storyhook::error::AppError> {
    use std::io::IsTerminal;

    let refuse = |why: &str| -> storyhook::error::AppError {
        storyhook::error::AppError::Validation(format!(
            "`story github-auth login` needs somebody to ask, and {why}.\n\nThe daemon's \
             background poll needs a token stored by an interactive `login` — there is no \
             non-interactive form of this command. `story pr-check` still works with \
             STORYHOOK_GITHUB_TOKEN set, run by hand or from a scheduler."
        ))
    };
    if json {
        return Err(refuse("--json cannot carry a prompt"));
    }
    if !std::io::stdin().is_terminal() {
        return Err(refuse("there is no terminal here"));
    }

    eprintln!(
        "This stores a GitHub Personal Access Token in your OS keychain and grants the \
         storyhook daemon unattended background access to check pull requests and close \
         linked stories on your behalf, until you run `story github-auth logout`.\n"
    );
    let raw = dialoguer::Password::new()
        .with_prompt("GitHub Personal Access Token")
        .interact()
        .map_err(|e| storyhook::error::AppError::GithubAuth(format!("reading the token: {e}")))?;
    storyhook::domain::secret::GithubToken::new(raw)
}

/// Answers `story github-auth login|status|logout` (SH-212) — entirely in
/// this process. The OS keychain is a machine-level resource, not project
/// data, so this needs `env` (for the store's own key, which names the
/// keychain entry) but never a store or the daemon.
#[cfg(feature = "github-pr")]
fn run_github_auth(
    action: &GithubAuthAction,
    env: &storyhook::env::Environment,
    json: bool,
) -> Result<Response, storyhook::error::AppError> {
    use storyhook::github::credential_store;

    let account = env.store().key();
    match action {
        GithubAuthAction::Login => {
            let token = ask_github_token(json)?;
            let store = credential_store::default_credential_store()?;
            credential_store::login(&store, &account, &token)?;
            Ok(Response::Message(
                "GitHub credential stored. The daemon's background poll will start using it \
                 on its next tick; run `story github-auth logout` to revoke it."
                    .to_string(),
            ))
        }
        GithubAuthAction::Status => match credential_store::default_credential_store() {
            Ok(store) => match credential_store::read(&store, &account)? {
                Some(_) => Ok(Response::Message(
                    "a GitHub credential is stored; the daemon's background poll can use it."
                        .to_string(),
                )),
                None => Ok(Response::Message(
                    "no GitHub credential is stored. Run `story github-auth login` to add one."
                        .to_string(),
                )),
            },
            // Diagnostic, not a failure: `status` degrades to reporting why
            // rather than exiting non-zero, the same posture the daemon's own
            // poll thread takes on a missing keychain backend.
            Err(error) => Ok(Response::Message(format!(
                "no keychain backend is available on this platform, so nothing could be \
                 stored: {error}"
            ))),
        },
        GithubAuthAction::Logout => {
            let store = credential_store::default_credential_store()?;
            credential_store::logout(&store, &account)?;
            Ok(Response::Message(
                "GitHub credential removed. The daemon's background poll stops using it on \
                 its next tick."
                    .to_string(),
            ))
        }
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
        } => Some(*port),
        _ => None,
    }
}
