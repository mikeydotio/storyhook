//! Authoritative command registration and option grammar (SH-898).
//!
//! Registration creates both the finite command identity and its parser binding.
//! Help and flag validation resolve this same identity; aliases do not acquire a
//! separate grammar. Existing parsers retain their legacy acceptance semantics.
use super::*;

pub mod grammar;

/// The entry boundary; it is not an authorization or visibility classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum FamilyHandler {
    /// Ordinary invocation parsing (including local invocation handlers).
    Parsed,
    /// Raw local GitHub protocol; global flags belong to the delegated tool.
    Github,
    /// Terminal UI after global option parsing.
    Tui,
    /// Pure offline command discovery.
    Describe,
}

/// Closed set of handlers that run before global option parsing.
pub enum BeforeGlobals {
    Github,
}
/// Closed set of handlers that run after globals and before invocation parsing.
pub enum BeforeInvocation {
    Tui,
    /// Pure offline command discovery.
    Describe,
}

/// No project, store, daemon, environment or helper is consulted for routing.
pub fn before_globals(args: &[String]) -> Option<BeforeGlobals> {
    match CommandId::find(args.first()?)?.handler() {
        FamilyHandler::Github => Some(BeforeGlobals::Github),
        FamilyHandler::Parsed | FamilyHandler::Tui | FamilyHandler::Describe => None,
    }
}

/// Preserve the existing terminal help precedence, including legacy terminators.
pub fn before_invocation(args: &[String]) -> Option<BeforeInvocation> {
    let handler = CommandId::find(args.first()?)?.handler();
    // Discovery owns its path words, including the registered --help spelling.
    // Existing commands retain their legacy help precedence.
    if handler == FamilyHandler::Describe {
        return Some(BeforeInvocation::Describe);
    }
    if super::is_help_request(args) {
        return None;
    }
    match handler {
        FamilyHandler::Tui => Some(BeforeInvocation::Tui),
        FamilyHandler::Describe => Some(BeforeInvocation::Describe),
        FamilyHandler::Parsed | FamilyHandler::Github => None,
    }
}

macro_rules! commands {
    ($( $variant:ident [$($name:literal),+] $handler:ident $grammar:expr; ($arg:ident) => $body:expr, )*) => {
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
            pub const fn grammar(self) -> Grammar {
                match self { $(Self::$variant => $grammar,)* }
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
    HelpFlag ["-h", "--help"] Parsed Grammar::new("[<ignored>...]", "", FormKind::Command); (args) => Ok(Invocation::Help),
    VersionFlag ["-V", "--version"] Parsed Grammar::new("[<ignored>...]", "", FormKind::Command); (args) => Ok(Invocation::Version),
    Describe ["describe"] Describe Grammar::new("[<command-path>...] [--audience <audience:audiences>]", "", FormKind::Early); (args) => unknown(&args[0]),
    Help ["help"] Parsed Grammar::new("[<topic:help-topics>] [--all | --compact]", "", FormKind::Command); (args) => parse_help(args),
    Mcp ["mcp"] Parsed Grammar::new("", "", FormKind::Retired); (args) => Err(AppError::Usage(
            "`story mcp` is retired. Use CLI commands with --json instead. \
             Remove the storyhook MCP server from your host configuration. \
             Run `story help agent-guide` and `story help json-format` for migration guidance."
                .into(),
        )),
    Update ["update"] Parsed Grammar::new("[--check] [--force] [--source <repository>]", "", FormKind::Command); (args) => parse_update(args),
        // Not left to fall through to `unknown command`. Five years of
        // documents, this repo's own plugin skill, and every agent that has
        // ever seen storyhook all say `story init`; the least useful thing to
        // tell any of them is that no such command exists.
    Init ["init"] Parsed Grammar::new("", "", FormKind::Retired); (args) => Err(AppError::Usage(
            "`story init` is now `story project new`.\n\nThe project verbs moved into one \
             group: `story project new`, `story project list`, `story project delete`.\n\n  \
             story project new --prefix <PREFIX>"
                .to_string(),
        )),
    Project ["project"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_project(args),
    DispatchPolicy ["dispatch-policy"] Parsed Grammar::new("", "", FormKind::Command); (args) => dispatch_policy::parse(args),
    New ["new"] Parsed Grammar::new("<title>... [--state <state:states>] [--type <type:types>] [--description <text>] [--priority <priority:priority>] [--complexity <complexity:complexity>] [--label <label:labels>]... [--labels <csv:labels>] [--blocked-by <id:stories>]... [--draft]", "example", FormKind::Command); (args) => parse_new(args),
    State ["state"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_state(args),
    List ["list"] Parsed Grammar::new("[--state <state:states>] [--priority <csv:priority>] [--label <csv:labels>] [--created-after <date>] [--updated-after <date>] [--stale <duration>] [--phase <phase:phases>] [--type <type:types>] [--flagged] [--blocked] [--ready] [--drafts] [--unassessed] [--include-closed] [--include-archived] [--all]", "", FormKind::Command); (args) => parse_list(args),
    Next ["next"] Parsed Grammar::new("[--count <count>] [--phase <phase:phases>] [--epic <id:stories>] [--exclude-label <csv:labels>]", "", FormKind::Command); (args) => parse_next(args),
    Claim ["claim"] Parsed Grammar::new("(<id:stories> | --next [--phase <phase:phases>] [--epic <id:stories>] [--exclude-label <csv:labels>]) [--comment <text> | --no-comment] [--dry-run]", "SH-1", FormKind::Command); (args) => parse_claim(args),
    Unclaim ["unclaim"] Parsed Grammar::new("<id:stories> [--comment <text> | --no-comment] [--dry-run]", "SH-1", FormKind::Command); (args) => parse_unclaim(args),
    Reset ["reset"] Parsed Grammar::new("<id:stories> [--force] [--dry-run]", "SH-1", FormKind::Command); (args) => {
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
                            usage::RESET_1.into(),
                        ));
                    }
                }
            }
            let id = id.ok_or_else(|| {
                AppError::Usage(usage::RESET_1.into())
            })?;
            let caller = crate::service::reset::ResetCaller::capture();
            if dry_run {
                Ok(Invocation::ResetPreview { id, caller })
            } else {
                Ok(Invocation::Reset { id, force, caller })
            }
        },
    Internal ["internal"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_internal(args),
    Engine ["engine"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_engine(args),
    Verifier ["verifier"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_verifier(args),
    Cleanup ["cleanup"] Parsed Grammar::new("[--dry-run]", "", FormKind::Command); (args) => parse_cleanup(args),
    Resources ["resources"] Parsed Grammar::new("<id:stories> [--lease-json <json>] [--window-name <name>] [--worktree-root <path>] [--tmux-socket <path>] [--location-only]", "SH-1", FormKind::Command); (args) => parse_resources(args),
    Summary ["summary"] Parsed Grammar::new("", "", FormKind::Command); (args) => {
            expect_no_more(&args[1..], usage::SUMMARY_1)?;
            Ok(Invocation::Summary)
        },
    Report ["report"] Parsed Grammar::new("[--html]", "", FormKind::Command); (args) => parse_report(args),
    Search ["search"] Parsed Grammar::new("<query>...", "example", FormKind::Command); (args) => parse_search(args),
    Import ["import"] Parsed Grammar::new("[<file>]", "", FormKind::Command); (args) => parse_import(args),
    Decompose ["decompose"] Parsed Grammar::new("(<file> | --stdin) [--dry-run]", "example.md", FormKind::Command); (args) => parse_decompose(args),
    ImportProject ["import-project"] Parsed Grammar::new("<file> [--legacy-links]", "example.json", FormKind::Command); (args) => parse_import_project(args),
    Migrate ["migrate"] Parsed Grammar::new("[<path>] [--dry-run]", "", FormKind::Command); (args) => parse_migrate(args),
        // Deleted rather than redirected-and-kept: `link checkout` is strictly
        // more capable. `relink` needed a pointer file in the directory it was
        // pointed at, which is precisely what a checkout that has been moved,
        // renamed or freshly cloned may not have; `link checkout` records the
        // path against a project named the ordinary way and asks the directory
        // for nothing.
    Relink ["relink"] Parsed Grammar::new("", "", FormKind::Retired); (args) => Err(AppError::Usage(
            "`story relink` is now `story project link checkout`.\n\nIt no longer reads a \
             pointer file, so it works for a checkout that never had one:\n\n  story --project \
             <SLUG> project link checkout <PATH>"
                .to_string(),
        )),
    Export ["export"] Parsed Grammar::new("", "", FormKind::Command); (args) => {
            expect_no_more(&args[1..], usage::EXPORT_1)?;
            Ok(Invocation::Export)
        },
    LoadContext ["load-context", "context"] Parsed Grammar::new("[--format <format:context-format>] [--story <id:stories>]", "", FormKind::Command); (args) => parse_context(args),
    Phase ["phase"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_phase(args),
    Type ["type"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_type(args),
    Epic ["epic"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_epic(args),
    Handoff ["handoff"] Parsed Grammar::new("[--since <duration>]", "", FormKind::Command); (args) => parse_handoff(args),
    Graph ["graph"] Parsed Grammar::new("[--critical-path | --blocked-by <id:stories> | --parallel-groups] [<ignored>...]", "", FormKind::Command); (args) => parse_graph(args),
    Doctor ["doctor"] Parsed Grammar::new("[--fix]", "", FormKind::Command); (args) => parse_doctor(args),
    LaneBudget ["lane-budget"] Parsed Grammar::new("", "", FormKind::Command); (args) => {
            expect_no_more(&args[1..], usage::LANE_BUDGET_1)?;
            Ok(Invocation::LaneBudget)
        },
    Hooks ["hooks"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_hooks(args),
    Scaffold ["scaffold"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_scaffold(args),
    CommitSync ["commit-sync", "sync-git"] Parsed Grammar::new("[--since <duration>]", "", FormKind::Command); (args) => parse_commit_sync(args),
    LinkPr ["link-pr"] Parsed Grammar::new("<id:stories> <url> [--no-close-on-merge]", "SH-1 https://example.invalid/pr/1", FormKind::Command); (args) => parse_link_pr(args),
    UnlinkPr ["unlink-pr"] Parsed Grammar::new("<id:stories> <url>", "SH-1 https://example.invalid/pr/1", FormKind::Command); (args) => parse_unlink_pr(args),
    Attachment ["attachment"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_attachment(args),
    PrCheck ["pr-check"] Parsed Grammar::new("[<id:stories>]", "", FormKind::Command); (args) => parse_pr_check(args),
    Plugin ["plugin"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_plugin(args),
    Web ["web"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_web(args),
    Token ["token"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_token(args),
    Daemon ["daemon"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_daemon(args),
    Store ["store"] Parsed Grammar::new("", "", FormKind::Group); (args) => parse_store(args),
    Continuation ["continuation"] Parsed Grammar::new("", "", FormKind::Group); (args) => continuation::parse(args),
    SessionEligibility ["session-eligibility"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => {
            if args.len() != 2 {
                return Err(AppError::Usage(
                    usage::SESSION_ELIGIBILITY_1.into(),
                ));
            }
            Ok(Invocation::SessionEligibility {
                id: args[1].clone(),
            })
        },
    Show ["show"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_show(args),
    Log ["log"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_log(args),
    Comment ["comment"] Parsed Grammar::new("<id:stories> <text>...", "SH-1 example", FormKind::Command); (args) => parse_comment(args),
    Move ["move"] Parsed Grammar::new("<id:stories> <state:states> [--if-state <expected:states>] [--reason <text>] [<comment>...]", "SH-1 done", FormKind::Command); (args) => parse_move(args),
    Close ["close"] Parsed Grammar::new("<id:stories> <reason>...", "SH-1 example", FormKind::Command); (args) => parse_close(args),
    Block ["block"] Parsed Grammar::new("<id:stories> (--on <blocker:stories> [--on <blocker:stories>]... [<reason>...] | <reason>...)", "SH-1 example", FormKind::Command); (args) => parse_block(args),
    Unblock ["unblock"] Parsed Grammar::new("<id:stories> [--on <blocker:stories>]...", "SH-1", FormKind::Command); (args) => parse_unblock(args),
    Prioritize ["prioritize"] Parsed Grammar::new("<id:stories> <priority:priority>", "SH-1 low", FormKind::Command); (args) => parse_prioritize(args),
    Label ["label"] Parsed Grammar::new("<id:stories> <csv:labels>", "SH-1 example", FormKind::Command); (args) => parse_label(args),
    Unlabel ["unlabel"] Parsed Grammar::new("<id:stories> <csv:labels>", "SH-1 example", FormKind::Command); (args) => parse_unlabel(args),
    Reopen ["reopen"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_reopen_verb(args),
    Archive ["archive"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_hide(args),
    Unarchive ["unarchive"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_unhide(args),
    ArchiveState ["archive-state"] Parsed Grammar::new("<state:states> [--force]", "done", FormKind::Command); (args) => parse_hide_state(args),
    Publish ["publish"] Parsed Grammar::new("<id:stories>", "SH-1", FormKind::Command); (args) => parse_publish(args),
    Delete ["delete"] Parsed Grammar::new("<id:stories> [--force]", "SH-1", FormKind::Command); (args) => parse_delete_verb(args),
    Purge ["purge"] Parsed Grammar::new("", "", FormKind::Retired); (args) => parse_purge_verb(args),
    Set ["set"] Parsed Grammar::new("<id:stories> [--title <title>] [--state <state:states>] [--priority <priority:priority>] [--complexity <complexity:complexity>] [--labels <csv:labels>] [--blocked <reason>] [--unblocked] [--input-json <object> | --json <object>] [--type <type:types>] [--description <text>]", "SH-1 --title example", FormKind::Command); (args) => parse_set(args),
    Relate ["relate", "link"] Parsed Grammar::new("<a:stories> <relation:relationships> <b:stories>", "SH-1 blocks SH-2", FormKind::Command); (args) => parse_relate(args),
    Unrelate ["unrelate", "unlink"] Parsed Grammar::new("<a:stories> <relation:relationships> <b:stories>", "SH-1 blocks SH-2", FormKind::Command); (args) => parse_unrelate(args),
    SessionStart ["session-start"] Parsed Grammar::new("", "", FormKind::Command); (args) => {
            expect_no_more(&args[1..], usage::SESSION_START_1)?;
            Ok(Invocation::SessionStart)
        },
    Github ["github"] Github Grammar::new("", "", FormKind::Group); (args) => unknown(&args[0]),
    Tui ["tui"] Tui Grammar::new("[<ignored>...]", "", FormKind::Early); (args) => unknown(&args[0]),
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
        command: CommandId::Describe,
        subcommand: None,
        flags: &[value("audience")],
    },
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
            value("input-json"),
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
    Describe => r#"  story describe [command path] --json [--audience task|operator|internal|all]
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
                  [--unblocked] [--input-json "<object>" | --json "<object>"] [--type <slug>]
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
    pub grammars: &'static [Grammar],
}
macro_rules! subcommands {
    ($( $group:ident ($root:ident [$($prefix:literal),*]) { $($variant:ident = $word:literal => $grammar:expr),+ $(,)? } )*) => {
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
            $(CommandGroup { command: CommandId::$root, prefix: &[$($prefix),*], words: &[$($word),+], grammars: &[$($grammar),+] },)*
        ];
    }
}
subcommands! {
    ProjectVerb (Project []) {
        New = "new" => Grammar::new("[--prefix <prefix> [--name <name>] [--attach <path> | --no-attach] [--no-agents-md]]", "--prefix EX --no-attach", FormKind::Command),
        Delete = "delete" => Grammar::new("[--force | -f]", "", FormKind::Command),
        SetPrefix = "set-prefix" => Grammar::new("<prefix> [--force | -f]", "EX", FormKind::Command),
        Init = "init" => Grammar::new("", "", FormKind::Retired),
        Deinit = "deinit" => Grammar::new("", "", FormKind::Retired),
        List = "list" => Grammar::new("", "", FormKind::Command),
        Show = "show" => Grammar::new("", "", FormKind::Command),
        Link = "link" => Grammar::new("", "", FormKind::Group),
        Unlink = "unlink" => Grammar::new("", "", FormKind::Group),
        Settings = "settings" => Grammar::new("", "", FormKind::Group),
    }
    ProjectLinkVerb (Project ["link"]) {
        Origin = "origin" => Grammar::new("[<url>]", "", FormKind::Command),
        Checkout = "checkout" => Grammar::new("[<path>]", "", FormKind::Command),
    }
    ProjectUnlinkVerb (Project ["unlink"]) {
        Origin = "origin" => Grammar::new("[<url>]", "", FormKind::Command),
        Checkout = "checkout" => Grammar::new("", "", FormKind::Command),
    }
    ProjectSettingsVerb (Project ["settings"]) {
        List = "list" => Grammar::new("", "", FormKind::Command),
        Get = "get" => Grammar::new("<key:project-settings>", "automations.enabled", FormKind::Command),
        Set = "set" => Grammar::new("<key:project-settings> <value>", "automations.enabled false", FormKind::Command),
        Unset = "unset" => Grammar::new("<key:project-settings>", "automations.enabled", FormKind::Command),
    }
    StateVerb (State []) {
        List = "list" => Grammar::new("", "", FormKind::Command),
        Add = "add" => Grammar::new("<slug> --super <super:superstate> [--role <role:active-role>] [--description <text>]", "review --super OPEN", FormKind::Command),
        Set = "set" => Grammar::new("<state:states> [--super <super:superstate>] [--role <role:state-role>] [--description <text> | --no-description] [--move-stories-to <state:states>]", "review --description example", FormKind::Command),
        Remove = "remove" => Grammar::new("<state:states> [--move-stories-to <state:states>]", "review", FormKind::Command),
        Reorder = "reorder" => Grammar::new("<csv:states>...", "todo done", FormKind::Command),
    }
    EngineVerb (Engine []) {
        ResetCheck = "reset-check" => Grammar::new("<id:stories>", "SH-1", FormKind::Command),
        ResetTarget = "reset-target" => Grammar::new("--run <run> --token <token>", "--run run --token token", FormKind::Command),
        Start = "start" => Grammar::new("[--epic <id:stories>] [--lanes <lanes>] [--agent <agent:providers>] [--model <model:provider-models>] [--effort <effort:provider-efforts>] [--speed <speed:speed>]", "", FormKind::Command),
        Configure = "configure" => Grammar::new("(--lanes <lanes> | --model <model:provider-models> | --effort <effort:provider-efforts> | --speed <speed:speed>)... [--run <run>]", "--lanes 1", FormKind::Command),
        Adopt = "adopt" => Grammar::new("<id:stories>... [--run <run>]", "SH-1", FormKind::Command),
        Status = "status" => Grammar::new("[--run <run>]", "", FormKind::Command),
        Pause = "pause" => Grammar::new("[--run <run>]", "", FormKind::Command),
        Resume = "resume" => Grammar::new("[--run <run>]", "", FormKind::Command),
        Stop = "stop" => Grammar::new("[--run <run>] [--now]", "", FormKind::Command),
        Ack = "ack" => Grammar::new("[--run <run>]", "", FormKind::Command),
    }
    VerifierVerb (Verifier []) {
        Landing = "landing" => Grammar::new("", "", FormKind::Group),
        Evidence = "evidence" => Grammar::new("<id:stories>", "SH-1", FormKind::Command),
        RepairAdmit = "repair-admit" => Grammar::new("<story:stories> <attempt> <generation> <base> <head> <head-tree> <tree>", "SH-1 attempt 1 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", FormKind::Command),
        Repair = "repair" => Grammar::new("", "", FormKind::Group),
        GateConfig = "gate-config" => Grammar::new("<checkout> <base> <head> <tree>", "/tmp/example aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", FormKind::Command),
        Status = "status" => Grammar::new("", "", FormKind::Command),
        Start = "start" => Grammar::new("", "", FormKind::Command),
        Stop = "stop" => Grammar::new("", "", FormKind::Command),
        Drain = "drain" => Grammar::new("", "", FormKind::Command),
        Ack = "ack" => Grammar::new("<incident> [--leave-stopped]", "incident", FormKind::Command),
    }
    PhaseVerb (Phase []) {
        List = "list" => Grammar::new("", "", FormKind::Command),
        Show = "show" => Grammar::new("<phase:phases> [<ignored>...]", "1", FormKind::Command),
        Add = "add" => Grammar::new("<id:stories> <phase:phases> [<ignored>...]", "SH-1 1", FormKind::Command),
        Remove = "remove" => Grammar::new("<id:stories>", "SH-1", FormKind::Command),
        Create = "create" => Grammar::new("<phase:phases> [<title>...]", "1 example", FormKind::Command),
    }
    TypeVerb (Type []) {
        List = "list" => Grammar::new("", "", FormKind::Command),
        Add = "add" => Grammar::new("<slug> [--description <text>] [--emoji <glyph>]", "example", FormKind::Command),
        Set = "set" => Grammar::new("<type:types> [--description <text> | --no-description] [--emoji <glyph> | --no-emoji]", "example --description example", FormKind::Command),
        Remove = "remove" => Grammar::new("<type:types>", "example", FormKind::Command),
    }
    EpicVerb (Epic []) {
        List = "list" => Grammar::new("", "", FormKind::Command),
        Show = "show" => Grammar::new("<id:stories>", "SH-1", FormKind::Command),
        Create = "create" => Grammar::new("<title>...", "example", FormKind::Command),
        Add = "add" => Grammar::new("<epic:stories> <story:stories>", "SH-1 SH-2", FormKind::Command),
    }
    HooksVerb (Hooks []) {
        Install = "install" => Grammar::new("", "", FormKind::Command),
        Uninstall = "uninstall" => Grammar::new("", "", FormKind::Command),
        List = "list" => Grammar::new("", "", FormKind::Command),
        Test = "test" => Grammar::new("<event:hook-events>", "story.moved", FormKind::Command),
    }
    AttachmentVerb (Attachment []) {
        Add = "add" => Grammar::new("<id:stories> <path> [--name <name>]", "SH-1 example.txt", FormKind::Command),
        List = "list" => Grammar::new("<id:stories>", "SH-1", FormKind::Command),
        Remove = "remove" => Grammar::new("<id:stories> <number>", "SH-1 1", FormKind::Command),
        Save = "save" => Grammar::new("<id:stories> <number> <path>", "SH-1 1 example.txt", FormKind::Command),
    }
    PluginVerb (Plugin []) {
        Install = "install" => Grammar::new("<provider:providers>", "codex", FormKind::Command),
        Uninstall = "uninstall" => Grammar::new("<provider:providers>", "codex", FormKind::Command),
        Reinstall = "reinstall" => Grammar::new("", "", FormKind::Command),
        Run = "run" => Grammar::new("<provider:codex-launcher> [--] <helper-command> [<argument>...]", "codex -- example", FormKind::Command),
    }
    StoreVerb (Store []) {
        New = "new" => Grammar::new("<path>", "/tmp/example.db", FormKind::Command),
        Backup = "backup" => Grammar::new("[--label <label>]", "", FormKind::Command),
    }
    DaemonVerb (Daemon []) {
        Logs = "logs" => Grammar::new("[--follow] [--directory <path>]", "", FormKind::Command),
        Start = "start" => Grammar::new("[--port <port>]", "", FormKind::Command),
        Restart = "restart" => Grammar::new("", "", FormKind::Command),
        Serve = "--serve" => Grammar::new("[--port <port>] [--owner <owner>]", "", FormKind::Command),
        Stop = "stop" => Grammar::new("[--force]", "", FormKind::Command),
        Gc = "gc" => Grammar::new("[--force]", "", FormKind::Command),
        Status = "status" => Grammar::new("", "", FormKind::Command),
        Install = "install" => Grammar::new("[--this-binary]", "", FormKind::Command),
        Uninstall = "uninstall" => Grammar::new("", "", FormKind::Command),
        Token = "token" => Grammar::new("", "", FormKind::Command),
    }
    WebVerb (Web []) {
        Start = "start" => Grammar::new("[--port <port>]", "", FormKind::Command),
        Stop = "stop" => Grammar::new("", "", FormKind::Command),
        Status = "status" => Grammar::new("", "", FormKind::Command),
        Open = "open" => Grammar::new("", "", FormKind::Command),
        Address = "address" => Grammar::new("", "", FormKind::Command),
        Revoke = "revoke" => Grammar::new("", "", FormKind::Retired),
        Serve = "--serve" => Grammar::new("[--port <port>]", "", FormKind::Command),
    }
    TokenVerb (Token []) {
        New = "new" => Grammar::new("<name>", "example", FormKind::Command),
        List = "list" => Grammar::new("", "", FormKind::Command),
        Revoke = "revoke" => Grammar::new("<name>", "example", FormKind::Command),
    }
    VerifierLandingVerb (Verifier ["landing"]) { Show = "show" => Grammar::new("", "", FormKind::Command), Release = "release" => Grammar::new("<intent> --reason <reason>", "intent --reason example", FormKind::Command) }

    VerifierRepairVerb (Verifier ["repair"]) { Show = "show" => Grammar::new("<recovery>", "recovery", FormKind::Command), Decide = "decide" => Grammar::new("<recovery> --input <json-file>", "recovery --input input.json", FormKind::Command), Satisfy = "satisfy" => Grammar::new("<recovery> --input <json-file>", "recovery --input input.json", FormKind::Command) }

    InternalVerb (Internal []) { SupersedeBlockDeliveries = "supersede-block-deliveries" => Grammar::new("<id:stories>", "SH-1", FormKind::Command), SupersedeContinuations = "supersede-continuations" => Grammar::new("<id:stories>", "SH-1", FormKind::Command) }

    DoctorVerb (Doctor []) { Abandoned = "abandoned" => Grammar::new("", "", FormKind::Command), Crashes = "crashes" => Grammar::new("", "", FormKind::Command), Install = "install" => Grammar::new("", "", FormKind::Command) }

    DoctorAbandonedVerb (Doctor ["abandoned"]) { Clear = "clear" => Grammar::new("(--all | <request>)", "--all", FormKind::Command) }

    DoctorCrashesVerb (Doctor ["crashes"]) { Clear = "clear" => Grammar::new("(--all | <crash>)", "--all", FormKind::Command) }

    ScaffoldVerb (Scaffold []) { AgentsMd = "agents-md" => Grammar::new("", "", FormKind::Command), ClaudeMd = "claude-md" => Grammar::new("", "", FormKind::Command), CursorRules = "cursor-rules" => Grammar::new("", "", FormKind::Command) }

    ContinuationVerb (Continuation []) { Capabilities = "capabilities" => Grammar::new("", "", FormKind::Command), Request = "request" => Grammar::new("<id:stories> --stdin", "SH-1 --stdin", FormKind::Command), Status = "status" => Grammar::new("<id:stories>", "SH-1", FormKind::Command), Receipt = "receipt" => Grammar::new("<id:stories> <request> --stdin", "SH-1 request --stdin", FormKind::Command), Retry = "retry" => Grammar::new("<id:stories> <request>", "SH-1 request", FormKind::Command), Ack = "ack" => Grammar::new("<id:stories> <request> --reviewed-seq <sequence> --head <sha> --provider <provider:providers> --session-id <session>", "SH-1 request --reviewed-seq 1 --head aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --provider codex --session-id session", FormKind::Command) }

    DispatchPolicyVerb (DispatchPolicy []) { Show = "show" => Grammar::new("[--global]", "", FormKind::Command), Set = "set" => Grammar::new("[--global] --agent <agent:providers> --complexity <complexity:complexity> (--model <model:provider-models> | --effort <effort:provider-efforts>)...", "--agent codex --complexity low --model example", FormKind::Command), Reset = "reset" => Grammar::new("[--global] --agent <agent:providers> --complexity <complexity:complexity> [--model] [--effort]", "--agent codex --complexity low", FormKind::Command), Resolve = "resolve" => Grammar::new("<id:stories> --agent <agent:providers>", "SH-1 --agent codex", FormKind::Command) }

    GithubVerb (Github []) { Observe = "observe" => Grammar::new("--checkout <path> [--authority <path>] -- (ls-remote | fetch) <argument>...", "--checkout /tmp/example -- ls-remote origin", FormKind::Early), Resolve = "resolve" => Grammar::new("--checkout <path> [--authority <path>] [--expected <repository>]", "--checkout /tmp/example", FormKind::Early), Merge = "merge" => Grammar::new("--checkout <path> [--authority <path>] [--expected <repository>] -- <number> <head>", "--checkout /tmp/example -- 1 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", FormKind::Early), Exec = "exec" => Grammar::new("--checkout <path> [--authority <path>] [--expected <repository>] -- <argument>...", "--checkout /tmp/example -- repo view", FormKind::Early), Git = "git" => Grammar::new("--checkout <path> [--authority <path>] [--expected <repository>] -- <argument>...", "--checkout /tmp/example -- ls-remote origin", FormKind::Early) }

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
    INTERNAL_1 (Internal) = "usage: story internal supersede-block-deliveries <id> --json\n       \
             story internal supersede-continuations <id> --json";
    RESOURCES_1 (Resources) = "usage: story resources <id> [--lease-json JSON] [--window-name NAME] [--worktree-root PATH] [--tmux-socket PATH] [--location-only]";
    CLEANUP_1 (Cleanup) = "usage: story cleanup [--dry-run]";
    CLAIM_1 (Claim) = "usage: story claim <id> [--comment <text> | --no-comment] \
                           [--dry-run]\n       story claim --next [--phase <N>] [--epic <id>] \
                           [--exclude-label <csv>] [--comment <text> | --no-comment] \
                           [--dry-run]";
    UNCLAIM_1 (Unclaim) = "usage: story unclaim <id> [--comment <text> | --no-comment] \
                             [--dry-run]";
    PROJECT_1 (Project) = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                             [--attach <PATH> | --no-attach] [--no-agents-md] | delete \
                             [--force] | set-prefix <NEW-PREFIX> [--force] | show | list | \
                             link origin [URL]|checkout [PATH] | unlink origin [URL]|checkout \
                             | settings list|get|set|unset";
    PROJECT_2 (Project) = "usage: story project show\n\n`story project show` takes no \
                                  argument. It reports the project this directory resolves \
                                  to — name a different one with `--project <slug>`.";
    PROJECT_3 (Project) = "usage: story project delete [--force]\n\n`story project \
                                    delete` takes no positional argument. It destroys the \
                                    project this directory resolves to; name a different one \
                                    with `--project <slug>`.";
    PROJECT_4 (Project) = "usage: story project set-prefix <NEW-PREFIX> \
                                        [--force]\n\n`story project set-prefix` takes exactly \
                                        one positional argument, the new prefix. It rewrites \
                                        the project this directory resolves to; name a \
                                        different one with `--project <slug>`.";
    PROJECT_5 (Project) = "usage: story project new [--prefix <PREFIX>] [--name <NAME>] \
                                 [--attach <PATH> | --no-attach] [--no-agents-md]\n\nRun with no \
                                 flags at a terminal to be asked. `story project new` takes no \
                                 positional argument: name the project with --name and the \
                                 checkout with --attach.";
    PROJECT_6 (Project) = "usage: story project link origin [URL] | story project link checkout [PATH]\n\nThese attach \
     *git* associations to a project. They are unrelated to `story link`, which is an alias for \
     `story relate` and joins one story to another.";
    PROJECT_7 (Project) = "usage: story project unlink origin [URL] | story project unlink checkout\n\n`unlink \
     checkout` takes no path: a project has at most one. These are unrelated to `story unlink`, \
     which is an alias for `story unrelate`.";
    PROJECT_8 (Project) = "usage: story project settings list | get <key> | \
                                      set <key> <value> | unset <key>";
    NEW_1 (New) = "usage: story new <title> [--state <slug>] [--type <slug>] [--description <text>] [--priority <level>] [--complexity low|medium|high] [--label <name> ...] [--labels <csv>] [--blocked-by <id> ...] [--draft]";
    PUBLISH_1 (Publish) = "usage: story publish <id>";
    TYPE_1 (Type) = "usage: story type list | story type add <slug> [...] | story type set <slug> [...] | story type remove <slug>";
    TYPE_2 (Type) = "usage: story type add <slug> [--description \"<text>\"] [--emoji <glyph>]";
    TYPE_3 (Type) = "usage: story type set <slug> [--description \"<text>\"] [--no-description] [--emoji <glyph>] [--no-emoji]";
    TYPE_4 (Type) = "usage: story type remove <slug>";
    STATE_1 (State) = "usage: story state list | story state add <slug> --super OPEN|CLOSED | story state set <slug> [...] | story state remove <slug> | story state reorder <slug,...>";
    STATE_2 (State) = "usage: story state add <slug> --super OPEN|CLOSED [--role active] [--description \"<text>\"]";
    STATE_3 (State) = "usage: story state set <slug> [--super OPEN|CLOSED] [--role active|none] [--description \"<text>\"] [--no-description] [--move-stories-to <slug>]";
    STATE_4 (State) = "usage: story state remove <slug> [--move-stories-to <slug>]";
    STATE_5 (State) = "usage: story state reorder <slug,slug,...>";
    LIST_1 (List) = "usage: story list [--state <slug>] [--flagged] [--priority <levels>] [--label <labels>] [--created-after <date>] [--updated-after <date>] [--blocked] [--ready] [--stale <duration>] [--phase <N>] [--type <slug>] [--drafts] [--unassessed] [--include-closed] [--include-archived] [--all]";
    NEXT_1 (Next) = "usage: story next [--count <n>] [--phase <N>] [--epic <id>] \
                 [--exclude-label <csv>]";
    ENGINE_1 (Engine) = "usage: story engine start [--epic <id>] [--lanes <n>] [--agent claude|codex] [--model <id>] [--effort <id>] [--speed standard|fast]";
    ENGINE_2 (Engine) = "usage: story engine status [--run <id>]";
    ENGINE_3 (Engine) = "usage: story engine pause [--run <id>]";
    ENGINE_4 (Engine) = "usage: story engine resume [--run <id>]";
    ENGINE_5 (Engine) = "usage: story engine stop [--run <id>] [--now]";
    ENGINE_6 (Engine) = "usage: story engine ack [--run <id>]";
    ENGINE_7 (Engine) = "usage: story engine <start|configure|adopt|status|pause|resume|stop|ack>";
    ENGINE_8 (Engine) = "usage: story engine reset-check <story-id>";
    ENGINE_9 (Engine) = "usage: story engine reset-target --run <id> --token <token>";
    ENGINE_10 (Engine) = "usage: story engine adopt <id> [<id> ...] [--run <id>]";
    ENGINE_11 (Engine) = "usage: story engine configure (--lanes <n> | --model <id> | --effort <id> | --speed standard|fast) [--run <id>]";
    VERIFIER_1 (Verifier) = "usage: story verifier ack <incident-id> [--leave-stopped]";
    VERIFIER_2 (Verifier) = "usage: story verifier <status|evidence|landing|start|stop|drain|ack|repair>";
    VERIFIER_3 (Verifier) = "usage: story verifier landing show | release <intent-id> --reason <reason>";
    VERIFIER_4 (Verifier) = "usage: story verifier evidence <story-id> [--json]";
    VERIFIER_5 (Verifier) = "usage: story verifier repair-admit <story> <attempt> <generation> <base> <head> <head-tree> <tree> --json (private verifier callback)";
    VERIFIER_6 (Verifier) = "usage: story verifier repair show <recovery-id> | decide <recovery-id> --input <json-file> | satisfy <recovery-id> --input <json-file>";
    VERIFIER_7 (Verifier) = "usage: story verifier gate-config <checkout> <base> <head> <tree> --json";
    VERIFIER_8 (Verifier) = "usage: story verifier <status|start|stop|drain>";
    REPORT_1 (Report) = "usage: story report [--html]";
    SEARCH_1 (Search) = "usage: story search <query>";
    IMPORT_1 (Import) = "usage: story import [<file>]";
    DECOMPOSE_1 (Decompose) = "usage: story decompose <file> [--dry-run] | story decompose --stdin [--dry-run]";
    IMPORT_PROJECT_1 (ImportProject) = "usage: story import-project <file> [--legacy-links]";
    MIGRATE_1 (Migrate) = "usage: story migrate [<path>] [--dry-run]";
    LOAD_CONTEXT_1 (LoadContext) = "usage: story load-context [--format markdown|json] [--story <id>]";
    PHASE_1 (Phase) = "usage: story phase list|show <N>|add <id> <N>|remove <id>|create <N> [\"<title>\"]";
    PHASE_2 (Phase) = "usage: story phase show <N>";
    PHASE_3 (Phase) = "usage: story phase add <id> <N>";
    PHASE_4 (Phase) = "usage: story phase remove <id>";
    PHASE_5 (Phase) = "usage: story phase create <N> [\"<title>\"]";
    EPIC_1 (Epic) = "usage: story epic list|show <id>|create \"<title>\"|add <epic-id> <story-id>";
    EPIC_2 (Epic) = "usage: story epic show <id>";
    EPIC_3 (Epic) = "usage: story epic create \"<title>\"";
    EPIC_4 (Epic) = "usage: story epic add <epic-id> <story-id>";
    HANDOFF_1 (Handoff) = "usage: story handoff [--since <duration>]";
    GRAPH_1 (Graph) = "usage: story graph --blocked-by <id>";
    GRAPH_2 (Graph) = "usage: story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]";
    DOCTOR_1 (Doctor) = "usage: story doctor install";
    DOCTOR_2 (Doctor) = "usage: story doctor [--fix] | install | abandoned [clear (--all | <request-id>)] \
         | crashes [clear (--all | <crash-id>)]";
    DOCTOR_3 (Doctor) = "usage: story doctor abandoned [clear (--all | <request-id>)]";
    DOCTOR_4 (Doctor) = "usage: story doctor crashes [clear (--all | <crash-id>)]";
    UPDATE_1 (Update) = "usage: story update [--check] [--force] [--source HOST/OWNER/REPO]";
    HOOKS_1 (Hooks) = "usage: story hooks install|uninstall|list|test <event_type>";
    HOOKS_2 (Hooks) = "usage: story hooks test <event_type>";
    SCAFFOLD_1 (Scaffold) = "usage: story scaffold agents-md|claude-md|cursor-rules";
    COMMIT_SYNC_1 (CommitSync) = "usage: story commit-sync [--since <duration>]";
    LINK_PR_1 (LinkPr) = "usage: story link-pr <id> <url> [--no-close-on-merge]";
    UNLINK_PR_1 (UnlinkPr) = "usage: story unlink-pr <id> <url>";
    ATTACHMENT_1 (Attachment) = "usage: story attachment add <id> <path> [--name <text>] | \
    list <id> | remove <id> <n> | save <id> <n> <path>";
    PR_CHECK_1 (PrCheck) = "usage: story pr-check [<id>]";
    HELP_1 (Help) = "usage: story help [<topic>] [--all|--compact]";
    PLUGIN_1 (Plugin) = "usage: story plugin install|uninstall <claude|codex> | story plugin reinstall | story plugin run codex -- <helper-command> [args...]";
    STORE_1 (Store) = "usage: story store new <path> | story store backup [--label <text>]";
    DAEMON_1 (Daemon) = "usage: story daemon start [--port <PORT>] | restart | stop [--force] | status | \
                 install [--this-binary] | uninstall | token | gc [--force] | logs [--follow] [--directory <PATH>]";
    WEB_1 (Web) = "usage: story web start [--port <PORT>] | stop | status | open | address";
    TOKEN_1 (Token) = "usage: story token new <name> | story token list | story token revoke <name>";
    SHOW_1 (Show) = "usage: story show <id>";
    LOG_1 (Log) = "usage: story log <id>";
    COMMENT_1 (Comment) = "usage: story comment <id> \"<text>\"";
    MOVE_1 (Move) = "usage: story move <id> <state> [--if-state <expected>] [--reason <text>] [\"<comment>\"]";
    CLOSE_1 (Close) = "usage: story close <id> \"<reason>\"";
    BLOCK_1 (Block) = "usage: story block <id> --on <blocker> [--on <blocker>]... \
                          [\"<reason>\"] | story block <id> \"<reason>\"";
    UNBLOCK_1 (Unblock) = "usage: story unblock <id> [--on <blocker>]...";
    PRIORITIZE_1 (Prioritize) = "usage: story prioritize <id> <level>";
    LABEL_1 (Label) = "usage: story label <id> <labels-csv>";
    UNLABEL_1 (Unlabel) = "usage: story unlabel <id> <labels-csv>";
    REOPEN_1 (Reopen) = "usage: story reopen <id>";
    ARCHIVE_1 (Archive) = "usage: story archive <id>";
    UNARCHIVE_1 (Unarchive) = "usage: story unarchive <id>";
    ARCHIVE_STATE_1 (ArchiveState) = "usage: story archive-state <state> [--force]";
    DELETE_1 (Delete) = "usage: story delete <id> [--force]";
    RELATE_1 (Relate) = "usage: story relate <a> <relationship-type> <b>";
    UNRELATE_1 (Unrelate) = "usage: story unrelate <a> <relationship-type> <b>";
    SET_1 (Set) = "usage: story set <id> [--field value ...]";
    SET_2 (Set) = "usage: story set <id> [--title \"<title>\"] [--state <slug>] [--priority <level>] [--complexity low|medium|high] [--labels \"<csv>\"] [--blocked \"<reason>\"] [--unblocked] [--input-json \"<object>\" | --json \"<object>\"] [--type <slug>] [--description \"<text>\"]";
    RESET_1 (Reset) = "usage: story reset <id> [--force] [--dry-run]";
    SUMMARY_1 (Summary) = "usage: story summary";
    EXPORT_1 (Export) = "usage: story export";
    LANE_BUDGET_1 (LaneBudget) = "usage: story lane-budget";
    SESSION_ELIGIBILITY_1 (SessionEligibility) = "usage: story session-eligibility <id>";
    SESSION_START_1 (SessionStart) = "usage: story session-start";
    CONTINUATION_1 (Continuation) = "usage: story continuation request <id> --stdin | status <id> | receipt <id> <request> --stdin | retry <id> <request> | ack <id> <request> --reviewed-seq <n> --head <sha> --provider <codex|claude> --session-id <id> | capabilities";
    DISPATCH_POLICY_1 (DispatchPolicy) = "usage: story dispatch-policy show|set|reset|resolve [<story-id>] [--global] [--agent codex|claude] [--complexity low|medium|high] [--model <id>] [--effort <id>]. For reset, --model and --effort take no value. See story help dispatch-policy";
    GITHUB_1 (Github) = "usage: story github observe --checkout PATH [--authority PATH] -- ls-remote|fetch ARGUMENTS";
    GITHUB_2 (Github) = "usage: story github resolve|exec|git|merge --checkout PATH [--authority PATH] [--expected HOST/OWNER/REPO] [-- ARGUMENTS]";
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

/// A registered group, accepted form, retired refusal, or early local protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormKind {
    Command,
    Group,
    Retired,
    Early,
}

/// Canonical operand/option syntax and a concrete parser witness.
/// `[]` means optional, `()` groups alternatives, `|` chooses, and `...`
/// repeats the preceding item. Angle brackets name values and optional domains.
/// Options retain their legacy placement/duplicate rules in the bound parser.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Grammar {
    pub syntax: &'static str,
    pub example_tail: &'static str,
    pub kind: FormKind,
}
impl Grammar {
    pub const fn new(syntax: &'static str, example_tail: &'static str, kind: FormKind) -> Self {
        Self {
            syntax,
            example_tail,
            kind,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandPath {
    pub command: CommandId,
    /// Canonical words, including the family; operands are never path components.
    pub words: Vec<&'static str>,
    pub grammar: Grammar,
}
impl CommandPath {
    pub fn example(&self) -> Vec<String> {
        self.words
            .iter()
            .copied()
            .chain(self.grammar.example_tail.split_whitespace())
            .map(str::to_string)
            .collect()
    }
    pub fn aliases(&self) -> Vec<Vec<&'static str>> {
        self.command.names()[1..]
            .iter()
            .map(|alias| {
                let mut words = self.words.clone();
                words[0] = alias;
                words
            })
            .collect()
    }
}
/// Enumerate in registration order. Every nested entry is generated by the
/// same macro as the selector matched in the production parser.
pub fn paths() -> Vec<CommandPath> {
    let mut result = Vec::new();
    for command in CommandId::ALL {
        result.push(CommandPath {
            command: *command,
            words: vec![command.names()[0]],
            grammar: command.grammar(),
        });
        for group in GROUPS.iter().filter(|group| group.command == *command) {
            for (word, grammar) in group.words.iter().zip(group.grammars) {
                let words = std::iter::once(command.names()[0])
                    .chain(group.prefix.iter().copied())
                    .chain(std::iter::once(*word))
                    .collect();
                result.push(CommandPath {
                    command: *command,
                    words,
                    grammar: *grammar,
                });
            }
        }
    }
    result
}
/// Exact lookup; unlike dispatch this never interprets an operand as a path.
pub fn path(words: &[&str]) -> Option<CommandPath> {
    let command = CommandId::find(words.first()?)?;
    paths()
        .into_iter()
        .find(|entry| entry.command == command && entry.words[1..] == words[1..])
}

/// Topic aliases are help-only and never enter command dispatch.
pub struct HelpAlias {
    pub name: &'static str,
    pub command: CommandId,
}
pub const HELP_ALIASES: &[HelpAlias] = &[
    HelpAlias {
        name: "states",
        command: CommandId::State,
    },
    HelpAlias {
        name: "is",
        command: CommandId::Move,
    },
    HelpAlias {
        name: "awaits",
        command: CommandId::Block,
    },
    HelpAlias {
        name: "priority",
        command: CommandId::Prioritize,
    },
];
