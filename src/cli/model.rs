//! Authoritative command registration and option grammar (SH-898).
//!
//! Registration creates both the finite command identity and its parser binding.
//! Help and flag validation resolve this same identity; aliases do not acquire a
//! separate grammar. Existing parsers retain their legacy acceptance semantics.
use super::*;

/// The entry boundary; it is not an authorization or visibility classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum FamilyHandler {
    /// Ordinary invocation parsing (including local invocation handlers).
    Parsed,
    /// Raw local GitHub protocol; global flags belong to the delegated tool.
    Github,
    /// Terminal UI after global option parsing.
    Tui,
}

/// Closed set of handlers that run before global option parsing.
pub enum BeforeGlobals {
    Github,
}
/// Closed set of handlers that run after globals and before invocation parsing.
pub enum BeforeInvocation {
    Tui,
}

/// No project, store, daemon, environment or helper is consulted for routing.
pub fn before_globals(args: &[String]) -> Option<BeforeGlobals> {
    match CommandId::find(args.first()?)?.handler() {
        FamilyHandler::Github => Some(BeforeGlobals::Github),
        FamilyHandler::Parsed | FamilyHandler::Tui => None,
    }
}

/// Preserve the existing terminal help precedence, including legacy terminators.
pub fn before_invocation(args: &[String]) -> Option<BeforeInvocation> {
    if super::is_help_request(args) {
        return None;
    }
    match CommandId::find(args.first()?)?.handler() {
        FamilyHandler::Tui => Some(BeforeInvocation::Tui),
        FamilyHandler::Parsed | FamilyHandler::Github => None,
    }
}

macro_rules! commands {
    ($( $variant:ident [$($name:literal),+] $handler:ident ($arg:ident) => $body:expr, )*) => {
        /// Every registered command family, including early local handlers.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
        pub enum CommandId { $( $variant, )* }
        impl CommandId {
            /// Complete registration order, shared by dispatch and discovery.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            /// Canonical spelling first, followed by runnable compatibility aliases.
            pub const fn names(self) -> &'static [&'static str] {
                match self { $(Self::$variant => &[$($name),+],)* }
            }
            /// Resolve a spelling through the registration, not by probing a parser.
            pub fn find(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|command| command.names().contains(&name))
            }
            /// Execute exactly the parser bound by this command registration.
            pub(super) fn parse(self, args: &[String]) -> Result<Invocation, AppError> {
                match self { $(Self::$variant => { let $arg = args; let _ = $arg; $body },)* }
            }
            /// Entry routing is supplied by the same registration as the parser.
            pub const fn handler(self) -> FamilyHandler {
                match self { $(Self::$variant => FamilyHandler::$handler,)* }
            }
            /// The canonical help topic; missing topics retain general-help fallback.
            pub fn help_topic(self) -> &'static str { self.names()[0] }
        }
    }
}

fn unknown(name: &str) -> Result<Invocation, AppError> {
    Err(AppError::Usage(format!(
        "unknown command `{name}`. Run `story --help` for usage."
    )))
}

commands! {
    HelpFlag ["-h", "--help"] Parsed (args) => Ok(Invocation::Help),
    VersionFlag ["-V", "--version"] Parsed (args) => Ok(Invocation::Version),
    Help ["help"] Parsed (args) => parse_help(args),
    Mcp ["mcp"] Parsed (args) => Err(AppError::Usage(
            "`story mcp` is retired. Use CLI commands with --json instead. \
             Remove the storyhook MCP server from your host configuration. \
             Run `story help agent-guide` and `story help json-format` for migration guidance."
                .into(),
        )),
    Update ["update"] Parsed (args) => parse_update(args),
        // Not left to fall through to `unknown command`. Five years of
        // documents, this repo's own plugin skill, and every agent that has
        // ever seen storyhook all say `story init`; the least useful thing to
        // tell any of them is that no such command exists.
    Init ["init"] Parsed (args) => Err(AppError::Usage(
            "`story init` is now `story project new`.\n\nThe project verbs moved into one \
             group: `story project new`, `story project list`, `story project delete`.\n\n  \
             story project new --prefix <PREFIX>"
                .to_string(),
        )),
    Project ["project"] Parsed (args) => parse_project(args),
    DispatchPolicy ["dispatch-policy"] Parsed (args) => dispatch_policy::parse(args),
    New ["new"] Parsed (args) => parse_new(args),
    State ["state"] Parsed (args) => parse_state(args),
    List ["list"] Parsed (args) => parse_list(args),
    Next ["next"] Parsed (args) => parse_next(args),
    Claim ["claim"] Parsed (args) => parse_claim(args),
    Unclaim ["unclaim"] Parsed (args) => parse_unclaim(args),
    Reset ["reset"] Parsed (args) => {
            let mut id = None;
            let mut force = false;
            let mut dry_run = false;
            for arg in &args[1..] {
                match arg.as_str() {
                    "--force" if !force => force = true,
                    "--dry-run" if !dry_run => dry_run = true,
                    value if !value.starts_with('-') && id.is_none() => id = Some(value.to_owned()),
                    _ => {
                        return Err(AppError::Usage(
                            usage::U103.into(),
                        ));
                    }
                }
            }
            let id = id.ok_or_else(|| {
                AppError::Usage(usage::U103.into())
            })?;
            let caller = crate::service::reset::ResetCaller::capture();
            if dry_run {
                Ok(Invocation::ResetPreview { id, caller })
            } else {
                Ok(Invocation::Reset { id, force, caller })
            }
        },
    Internal ["internal"] Parsed (args) => parse_internal(args),
    Engine ["engine"] Parsed (args) => parse_engine(args),
    Verifier ["verifier"] Parsed (args) => parse_verifier(args),
    Cleanup ["cleanup"] Parsed (args) => parse_cleanup(args),
    Resources ["resources"] Parsed (args) => parse_resources(args),
    Summary ["summary"] Parsed (args) => {
            expect_no_more(&args[1..], usage::U104)?;
            Ok(Invocation::Summary)
        },
    Report ["report"] Parsed (args) => parse_report(args),
    Search ["search"] Parsed (args) => parse_search(args),
    Import ["import"] Parsed (args) => parse_import(args),
    Decompose ["decompose"] Parsed (args) => parse_decompose(args),
    ImportProject ["import-project"] Parsed (args) => parse_import_project(args),
    Migrate ["migrate"] Parsed (args) => parse_migrate(args),
        // Deleted rather than redirected-and-kept: `link checkout` is strictly
        // more capable. `relink` needed a pointer file in the directory it was
        // pointed at, which is precisely what a checkout that has been moved,
        // renamed or freshly cloned may not have; `link checkout` records the
        // path against a project named the ordinary way and asks the directory
        // for nothing.
    Relink ["relink"] Parsed (args) => Err(AppError::Usage(
            "`story relink` is now `story project link checkout`.\n\nIt no longer reads a \
             pointer file, so it works for a checkout that never had one:\n\n  story --project \
             <SLUG> project link checkout <PATH>"
                .to_string(),
        )),
    Export ["export"] Parsed (args) => {
            expect_no_more(&args[1..], usage::U105)?;
            Ok(Invocation::Export)
        },
    LoadContext ["load-context", "context"] Parsed (args) => parse_context(args),
    Phase ["phase"] Parsed (args) => parse_phase(args),
    Type ["type"] Parsed (args) => parse_type(args),
    Epic ["epic"] Parsed (args) => parse_epic(args),
    Handoff ["handoff"] Parsed (args) => parse_handoff(args),
    Graph ["graph"] Parsed (args) => parse_graph(args),
    Doctor ["doctor"] Parsed (args) => parse_doctor(args),
    LaneBudget ["lane-budget"] Parsed (args) => {
            expect_no_more(&args[1..], usage::U106)?;
            Ok(Invocation::LaneBudget)
        },
    Hooks ["hooks"] Parsed (args) => parse_hooks(args),
    Scaffold ["scaffold"] Parsed (args) => parse_scaffold(args),
    CommitSync ["commit-sync", "sync-git"] Parsed (args) => parse_commit_sync(args),
    LinkPr ["link-pr"] Parsed (args) => parse_link_pr(args),
    UnlinkPr ["unlink-pr"] Parsed (args) => parse_unlink_pr(args),
    Attachment ["attachment"] Parsed (args) => parse_attachment(args),
    PrCheck ["pr-check"] Parsed (args) => parse_pr_check(args),
    Plugin ["plugin"] Parsed (args) => parse_plugin(args),
    Web ["web"] Parsed (args) => parse_web(args),
    Token ["token"] Parsed (args) => parse_token(args),
    Daemon ["daemon"] Parsed (args) => parse_daemon(args),
    Store ["store"] Parsed (args) => parse_store(args),
    Continuation ["continuation"] Parsed (args) => continuation::parse(args),
    SessionEligibility ["session-eligibility"] Parsed (args) => {
            if args.len() != 2 {
                return Err(AppError::Usage(
                    usage::U107.into(),
                ));
            }
            Ok(Invocation::SessionEligibility {
                id: args[1].clone(),
            })
        },
    Show ["show"] Parsed (args) => parse_show(args),
    Log ["log"] Parsed (args) => parse_log(args),
    Comment ["comment"] Parsed (args) => parse_comment(args),
    Move ["move"] Parsed (args) => parse_move(args),
    Close ["close"] Parsed (args) => parse_close(args),
    Block ["block"] Parsed (args) => parse_block(args),
    Unblock ["unblock"] Parsed (args) => parse_unblock(args),
    Prioritize ["prioritize"] Parsed (args) => parse_prioritize(args),
    Label ["label"] Parsed (args) => parse_label(args),
    Unlabel ["unlabel"] Parsed (args) => parse_unlabel(args),
    Reopen ["reopen"] Parsed (args) => parse_reopen_verb(args),
    Archive ["archive"] Parsed (args) => parse_hide(args),
    Unarchive ["unarchive"] Parsed (args) => parse_unhide(args),
    ArchiveState ["archive-state"] Parsed (args) => parse_hide_state(args),
    Publish ["publish"] Parsed (args) => parse_publish(args),
    Delete ["delete"] Parsed (args) => parse_delete_verb(args),
    Purge ["purge"] Parsed (args) => parse_purge_verb(args),
    Set ["set"] Parsed (args) => parse_set(args),
    Relate ["relate", "link"] Parsed (args) => parse_relate(args),
    Unrelate ["unrelate", "unlink"] Parsed (args) => parse_unrelate(args),
    SessionStart ["session-start"] Parsed (args) => {
            expect_no_more(&args[1..], usage::U108)?;
            Ok(Invocation::SessionStart)
        },
    Github ["github"] Github (args) => unknown(&args[0]),
    Tui ["tui"] Tui (args) => unknown(&args[0]),
}

/// One long flag a verb accepts, and whether the token after it is its value.
///
/// `takes_value` exists so the gate stays a *necessary-condition* check: the
/// token after `--description` is that flag's value and is never judged, so
/// `story new t --description --odd` keeps working exactly as it does today.
/// The gate may only refuse what a parser would have swallowed; it may never
/// refuse what one would have accepted.
#[derive(Clone, Copy, Debug)]
pub struct Flag {
    pub name: &'static str,
    pub takes_value: bool,
}

/// A value-taking flag.
const fn value(name: &'static str) -> Flag {
    Flag {
        name,
        takes_value: true,
    }
}

/// A flag that stands alone.
const fn bare(name: &'static str) -> Flag {
    Flag {
        name,
        takes_value: false,
    }
}

/// The long flags one verb path accepts.
///
/// `subcommand` is `Some` only where two subcommands of the same verb accept
/// genuinely different flags (`state add` versus `state set`). Lookup tries the
/// two-token key first and falls back to the verb alone, which is what keeps
/// `story move SH-1 done --if-state x` working: `SH-1` is an argument, not a
/// subcommand, so no two-token entry matches and `move`'s own entry answers.
pub struct FlagPath {
    pub command: CommandId,
    pub subcommand: Option<&'static str>,
    pub flags: &'static [Flag],
}

/// Every long flag this CLI accepts, by verb path.
///
/// **This table fails closed.** A verb with no entry declares nothing, so every
/// flag-shaped token reaching it is refused. That is deliberate: forgetting to
/// declare a new verb's flags produces a loud, immediate error, where forgetting
/// a per-verb guard would silently re-inherit SH-62. Rot in the loud direction
/// is recoverable; rot in the quiet direction is this defect.
///
/// Two verbs cannot be checked against their own help text, because their help
/// names no flags at all — see `UNDISCOVERABLE` in `tests/unknown_flag_sweep.rs`.
pub static FLAG_PATHS: &[FlagPath] = &[
    FlagPath {
        command: CommandId::Continuation,
        subcommand: None,
        flags: &[
            bare("stdin"),
            value("reviewed-seq"),
            value("head"),
            value("provider"),
            value("session-id"),
        ],
    },
    FlagPath {
        command: CommandId::DispatchPolicy,
        subcommand: Some("set"),
        flags: &[
            bare("global"),
            value("agent"),
            value("complexity"),
            value("model"),
            value("effort"),
        ],
    },
    FlagPath {
        command: CommandId::DispatchPolicy,
        subcommand: Some("reset"),
        flags: &[
            bare("global"),
            value("agent"),
            value("complexity"),
            bare("model"),
            bare("effort"),
        ],
    },
    FlagPath {
        command: CommandId::DispatchPolicy,
        subcommand: None,
        flags: &[bare("global"), value("agent")],
    },
    FlagPath {
        command: CommandId::New,
        subcommand: None,
        flags: &[
            value("state"),
            value("type"),
            value("description"),
            value("priority"),
            value("complexity"),
            value("label"),
            value("labels"),
            value("blocked-by"),
            bare("draft"),
        ],
    },
    FlagPath {
        command: CommandId::List,
        subcommand: None,
        flags: &[
            value("state"),
            value("priority"),
            value("label"),
            value("created-after"),
            value("updated-after"),
            value("stale"),
            value("phase"),
            value("type"),
            bare("flagged"),
            bare("blocked"),
            bare("ready"),
            bare("drafts"),
            bare("unassessed"),
            bare("include-closed"),
            bare("include-archived"),
            bare("all"),
        ],
    },
    FlagPath {
        command: CommandId::Next,
        subcommand: None,
        flags: &[
            value("count"),
            value("phase"),
            value("epic"),
            value("exclude-label"),
        ],
    },
    FlagPath {
        command: CommandId::Claim,
        subcommand: None,
        flags: &[
            value("phase"),
            value("epic"),
            value("exclude-label"),
            value("comment"),
            bare("next"),
            bare("no-comment"),
            bare("dry-run"),
        ],
    },
    FlagPath {
        command: CommandId::Reset,
        subcommand: None,
        flags: &[bare("force"), bare("dry-run")],
    },
    FlagPath {
        command: CommandId::Unclaim,
        subcommand: None,
        flags: &[value("comment"), bare("no-comment"), bare("dry-run")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("adopt"),
        flags: &[value("run")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("configure"),
        flags: &[
            value("run"),
            value("lanes"),
            value("model"),
            value("effort"),
            value("speed"),
        ],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("start"),
        flags: &[
            value("epic"),
            value("lanes"),
            value("agent"),
            value("model"),
            value("effort"),
            value("speed"),
        ],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("reset-target"),
        flags: &[value("run"), value("token")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("reset-check"),
        flags: &[],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("status"),
        flags: &[value("run")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("pause"),
        flags: &[value("run")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("resume"),
        flags: &[value("run")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("stop"),
        flags: &[value("run"), bare("now")],
    },
    FlagPath {
        command: CommandId::Engine,
        subcommand: Some("ack"),
        flags: &[value("run")],
    },
    FlagPath {
        command: CommandId::Verifier,
        subcommand: Some("ack"),
        flags: &[bare("leave-stopped")],
    },
    FlagPath {
        command: CommandId::Verifier,
        subcommand: Some("repair"),
        flags: &[value("input")],
    },
    FlagPath {
        command: CommandId::Verifier,
        subcommand: Some("landing"),
        flags: &[value("reason")],
    },
    FlagPath {
        command: CommandId::Resources,
        subcommand: None,
        flags: &[
            bare("location-only"),
            value("lease-json"),
            value("window-name"),
            value("worktree-root"),
            value("tmux-socket"),
        ],
    },
    FlagPath {
        command: CommandId::Cleanup,
        subcommand: None,
        flags: &[bare("dry-run")],
    },
    FlagPath {
        command: CommandId::Set,
        subcommand: None,
        flags: &[
            value("title"),
            value("state"),
            value("priority"),
            value("complexity"),
            value("labels"),
            value("blocked"),
            value("json"),
            value("type"),
            value("description"),
            bare("unblocked"),
        ],
    },
    FlagPath {
        command: CommandId::Move,
        subcommand: None,
        flags: &[value("if-state"), value("reason")],
    },
    FlagPath {
        command: CommandId::Block,
        subcommand: None,
        flags: &[value("on")],
    },
    FlagPath {
        command: CommandId::Unblock,
        subcommand: None,
        flags: &[value("on")],
    },
    FlagPath {
        command: CommandId::Reopen,
        subcommand: None,
        flags: &[],
    },
    FlagPath {
        command: CommandId::Delete,
        subcommand: None,
        flags: &[bare("force")],
    },
    // `purge` is a retired redirect rather than an unknown command. Keep its
    // former flag declared so SH-62's pre-parser gate lets every old spelling
    // reach the refusal that names `story delete`.
    FlagPath {
        command: CommandId::Purge,
        subcommand: None,
        flags: &[bare("force")],
    },
    FlagPath {
        command: CommandId::ArchiveState,
        subcommand: None,
        flags: &[bare("force")],
    },
    FlagPath {
        command: CommandId::Report,
        subcommand: None,
        flags: &[bare("html")],
    },
    FlagPath {
        command: CommandId::Doctor,
        subcommand: None,
        flags: &[bare("fix")],
    },
    FlagPath {
        command: CommandId::Doctor,
        subcommand: Some("abandoned"),
        flags: &[bare("all")],
    },
    FlagPath {
        command: CommandId::Doctor,
        subcommand: Some("crashes"),
        flags: &[bare("all")],
    },
    FlagPath {
        command: CommandId::Update,
        subcommand: None,
        flags: &[bare("check"), bare("force"), value("source")],
    },
    FlagPath {
        command: CommandId::Handoff,
        subcommand: None,
        flags: &[value("since")],
    },
    FlagPath {
        command: CommandId::CommitSync,
        subcommand: None,
        flags: &[value("since")],
    },
    FlagPath {
        command: CommandId::LinkPr,
        subcommand: None,
        flags: &[bare("no-close-on-merge")],
    },
    // One shared entry for all four subcommands, matching this table's
    // existing looseness for `daemon`: only `add` accepts `--name`, and
    // `parse_attachment` itself is what refuses it on `list`/`remove`/`save`.
    FlagPath {
        command: CommandId::Attachment,
        subcommand: None,
        flags: &[value("name")],
    },
    FlagPath {
        command: CommandId::Decompose,
        subcommand: None,
        flags: &[bare("stdin"), bare("dry-run")],
    },
    FlagPath {
        command: CommandId::Migrate,
        subcommand: None,
        flags: &[bare("dry-run")],
    },
    FlagPath {
        command: CommandId::ImportProject,
        subcommand: None,
        flags: &[bare("legacy-links")],
    },
    FlagPath {
        command: CommandId::LoadContext,
        subcommand: None,
        flags: &[value("format"), value("story")],
    },
    FlagPath {
        command: CommandId::Graph,
        subcommand: None,
        flags: &[
            bare("critical-path"),
            bare("parallel-groups"),
            value("blocked-by"),
        ],
    },
    FlagPath {
        command: CommandId::Help,
        subcommand: None,
        flags: &[bare("compact"), bare("all")],
    },
    // `--serve` is a subcommand spelled as a flag: it is what the spawner
    // execs, never what a user types. Declared, or the daemon cannot start.
    // `--force` belongs only to `stop`, and `port` only to `start`/`--serve`
    // — one shared entry rather than per-subcommand ones, matching this
    // table's existing looseness for `daemon`: `parse_daemon` itself is what
    // actually refuses a flag on the wrong subcommand.
    // Scoped to `install` on purpose. `declared_flags` prefers a
    // `(verb, Some(subcommand))` entry over the verb's own, so declaring
    // `--this-binary` here rather than on the `daemon` row is what makes every
    // sibling subcommand refuse it *by construction* rather than by a list
    // somebody has to remember to keep (SH-136's class). It is also the one
    // residual `tests/trailing_arguments.rs` names as its own blind spot — that
    // scan drops every `-`-prefixed word — so `tests/daemon_install_flag.rs`
    // proves the scoping instead.
    FlagPath {
        command: CommandId::Daemon,
        subcommand: Some("install"),
        flags: &[bare("this-binary")],
    },
    FlagPath {
        command: CommandId::Daemon,
        subcommand: Some("gc"),
        flags: &[bare("force")],
    },
    FlagPath {
        command: CommandId::Daemon,
        subcommand: Some("logs"),
        flags: &[bare("follow"), value("directory")],
    },
    FlagPath {
        command: CommandId::Daemon,
        subcommand: None,
        flags: &[bare("serve"), value("port"), bare("force"), value("owner")],
    },
    FlagPath {
        command: CommandId::Web,
        subcommand: None,
        flags: &[bare("serve"), value("port")],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("new"),
        flags: &[
            value("prefix"),
            value("name"),
            value("attach"),
            bare("no-attach"),
            bare("no-agents-md"),
        ],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("delete"),
        flags: &[bare("force")],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("set-prefix"),
        flags: &[bare("force")],
    },
    // The two retired verbs keep their entries, and this is the reason rather
    // than an oversight. Both are redirects now, and SH-62's gate runs *ahead*
    // of every parser — so without an entry declaring what each used to take,
    // `story project init --prefix AB` would be answered "unknown flag
    // `--prefix`" and the redirect naming `story project new` would never fire.
    // A redirect that only works for the flagless spelling is half a redirect.
    // Both entries go when the redirects do, at 3.0.0.
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("init"),
        flags: &[value("prefix"), value("name"), bare("no-agents-md")],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("deinit"),
        flags: &[bare("force")],
    },
    // Declared with no flags rather than left out. Under SH-62's fail-closed
    // rule both spellings refuse every flag-shaped token, so the behaviour is
    // identical — but an entry says "this verb takes no flags" where an absence
    // says nothing, and the next person to add one will look here first.
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("link"),
        flags: &[],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("unlink"),
        flags: &[],
    },
    FlagPath {
        command: CommandId::Project,
        subcommand: Some("show"),
        flags: &[],
    },
    FlagPath {
        command: CommandId::Type,
        subcommand: Some("add"),
        flags: &[value("description"), value("emoji")],
    },
    FlagPath {
        command: CommandId::Type,
        subcommand: Some("set"),
        flags: &[
            value("description"),
            bare("no-description"),
            value("emoji"),
            bare("no-emoji"),
        ],
    },
    FlagPath {
        command: CommandId::State,
        subcommand: Some("add"),
        flags: &[value("super"), value("role"), value("description")],
    },
    FlagPath {
        command: CommandId::State,
        subcommand: Some("set"),
        flags: &[
            value("super"),
            value("role"),
            value("description"),
            bare("no-description"),
            value("move-stories-to"),
        ],
    },
    FlagPath {
        command: CommandId::State,
        subcommand: Some("remove"),
        flags: &[value("move-stories-to")],
    },
    FlagPath {
        command: CommandId::Store,
        subcommand: Some("backup"),
        flags: &[value("label")],
    },
];

/// One legacy help syntax block, attached to the actual registered command.
#[derive(Clone, Copy, Debug)]
pub struct HelpSyntax {
    pub command: CommandId,
    pub text: &'static str,
}
macro_rules! help_syntax {
    ($prefix:literal; $( $command:ident => $text:literal, )* ; $suffix:literal) => {
        /// Legacy general help, assembled from the registered syntax blocks.
        pub const HELP_TEXT: &str = concat!($prefix, $($text,)* $suffix);
        /// The same blocks consumed by command discovery, without copying help.
        pub static HELP_SYNTAX: &[HelpSyntax] = &[
            $(HelpSyntax { command: CommandId::$command, text: $text },)*
        ];
    }
}
help_syntax! {
"story - CLI-first issue tracker for AI agents\n\nUsage:\n";
    Project => r#"  story project new --prefix <PREFIX> [--name <NAME>] [--attach <PATH> | --no-attach]
                    [--no-agents-md]                (no flags at a terminal: it asks)
"#,
    Project => r#"  story project link origin [URL] | link checkout [PATH]
"#,
    Project => r#"  story project unlink origin [URL] | unlink checkout
"#,
    Project => r#"  story project delete [--force]                   (delete a project and its stories)
"#,
    Project => r#"  story project list                               (every project storyhook knows)
"#,
    Project => r#"  story project settings list|get|set|unset        (this project's settings)
"#,
    DispatchPolicy => r#"  story dispatch-policy show|set|reset|resolve      (complexity-based model and effort)
"#,
    New => r#"  story new <title> [--state <slug>] [--type <slug>] [--description <text>]
                    [--priority <level>] [--complexity low|medium|high] [--label <name> ...]
                    [--blocked-by <id> ...]          (filed already blocked)
                    [--draft]                        (claims an id; not yet live)
"#,
    Tui => r#"  story tui                                           (interactive terminal UI)
"#,
    Web => r#"  story web start [--port <PORT>]                  (start web dashboard)
"#,
    Web => r#"  story web stop                                   (stop web dashboard)
"#,
    Web => r#"  story web open                                   (open the dashboard in your browser)
"#,
    Web => r#"  story web address                                (copy the dashboard URL to the clipboard)
"#,
    Daemon => r#"  story daemon start [--port <PORT>]
"#,
    Daemon => r#"  story daemon restart                             (drain and replace the running daemon)
"#,
    Daemon => r#"  story daemon stop [--force]
"#,
    Daemon => r#"  story daemon status
"#,
    Daemon => r#"  story daemon install [--this-binary]
"#,
    Daemon => r#"  story daemon uninstall
"#,
    Daemon => r#"  story daemon token
"#,
    Daemon => r#"  story daemon gc [--force]                        (reclaim runtime dirs of stores that are gone)
"#,
    Token => r#"  story token new <name>                           (mint a named dashboard token)
"#,
    Token => r#"  story token list                                 (show every live token)
"#,
    Token => r#"  story token revoke <name>                        (end one token immediately)
"#,
    State => r#"  story state list
"#,
    State => r#"  story state add <state-slug> --super OPEN|CLOSED [--role active]
                               [--description "<text>"]
"#,
    State => r#"  story state set <state-slug> [--super OPEN|CLOSED] [--role active|none]
                               [--description "<text>"] [--no-description]
                               [--move-stories-to <state-slug>]
"#,
    State => r#"  story state remove <state-slug> [--move-stories-to <state-slug>]
"#,
    State => r#"  story state reorder <state-slug,state-slug,...>   (board column order)
"#,
    List => r#"  story list [--state <slug>] [--flagged] [--priority <levels>]
             [--label <labels>] [--created-after <date>] [--updated-after <date>]
             [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>]
             [--drafts]                                (narrows to drafts only)
             [--unassessed]                            (narrows to stories nobody has assessed)
             [--include-closed]                        (also show closed, unarchived stories)
             [--include-archived]                      (also show archived stories; implies --include-closed)
             [--all]                                   (--include-closed --include-archived)
"#,
    Next => r#"  story next [--count <n>] [--phase <N>] [--epic <id>] [--exclude-label <csv>]
"#,
    Claim => r#"  story claim <id> [--comment <text> | --no-comment] [--dry-run]
"#,
    Claim => r#"  story claim --next [--phase <N>] [--epic <id>] [--exclude-label <csv>]
                     [--comment <text> | --no-comment] [--dry-run]
                                                    (take a story, atomically)
"#,
    Reset => r#"  story reset <id> [--force]  Remove owned workspace and return to Todo
"#,
    Unclaim => r#"  story unclaim <id> [--comment <text> | --no-comment]
                     [--dry-run]                    (hand it back where it came from)
"#,
    Engine => r#"  story engine start [--epic <id>] [--lanes <n>] [--agent claude|codex]
                     [--model <id>] [--effort <id>] [--speed standard|fast]
"#,
    Engine => r#"  story engine configure (--lanes <n> | --model <id> | --effort <id> | --speed standard|fast) [--run <id>]
"#,
    Engine => r#"  story engine adopt <id> [<id> ...] [--run <id>]
"#,
    Engine => r#"  story engine status [--run <id>]
"#,
    Engine => r#"  story engine pause|resume|ack [--run <id>]
"#,
    Engine => r#"  story engine stop [--run <id>] [--now]
"#,
    Verifier => r#"  story verifier status | start | stop | drain
"#,
    Verifier => r#"  story verifier evidence <story-id> [--json]
"#,
    Verifier => r#"  story verifier ack <incident-id> [--leave-stopped] (acknowledge and retry by default)
"#,
    Verifier => r#"  story verifier repair show <recovery-id> --json
"#,
    Verifier => r#"  story verifier repair decide <recovery-id> --input <json-file>
"#,
    Verifier => r#"  story verifier repair satisfy <recovery-id> --input <json-file>
"#,
    Verifier => r#"  story verifier gate-config <checkout> <base> <head> <tree> --json
"#,
    Resources => r#"  story resources <id> [--json]                    (inspect existing resource identity)
"#,
    Cleanup => r#"  story cleanup [--dry-run]                         (clean closed-story resources and retry incomplete cleanup)
"#,
    DispatchPolicy => r#"  story dispatch-policy show|set|reset|resolve      (automatic model and effort settings)
"#,
    Summary => r#"  story summary
"#,
    Report => r#"  story report [--html]
"#,
    Search => r#"  story search <query>
"#,
    Import => r#"  story import [<file>]
"#,
    Export => r#"  story export
"#,
    Decompose => r#"  story decompose <file> [--dry-run]     (markdown or YAML)
"#,
    Decompose => r#"  story decompose --stdin [--dry-run]
"#,
    ImportProject => r#"  story import-project <file>
"#,
    Migrate => r#"  story migrate [<path>] [--dry-run]               (move a .storyhook tree into the store)
"#,
    Store => r#"  story store new <path>                           (create an empty store beside the default one)
"#,
    Store => r#"  story store backup [--label <text>]              (safe, on-demand backup of the ambient store)
"#,
    LoadContext => r#"  story load-context [--format markdown|json] [--story <id>]
"#,
    SessionEligibility => r#"  story session-eligibility <id>                 (structured active-session check)
"#,
    Handoff => r#"  story handoff [--since <duration>]
"#,
    Phase => r#"  story phase list
"#,
    Phase => r#"  story phase show <N>
"#,
    Phase => r#"  story phase add <id> <N>
"#,
    Phase => r#"  story phase remove <id>
"#,
    Phase => r#"  story phase create <N> ["<title>"]
"#,
    Graph => r#"  story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]
"#,
    Doctor => r#"  story doctor [--fix]
"#,
    Doctor => r#"  story doctor install                             (what is installed here, and what is pending)
"#,
    Doctor => r#"  story doctor abandoned [clear (--all | <request-id>)]
"#,
    Doctor => r#"  story doctor crashes [clear (--all | <crash-id>)]
"#,
    Update => r#"  story update [--check] [--force] [--source HOST/OWNER/REPO]                 (self-update the story binary)
"#,
    Hooks => r#"  story hooks install|uninstall|list|test <event_type>
"#,
    CommitSync => r#"  story commit-sync [--since <duration>]
"#,
    LinkPr => r#"  story link-pr <id> <url> [--no-close-on-merge]    (link a GitHub pull request to a story)
"#,
    UnlinkPr => r#"  story unlink-pr <id> <url>
"#,
    Attachment => r#"  story attachment add <id> <path> [--name <text>]  (attach an image to a story)
"#,
    Attachment => r#"  story attachment list <id>
"#,
    Attachment => r#"  story attachment remove <id> <n>
"#,
    Attachment => r#"  story attachment save <id> <n> <path>
"#,
    PrCheck => r#"  story pr-check [<id>]                             (requires the github-pr feature)
"#,
    Scaffold => r#"  story scaffold agents-md|claude-md|cursor-rules
"#,
    Help => r#"  story help [<command>] [--compact] [--all]
"#,
    Plugin => r#"  story plugin install|uninstall <claude|codex>
"#,
    Plugin => r#"  story plugin reinstall                            (every provider that has it registered, from this binary)
"#,
    Plugin => r#"  story plugin run codex -- <helper-command> [args...]  (internal stable Codex launcher)
"#,
    Show => r#"  story show <id>
"#,
    Log => r#"  story log <id>
"#,
    Comment => r#"  story comment <id> "<text>"
"#,
    Move => r#"  story move <id> <state-slug> [--if-state <expected>] ["<comment>"]
"#,
    Block => r#"  story block <id> --on <blocker> [--on <blocker>]... ["<reason>"]
"#,
    Block => r#"  story block <id> "<reason>"
"#,
    Unblock => r#"  story unblock <id> [--on <blocker>]...
"#,
    Prioritize => r#"  story prioritize <id> <critical|high|medium|low>
"#,
    Label => r#"  story label <id> <labels-csv>
"#,
    Unlabel => r#"  story unlabel <id> <labels-csv>
"#,
    Close => r#"  story close <id> "<reason>"                       (retire a story that will not be done)
"#,
    Reopen => r#"  story reopen <id>
"#,
    Archive => r#"  story archive <id>                               (hide a closed story from the primary UI)
"#,
    Unarchive => r#"  story unarchive <id>
"#,
    ArchiveState => r#"  story archive-state <state-slug> [--force]        (archive every story in a closed column)
"#,
    Publish => r#"  story publish <id>                               (make a draft live; one-way)
"#,
    Delete => r#"  story delete <id> [--force]                      (permanently remove a story)
"#,
    Set => r#"  story set <id> [--title "<title>"] [--state <slug>] [--priority <level>]
                 [--complexity low|medium|high]
                  [--labels "<csv>"] [--blocked "<reason>"]
                  [--unblocked] [--json "<json>"] [--type <slug>]
                  [--description "<text>"]
"#,
    Relate => r#"  story relate <a> <relationship-type> <b>
"#,
    Unrelate => r#"  story unrelate <a> <relationship-type> <b>
"#,
    Relate => r#"  story link <a> <relationship-type> <b>
"#,
    Unrelate => r#"  story unlink <a> <relationship-type> <b>
"#,
    Type => r#"  story type list
"#,
    Type => r#"  story type add <slug> [--description "<text>"] [--emoji <glyph>]
"#,
    Type => r#"  story type set <slug> [--description "<text>"] [--no-description]
                        [--emoji <glyph>] [--no-emoji]
"#,
    Type => r#"  story type remove <slug>
"#,
    Epic => r#"  story epic list
"#,
    Epic => r#"  story epic show <id>
"#,
    Epic => r#"  story epic create "<title>"
"#,
    Epic => r#"  story epic add <epic-id> <story-id>
"#,
; "\nStory ids:\n  Everywhere <id> appears above, both forms name the same story: the canonical\n  `SH-5`, and the bare number `5` on its own. The number is read against the\n  project the command is acting on, so `5` means nothing until that is settled —\n  with no project, you get the same refusal every other command gives, not a\n  missing story. An id carrying a *different* project's prefix is refused\n  outright rather than resolved: `--project` decides which project you are in,\n  and an id never overrides it.\n\nAgent workflow and output contracts: story help agent-guide; story help json-format\n\nGlobal options:\n  --json          Emit structured JSON\n  --quiet         Suppress success output\n  --no-hooks      Suppress event hook execution\n  --store-path <file>\n                  Run against a named store rather than the default one. Also\n                  spelled $STORYHOOK_STORE_PATH. One daemon serves one store, so\n                  a command under this flag can neither read nor write any other.\n  --project <slug>\n                  Act on this project, whatever directory you are in. Also\n                  spelled $STORYHOOK_PROJECT, which the flag beats. With neither,\n                  storyhook uses the project this checkout belongs to, and\n                  refuses rather than guessing when there is none.\n                  `story project list` shows the slugs.\n  --deadline <seconds>\n                  Give up waiting on the daemon after this long, rather than\n                  however long starting one and running the command could\n                  otherwise take. The request is not cancelled: the daemon\n                  finishes what it accepted, this process just stops waiting\n                  for the answer. For a caller — a session hook, a script —\n                  that cannot wait regardless of whether storyhook could.\n  -h, --help\n  -V, --version   Print the installed story version\n"
}

/// A nested command selection used by the parser itself. Each group's enum is
/// generated here and matched by its handler, so discovery cannot invent a leaf.
#[derive(Clone, Copy, Debug)]
pub struct CommandGroup {
    pub command: CommandId,
    pub prefix: &'static [&'static str],
    pub words: &'static [&'static str],
}
macro_rules! subcommands {
    ($( $group:ident ($root:ident [$($prefix:literal),*]) { $($variant:ident = $word:literal),+ $(,)? } )*) => {
        $(
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub enum $group { $($variant),+ }
            impl $group {
                pub fn find(word: &str) -> Option<Self> {
                    match word { $($word => Some(Self::$variant),)+ _ => None }
                }
            }
        )*
        pub static GROUPS: &[CommandGroup] = &[
            $(CommandGroup { command: CommandId::$root, prefix: &[$($prefix),*], words: &[$($word),+] },)*
        ];
    }
}
subcommands! {
    ProjectVerb (Project []) {
        New = "new",
        Delete = "delete",
        SetPrefix = "set-prefix",
        Init = "init",
        Deinit = "deinit",
        List = "list",
        Show = "show",
        Link = "link",
        Unlink = "unlink",
        Settings = "settings",
    }
    ProjectLinkVerb (Project ["link"]) {
        Origin = "origin",
        Checkout = "checkout",
    }
    ProjectUnlinkVerb (Project ["unlink"]) {
        Origin = "origin",
        Checkout = "checkout",
    }
    ProjectSettingsVerb (Project ["settings"]) {
        List = "list",
        Get = "get",
        Set = "set",
        Unset = "unset",
    }
    StateVerb (State []) {
        List = "list",
        Add = "add",
        Set = "set",
        Remove = "remove",
        Reorder = "reorder",
    }
    EngineVerb (Engine []) {
        ResetCheck = "reset-check",
        ResetTarget = "reset-target",
        Start = "start",
        Configure = "configure",
        Adopt = "adopt",
        Status = "status",
        Pause = "pause",
        Resume = "resume",
        Stop = "stop",
        Ack = "ack",
    }
    VerifierVerb (Verifier []) {
        Landing = "landing",
        Evidence = "evidence",
        RepairAdmit = "repair-admit",
        Repair = "repair",
        GateConfig = "gate-config",
        Status = "status",
        Start = "start",
        Stop = "stop",
        Drain = "drain",
        Ack = "ack",
    }
    PhaseVerb (Phase []) {
        List = "list",
        Show = "show",
        Add = "add",
        Remove = "remove",
        Create = "create",
    }
    TypeVerb (Type []) {
        List = "list",
        Add = "add",
        Set = "set",
        Remove = "remove",
    }
    EpicVerb (Epic []) {
        List = "list",
        Show = "show",
        Create = "create",
        Add = "add",
    }
    HooksVerb (Hooks []) {
        Install = "install",
        Uninstall = "uninstall",
        List = "list",
        Test = "test",
    }
    AttachmentVerb (Attachment []) {
        Add = "add",
        List = "list",
        Remove = "remove",
        Save = "save",
    }
    PluginVerb (Plugin []) {
        Install = "install",
        Uninstall = "uninstall",
        Reinstall = "reinstall",
        Run = "run",
    }
    StoreVerb (Store []) {
        New = "new",
        Backup = "backup",
    }
    DaemonVerb (Daemon []) {
        Logs = "logs",
        Start = "start",
        Restart = "restart",
        Serve = "--serve",
        Stop = "stop",
        Gc = "gc",
        Status = "status",
        Install = "install",
        Uninstall = "uninstall",
        Token = "token",
    }
    WebVerb (Web []) {
        Start = "start",
        Stop = "stop",
        Status = "status",
        Open = "open",
        Address = "address",
        Revoke = "revoke",
        Serve = "--serve",
    }
    TokenVerb (Token []) {
        New = "new",
        List = "list",
        Revoke = "revoke",
    }
    VerifierLandingVerb (Verifier ["landing"]) { Show = "show", Release = "release" }

    VerifierRepairVerb (Verifier ["repair"]) { Show = "show", Decide = "decide", Satisfy = "satisfy" }

    InternalVerb (Internal []) { SupersedeBlockDeliveries = "supersede-block-deliveries", SupersedeContinuations = "supersede-continuations" }

    DoctorVerb (Doctor []) { Abandoned = "abandoned", Crashes = "crashes", Install = "install" }

    DoctorAbandonedVerb (Doctor ["abandoned"]) { Clear = "clear" }

    DoctorCrashesVerb (Doctor ["crashes"]) { Clear = "clear" }

    ScaffoldVerb (Scaffold []) { AgentsMd = "agents-md", ClaudeMd = "claude-md", CursorRules = "cursor-rules" }

    ContinuationVerb (Continuation []) { Capabilities = "capabilities", Request = "request", Status = "status", Receipt = "receipt", Retry = "retry", Ack = "ack" }

    DispatchPolicyVerb (DispatchPolicy []) { Show = "show", Set = "set", Reset = "reset", Resolve = "resolve" }

    GithubVerb (Github []) { Observe = "observe", Resolve = "resolve", Merge = "merge", Exec = "exec", Git = "git" }

}

/// Syntax quoted by the real argument validators. Kept separately from prose so
/// offline consumers can inspect the same usage that a failed parse returns.
#[derive(Clone, Copy, Debug)]
pub struct UsageSyntax {
    pub command: CommandId,
    pub text: &'static str,
}
macro_rules! usages {
    ($( $name:ident ($command:ident) = $text:literal; )*) => {
        pub mod usage { $(pub const $name: &str = $text;)* }
        pub static USAGE_SYNTAX: &[UsageSyntax] = &[
            $(UsageSyntax { command: CommandId::$command, text: usage::$name },)*
        ];
    }
}
usages! {
    U1 (Internal) = "usage: story internal supersede-block-deliveries <id> --json\n       \
             story internal supersede-continuations <id> --json";
    U2 (Resources) = "usage: story resources <id> [--lease-json JSON] [--window-name NAME] [--worktree-root PATH] [--tmux-socket PATH] [--location-only]";
    U3 (Cleanup) = "usage: story cleanup [--dry-run]";
    U4 (Claim) = "usage: story claim <id> [--comment <text> | --no-comment] \
                           [--dry-run]\n       story claim --next [--phase <N>] [--epic <id>] \
                           [--exclude-label <csv>] [--comment <text> | --no-comment] \
                           [--dry-run]";
    U5 (Unclaim) = "usage: story unclaim <id> [--comment <text> | --no-comment] \
                             [--dry-run]";
    U6 (Project) = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                             [--attach <PATH> | --no-attach] [--no-agents-md] | delete \
                             [--force] | set-prefix <NEW-PREFIX> [--force] | show | list | \
                             link origin [URL]|checkout [PATH] | unlink origin [URL]|checkout \
                             | settings list|get|set|unset";
    U7 (Project) = "usage: story project show\n\n`story project show` takes no \
                                  argument. It reports the project this directory resolves \
                                  to — name a different one with `--project <slug>`.";
    U8 (Project) = "usage: story project delete [--force]\n\n`story project \
                                    delete` takes no positional argument. It destroys the \
                                    project this directory resolves to; name a different one \
                                    with `--project <slug>`.";
    U9 (Project) = "usage: story project set-prefix <NEW-PREFIX> \
                                        [--force]\n\n`story project set-prefix` takes exactly \
                                        one positional argument, the new prefix. It rewrites \
                                        the project this directory resolves to; name a \
                                        different one with `--project <slug>`.";
    U10 (Project) = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                                 [--attach <PATH> | --no-attach] [--no-agents-md]\n\nRun with no \
                                 flags at a terminal to be asked. `story project new` takes no \
                                 positional argument: name the project with --name and the \
                                 checkout with --attach.";
    U11 (Project) = "usage: story project link origin [URL] | story project link checkout [PATH]\n\nThese attach \
     *git* associations to a project. They are unrelated to `story link`, which is an alias for \
     `story relate` and joins one story to another.";
    U12 (Project) = "usage: story project unlink origin [URL] | story project unlink checkout\n\n`unlink \
     checkout` takes no path: a project has at most one. These are unrelated to `story unlink`, \
     which is an alias for `story unrelate`.";
    U13 (Project) = "usage: story project settings list | get <key> | \
                                      set <key> <value> | unset <key>";
    U14 (New) = "usage: story new <title> [--state <slug>] [--type <slug>] [--description <text>] [--priority <level>] [--complexity low|medium|high] [--label <name> ...] [--labels <csv>] [--blocked-by <id> ...] [--draft]";
    U15 (Publish) = "usage: story publish <id>";
    U16 (Type) = "usage: story type list | story type add <slug> [...] | story type set <slug> [...] | story type remove <slug>";
    U17 (Type) = "usage: story type add <slug> [--description \"<text>\"] [--emoji <glyph>]";
    U18 (Type) = "usage: story type set <slug> [--description \"<text>\"] [--no-description] [--emoji <glyph>] [--no-emoji]";
    U19 (Type) = "usage: story type remove <slug>";
    U20 (State) = "usage: story state list | story state add <slug> --super OPEN|CLOSED | story state set <slug> [...] | story state remove <slug> | story state reorder <slug,...>";
    U21 (State) = "usage: story state add <slug> --super OPEN|CLOSED [--role active] [--description \"<text>\"]";
    U22 (State) = "usage: story state set <slug> [--super OPEN|CLOSED] [--role active|none] [--description \"<text>\"] [--no-description] [--move-stories-to <slug>]";
    U23 (State) = "usage: story state remove <slug> [--move-stories-to <slug>]";
    U24 (State) = "usage: story state reorder <slug,slug,...>";
    U25 (List) = "usage: story list [--state <slug>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>] [--drafts] [--unassessed] [--include-closed] [--include-archived] [--all]";
    U26 (Next) = "usage: story next [--count <n>] [--phase <N>] [--epic <id>] \
                 [--exclude-label <csv>]";
    U27 (Engine) = "usage: story engine start [--epic <id>] [--lanes <n>] [--agent claude|codex] [--model <id>] [--effort <id>] [--speed standard|fast]";
    U28 (Engine) = "usage: story engine status [--run <id>]";
    U29 (Engine) = "usage: story engine pause [--run <id>]";
    U30 (Engine) = "usage: story engine resume [--run <id>]";
    U31 (Engine) = "usage: story engine stop [--run <id>] [--now]";
    U32 (Engine) = "usage: story engine ack [--run <id>]";
    U33 (Engine) = "usage: story engine <start|configure|adopt|status|pause|resume|stop|ack>";
    U34 (Engine) = "usage: story engine reset-check <story-id>";
    U35 (Engine) = "usage: story engine reset-target --run <id> --token <token>";
    U36 (Engine) = "usage: story engine adopt <id> [<id> ...] [--run <id>]";
    U37 (Engine) = "usage: story engine configure (--lanes <n> | --model <id> | --effort <id> | --speed standard|fast) [--run <id>]";
    U38 (Verifier) = "usage: story verifier ack <incident-id> [--leave-stopped]";
    U39 (Verifier) = "usage: story verifier <status|evidence|landing|start|stop|drain|ack|repair>";
    U40 (Verifier) = "usage: story verifier landing show | release <intent-id> --reason <reason>";
    U41 (Verifier) = "usage: story verifier evidence <story-id> [--json]";
    U42 (Verifier) = "usage: story verifier repair-admit <story> <attempt> <generation> <base> <head> <head-tree> <tree> --json (private verifier callback)";
    U43 (Verifier) = "usage: story verifier repair show <recovery-id> | decide <recovery-id> --input <json-file> | satisfy <recovery-id> --input <json-file>";
    U44 (Verifier) = "usage: story verifier gate-config <checkout> <base> <head> <tree> --json";
    U45 (Verifier) = "usage: story verifier <status|start|stop|drain>";
    U46 (Report) = "usage: story report [--html]";
    U47 (Search) = "usage: story search <query>";
    U48 (Import) = "usage: story import [<file>]";
    U49 (Decompose) = "usage: story decompose <file> [--dry-run] | story decompose --stdin [--dry-run]";
    U50 (ImportProject) = "usage: story import-project <file> [--legacy-links]";
    U51 (Migrate) = "usage: story migrate [<path>] [--dry-run]";
    U52 (LoadContext) = "usage: story load-context [--format markdown|json] [--story <id>]";
    U53 (Phase) = "usage: story phase list|show <N>|add <id> <N>|remove <id>|create <N> [\"<title>\"]";
    U54 (Phase) = "usage: story phase show <N>";
    U55 (Phase) = "usage: story phase add <id> <N>";
    U56 (Phase) = "usage: story phase remove <id>";
    U57 (Phase) = "usage: story phase create <N> [\"<title>\"]";
    U58 (Epic) = "usage: story epic list|show <id>|create \"<title>\"|add <epic-id> <story-id>";
    U59 (Epic) = "usage: story epic show <id>";
    U60 (Epic) = "usage: story epic create \"<title>\"";
    U61 (Epic) = "usage: story epic add <epic-id> <story-id>";
    U62 (Handoff) = "usage: story handoff [--since <duration>]";
    U63 (Graph) = "usage: story graph --blocked-by <id>";
    U64 (Graph) = "usage: story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]";
    U65 (Doctor) = "usage: story doctor install";
    U66 (Doctor) = "usage: story doctor [--fix] | install | abandoned [clear (--all | <request-id>)] \
         | crashes [clear (--all | <crash-id>)]";
    U67 (Doctor) = "usage: story doctor abandoned [clear (--all | <request-id>)]";
    U68 (Doctor) = "usage: story doctor crashes [clear (--all | <crash-id>)]";
    U69 (Update) = "usage: story update [--check] [--force] [--source HOST/OWNER/REPO]";
    U70 (Hooks) = "usage: story hooks install|uninstall|list|test <event_type>";
    U71 (Hooks) = "usage: story hooks test <event_type>";
    U72 (Scaffold) = "usage: story scaffold agents-md|claude-md|cursor-rules";
    U73 (CommitSync) = "usage: story commit-sync [--since <duration>]";
    U74 (LinkPr) = "usage: story link-pr <id> <url> [--no-close-on-merge]";
    U75 (UnlinkPr) = "usage: story unlink-pr <id> <url>";
    U76 (Attachment) = "usage: story attachment add <id> <path> [--name <text>] | \
    list <id> | remove <id> <n> | save <id> <n> <path>";
    U77 (PrCheck) = "usage: story pr-check [<id>]";
    U78 (Help) = "usage: story help [<topic>] [--all|--compact]";
    U79 (Plugin) = "usage: story plugin install|uninstall <claude|codex> | story plugin reinstall | story plugin run codex -- <helper-command> [args...]";
    U80 (Store) = "usage: story store new <path> | story store backup [--label <text>]";
    U81 (Daemon) = "usage: story daemon start [--port <PORT>] | restart | stop [--force] | status | \
                 install [--this-binary] | uninstall | token | gc [--force] | logs [--follow] [--directory <PATH>]";
    U82 (Web) = "usage: story web start [--port <PORT>] | stop | status | open | address";
    U83 (Token) = "usage: story token new <name> | story token list | story token revoke <name>";
    U84 (Show) = "usage: story show <id>";
    U85 (Log) = "usage: story log <id>";
    U86 (Comment) = "usage: story comment <id> \"<text>\"";
    U87 (Move) = "usage: story move <id> <state> [--if-state <expected>] [--reason <text>] [\"<comment>\"]";
    U88 (Close) = "usage: story close <id> \"<reason>\"";
    U89 (Block) = "usage: story block <id> --on <blocker> [--on <blocker>]... \
                          [\"<reason>\"] | story block <id> \"<reason>\"";
    U90 (Unblock) = "usage: story unblock <id> [--on <blocker>]...";
    U91 (Prioritize) = "usage: story prioritize <id> <level>";
    U92 (Label) = "usage: story label <id> <labels-csv>";
    U93 (Unlabel) = "usage: story unlabel <id> <labels-csv>";
    U94 (Reopen) = "usage: story reopen <id>";
    U95 (Archive) = "usage: story archive <id>";
    U96 (Unarchive) = "usage: story unarchive <id>";
    U97 (ArchiveState) = "usage: story archive-state <state> [--force]";
    U98 (Delete) = "usage: story delete <id> [--force]";
    U99 (Relate) = "usage: story relate <a> <relationship-type> <b>";
    U100 (Unrelate) = "usage: story unrelate <a> <relationship-type> <b>";
    U101 (Set) = "usage: story set <id> [--field value ...]";
    U102 (Set) = "usage: story set <id> [--title \"<title>\"] [--state <slug>] [--priority <level>] [--complexity low|medium|high] [--labels \"<csv>\"] [--blocked \"<reason>\"] [--unblocked] [--json \"<json>\"] [--type <slug>] [--description \"<text>\"]";
    U103 (Reset) = "usage: story reset <id> [--force] [--dry-run]";
    U104 (Summary) = "usage: story summary";
    U105 (Export) = "usage: story export";
    U106 (LaneBudget) = "usage: story lane-budget";
    U107 (SessionEligibility) = "usage: story session-eligibility <id>";
    U108 (SessionStart) = "usage: story session-start";
    U109 (Continuation) = "usage: story continuation request <id> --stdin | status <id> | receipt <id> <request> --stdin | retry <id> <request> | ack <id> <request> --reviewed-seq <n> --head <sha> --provider <codex|claude> --session-id <id> | capabilities";
    U110 (DispatchPolicy) = "usage: story dispatch-policy show|set|reset|resolve [<story-id>] [--global] [--agent codex|claude] [--complexity low|medium|high] [--model <id>] [--effort <id>]. For reset, --model and --effort take no value. See story help dispatch-policy";
    U111 (Github) = "usage: story github observe --checkout PATH [--authority PATH] -- ls-remote|fetch ARGUMENTS";
    U112 (Github) = "usage: story github resolve|exec|git|merge --checkout PATH [--authority PATH] [--expected HOST/OWNER/REPO] [-- ARGUMENTS]";
}

/// Local protocols before current-directory and project resolution.
pub enum BeforeEnvironment<'a> {
    Logs {
        follow: bool,
        directory: Option<&'a std::path::Path>,
    },
    Serve {
        port: Option<u16>,
        owner: Option<&'a str>,
    },
}
/// Resolve an already parsed invocation without accessing the runtime.
pub fn before_environment(invocation: &Invocation) -> Option<BeforeEnvironment<'_>> {
    match invocation {
        Invocation::Daemon {
            action: DaemonAction::Logs { follow, directory },
        } => Some(BeforeEnvironment::Logs {
            follow: *follow,
            directory: directory.as_deref(),
        }),
        Invocation::Daemon {
            action: DaemonAction::Serve { port, owner },
        } => Some(BeforeEnvironment::Serve {
            port: *port,
            owner: owner.as_deref(),
        }),
        Invocation::Web {
            action: WebAction::Serve { port },
        } => Some(BeforeEnvironment::Serve {
            port: *port,
            owner: None,
        }),
        _ => None,
    }
}
/// Local protocols after current-directory resolution but before opening a store.
pub enum BeforeStore<'a> {
    Plugin { target: &'a str, args: &'a [String] },
    StoreNew { path: &'a str },
}
pub fn before_store(invocation: &Invocation) -> Option<BeforeStore<'_>> {
    match invocation {
        Invocation::Plugin {
            action: PluginAction::Run { target, args },
        } => Some(BeforeStore::Plugin { target, args }),
        Invocation::Store {
            action: StoreAction::New { path },
        } => Some(BeforeStore::StoreNew { path }),
        _ => None,
    }
}
/// Interactive preparation is conditional; stated project creation skips it.
pub fn needs_questionnaire(invocation: &Invocation) -> bool {
    matches!(
        invocation,
        Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Ask)
        }
    )
}
