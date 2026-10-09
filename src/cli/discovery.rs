//! Offline, versioned discovery. Only compiled command definitions are read.
use super::model::{self, CommandId as C, CommandPath, FormKind};
use crate::error::AppError;
use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    Task,
    Operator,
    Internal,
    All,
}
impl Audience {
    fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "task" => Ok(Self::Task),
            "operator" => Ok(Self::Operator),
            "internal" => Ok(Self::Internal),
            "all" => Ok(Self::All),
            _ => Err(AppError::Usage(
                "--audience must be task, operator, internal, or all".into(),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputClass {
    Envelope,
    RawDocument,
    JsonLines,
    DelegatedHelper,
    Terminal,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputContract {
    pub class: OutputClass,
    pub selection: &'static str,
    pub schema: &'static str,
    pub empty: &'static str,
    pub stdout: &'static str,
    pub stderr: &'static str,
    pub errors: &'static str,
    pub exit_status: &'static str,
    pub quiet: &'static str,
    pub follow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    None,
    Read,
    Write,
    Conditional,
}

#[derive(Clone, Debug, Serialize)]
pub struct Effects {
    /// Domain data access, distinct from daemon startup and its runtime files.
    pub store: Access,
    pub filesystem: Access,
    pub processes: Access,
    pub remote: Access,
    pub may_start_daemon: bool,
    pub event_hooks_may_run: bool,
    pub detail: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Capabilities {
    pub effects: Effects,
    pub dry_run: Option<&'static str>,
    pub confirmation: Option<&'static str>,
    pub guarded_write: Option<&'static str>,
    pub noninteractive: &'static str,
    pub uncertain_outcome: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Descriptor {
    pub path: Vec<&'static str>,
    pub aliases: Vec<Vec<&'static str>>,
    pub help_only_aliases: Vec<&'static str>,
    pub entry_kind: FormKind,
    pub audience: Audience,
    pub syntax: &'static str,
    pub arguments: model::grammar::Expression,
    pub grammar_notes: &'static str,
    pub help_topic: &'static str,
    pub output: Vec<OutputContract>,
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, Serialize)]
pub struct Document {
    pub schema: &'static str,
    pub schema_version: u32,
    pub cli_contract: &'static str,
    pub audience: Audience,
    pub requested_path: Vec<String>,
    pub visibility_is_authorization: bool,
    pub commands: Vec<Descriptor>,
}

/// Parse only discovery's arguments; the caller has already removed globals.
/// This function does not parse executable command examples (some legacy parsers
/// capture environment state), open a store, resolve a project, or invoke helpers.
pub fn describe(args: &[String]) -> Result<Document, AppError> {
    let mut audience = Audience::Task;
    let mut seen_audience = false;
    let mut words = Vec::new();
    let mut arguments = args.iter();
    while let Some(word) = arguments.next() {
        if word == "--audience" || word.starts_with("--audience=") {
            if seen_audience {
                return Err(AppError::Usage("--audience may be supplied once".into()));
            }
            seen_audience = true;
            let value = match word.split_once('=') {
                Some((_, value)) => value,
                None => arguments
                    .next()
                    .ok_or_else(|| AppError::Usage("--audience needs a value".into()))?,
            };
            audience = Audience::parse(value)?;
        } else {
            words.push(word.as_str());
        }
    }
    let selected = if words.is_empty() {
        None
    } else {
        Some(model::path(&words).ok_or_else(|| {
            AppError::Usage(format!(
                "unknown command path `{}`; run story describe --audience all --json",
                words.join(" ")
            ))
        })?)
    };
    let requested_path = selected.as_ref().map_or_else(Vec::new, |entry| {
        entry.words.iter().map(|word| (*word).into()).collect()
    });
    let mut commands = Vec::new();
    for path in model::paths() {
        if selected
            .as_ref()
            .is_some_and(|parent| !path.words.starts_with(&parent.words))
        {
            continue;
        }
        let descriptor = descriptor(&path)?;
        if audience == Audience::All || descriptor.audience == audience {
            commands.push(descriptor);
        }
    }
    commands.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Document {
        schema: "storyhook.command-discovery",
        schema_version: SCHEMA_VERSION,
        cli_contract: "legacy-compatible",
        audience,
        requested_path,
        visibility_is_authorization: false,
        commands,
    })
}

fn audience(path: &CommandPath) -> Audience {
    use Audience::*;
    // Exhaustive at the command-family boundary. Individual protocol subcommands
    // stay internal even when their operator siblings share the same family.
    match path.command {
        C::Internal
        | C::Resources
        | C::Continuation
        | C::SessionEligibility
        | C::SessionStart
        | C::Github
        | C::Mcp => Internal,
        C::Engine if matches!(path.words.get(1), Some(&"reset-check" | &"reset-target")) => {
            Internal
        }
        C::Verifier
            if matches!(path.words.get(1), Some(&"repair-admit" | &"gate-config"))
                || matches!(path.words.get(2), Some(&"decide" | &"satisfy")) =>
        {
            Internal
        }
        C::Daemon | C::Web if path.words.get(1) == Some(&"--serve") => Internal,
        C::Plugin if path.words.get(1) == Some(&"run") => Internal,
        C::Project
        | C::DispatchPolicy
        | C::State
        | C::Type
        | C::Engine
        | C::Verifier
        | C::Cleanup
        | C::ImportProject
        | C::Migrate
        | C::Store
        | C::Doctor
        | C::Update
        | C::LaneBudget
        | C::Hooks
        | C::Scaffold
        | C::Plugin
        | C::Web
        | C::Token
        | C::Daemon
        | C::Tui
        | C::Init
        | C::Relink
        | C::Purge => Operator,
        C::HelpFlag
        | C::VersionFlag
        | C::Help
        | C::Describe
        | C::New
        | C::List
        | C::Next
        | C::Claim
        | C::Unclaim
        | C::Reset
        | C::Summary
        | C::Report
        | C::Search
        | C::Import
        | C::Decompose
        | C::Export
        | C::LoadContext
        | C::Phase
        | C::Epic
        | C::Handoff
        | C::Graph
        | C::CommitSync
        | C::LinkPr
        | C::UnlinkPr
        | C::Attachment
        | C::PrCheck
        | C::Show
        | C::Log
        | C::Comment
        | C::Move
        | C::Close
        | C::Block
        | C::Unblock
        | C::Prioritize
        | C::Label
        | C::Unlabel
        | C::Reopen
        | C::Archive
        | C::Unarchive
        | C::ArchiveState
        | C::Publish
        | C::Delete
        | C::Set
        | C::Relate
        | C::Unrelate => Task,
    }
}

fn descriptor(path: &CommandPath) -> Result<Descriptor, AppError> {
    let (effects, detail) = effects(path);
    let syntax = path.grammar.syntax;
    let confirmation = match path.words.as_slice() {
        ["delete"]
        | ["archive-state"]
        | ["project", "delete" | "set-prefix"]
        | ["daemon", "gc"] => Some(
            "Unforced operation returns a confirmation plan. Interactive plain mode may prompt; JSON/quiet/nonterminal callers must explicitly pass --force. No implicit consent.",
        ),
        _ => None,
    };
    let guarded_write = match path.command {
        C::Move => Some(
            "--if-state compares the stored state atomically; conflict exits 9 and reports expected/actual. No compare-and-swap option is implied for other writes.",
        ),
        C::Claim => Some(
            "Claim atomically requires readiness and a claimable state; --next selects and claims in the same transaction.",
        ),
        _ => None,
    };
    let noninteractive = match path.words.as_slice() {
        ["tui"] => "Requires an interactive terminal.",
        ["project", "new"] => {
            "With no local flags, prompts at a terminal and refuses JSON/nonterminal input. State --prefix and other desired options for noninteractive creation."
        }
        _ if confirmation.is_some() => {
            "Explicit --force is required when a confirmation cannot be prompted."
        }
        _ => {
            "Supported subject to existing runtime, input and authorization requirements; discovery grants no permission."
        }
    };
    Ok(Descriptor {
        path: path.words.clone(), aliases: path.aliases(),
        help_only_aliases: if path.words.len() == 1 { model::HELP_ALIASES.iter().filter(|alias| alias.command == path.command).map(|alias| alias.name).collect() } else { Vec::new() },
        entry_kind: path.grammar.kind, audience: audience(path), syntax,
        arguments: model::grammar::expression(syntax).map_err(AppError::Storage)?,
        grammar_notes: "Canonical forms. Optional/choice/repeated nodes express multiplicity. Legacy parsers retain option placement, duplicates, normalization, --/help precedence and additional cross-field constraints. Static domains may be validated by the service; dynamic sources are named, never fetched. Global --json, --quiet, --project, --store-path, --no-hooks and --deadline retain existing behavior; raw github delegates its own options.",
        help_topic: path.command.help_topic(), output: outputs(path),
        capabilities: Capabilities { effects: Effects { detail, ..effects },
            dry_run: syntax.contains("--dry-run").then_some("--dry-run previews domain/resource changes; normal project resolution and daemon startup may still occur. It is not a universal no-process or cancellation guarantee."),
            confirmation, guarded_write, noninteractive,
            uncertain_outcome: "A timeout/lost reply does not prove a write failed or was cancelled. Read/reconcile the target state and any operation receipt before retrying; never blindly replay an uncertain mutation." },
    })
}

fn effects(path: &CommandPath) -> (Effects, &'static str) {
    use Access::*;
    let sub = path.words.get(1).copied().unwrap_or("");
    let leaf = path.words.last().copied().unwrap_or("");
    let mut e = Effects {
        store: Read,
        filesystem: Read,
        processes: Read,
        remote: None,
        may_start_daemon: true,
        event_hooks_may_run: false,
        detail: "",
    };
    let detail = match path.command {
        C::HelpFlag | C::VersionFlag | C::Help | C::Describe => {
            e.store = None;
            e.filesystem = None;
            e.processes = None;
            e.may_start_daemon = false;
            "Compiled command/help data only."
        }
        C::Mcp | C::Init | C::Relink | C::Purge => {
            e.store = None;
            e.filesystem = None;
            e.processes = None;
            e.may_start_daemon = false;
            "Retired spelling: actionable refusal; no operation."
        }
        C::List
        | C::Next
        | C::Summary
        | C::Report
        | C::Search
        | C::Export
        | C::LoadContext
        | C::Handoff
        | C::Graph
        | C::Show
        | C::Log
        | C::SessionEligibility => {
            "Reads project/story data. Normal execution may start a daemon and create its runtime files."
        }
        C::New
        | C::Publish
        | C::Comment
        | C::Move
        | C::Block
        | C::Unblock
        | C::Prioritize
        | C::Label
        | C::Unlabel
        | C::Reopen
        | C::Set
        | C::Relate
        | C::Unrelate
        | C::LinkPr
        | C::UnlinkPr => {
            e.store = Write;
            e.event_hooks_may_run = true;
            "Writes story data or relationships; configured event hooks may have further filesystem, process or remote effects."
        }
        C::Close => {
            e.store = Write;
            e.event_hooks_may_run = true;
            "Retires work that will not be done, recording a reason and moving to the configured dropped/closed state; does not delete its history."
        }
        C::Archive | C::Unarchive | C::ArchiveState => {
            e.store = Write;
            e.event_hooks_may_run = true;
            "Changes archive visibility of closed stories, preserving their data/history. Archive-state operates on a closed column and requires confirmation."
        }
        C::Delete => {
            e.store = Write;
            e.event_hooks_may_run = true;
            "Permanently removes story data after confirmation. It is not archive, close, reset, or release of a claim."
        }
        C::Claim | C::Unclaim => {
            e.store = Conditional;
            e.event_hooks_may_run = true;
            "Claims ready work or returns a claim to its prior state (Todo fallback). Optional comments and dry-run apply; unclaim does not tear down the work lane."
        }
        C::Reset => {
            e.store = Conditional;
            e.filesystem = Conditional;
            e.processes = Conditional;
            e.event_hooks_may_run = true;
            "Reset removes owned workspace/session resources and returns the story to Todo. --dry-run previews; --force changes reset authorization and is not the generic confirmation mechanism."
        }
        C::Cleanup => {
            e.store = Conditional;
            e.filesystem = Conditional;
            e.processes = Conditional;
            "Reclaims closed-story owned resources and retries incomplete cleanup; --dry-run previews."
        }
        C::Resources => {
            "Inspects existing ownership/resource identities; supplied lease/location options affect inspection only."
        }
        C::Import | C::Decompose | C::ImportProject | C::Migrate => {
            e.store = Conditional;
            e.filesystem = Conditional;
            e.event_hooks_may_run = true;
            "Reads input and imports stories/projects; supported dry-run variants preview. Project import/migration may write checkout associations and files."
        }
        C::Project => {
            if !matches!(sub, "list" | "show")
                && !(sub == "settings" && matches!(leaf, "list" | "get"))
            {
                e.store = Write;
                e.filesystem = Conditional;
                e.event_hooks_may_run = true;
            }
            "Project lifecycle, Git associations and settings. Project link/unlink attaches origins/checkouts; story link/unlink is a relationship alias."
        }
        C::State | C::Type | C::Phase | C::Epic => {
            if !matches!(sub, "list" | "show") {
                e.store = Write;
                e.event_hooks_may_run = true;
            }
            "Reads or updates project definitions, ordering and story organization according to the selected subcommand."
        }
        C::DispatchPolicy => {
            if matches!(sub, "set" | "reset") {
                e.store = Write;
            }
            "Reads/resolves or updates dispatch model/effort inheritance; --global selects installation scope. Does not dispatch a provider."
        }
        C::Engine => {
            if sub != "status" && sub != "reset-check" {
                e.store = Write;
                e.filesystem = Conditional;
                e.processes = Conditional;
                e.remote = Conditional;
            }
            "Controls automatic dispatch; start/resume/adopt may launch providers and downstream remote work. Merely describing this command performs none of those actions."
        }
        C::Verifier if sub == "measure-gate-class" => {
            e.store = None;
            e.may_start_daemon = false;
            e.filesystem = Write;
            e.processes = Write;
            e.remote = Conditional;
            "Local measurement helper creates isolated checkout/output/probe state and executes the configured gate. The gate determines remote effects. It never certifies or merges."
        }
        C::Verifier => {
            if !matches!(sub, "status" | "evidence" | "gate-config")
                && !(matches!(sub, "repair" | "landing") && leaf == "show")
            {
                e.store = Write;
                e.filesystem = Conditional;
                e.processes = Conditional;
                e.remote = Conditional;
            }
            if sub == "gate-config" {
                e.store = None;
                e.may_start_daemon = false;
            }
            "Reads or controls verification/recovery. Activation may launch tests/providers and merge remote work. Landing release reads GitHub state and releases a local intent; it does not merge or certify a gate."
        }
        C::Continuation | C::Internal | C::SessionStart => {
            if path.command == C::SessionStart {
                e.filesystem = Conditional;
            }
            if !matches!(sub, "capabilities" | "status") {
                e.store = Conditional;
            }
            "Internal hook/session protocol; existing ownership, authorization and receipt checks still apply. SessionStart may publish unavailable-context diagnostics. No approval is granted by visibility."
        }
        C::Doctor => {
            if sub == "install" || matches!(sub, "abandoned" | "crashes") {
                e.store = None;
                e.may_start_daemon = false;
            }
            if sub.is_empty() {
                e.store = Conditional;
                e.filesystem = Conditional;
            }
            if leaf == "clear" {
                e.filesystem = Write;
            }
            "Diagnostics read state; --fix or explicit clear actions may write repairs/diagnostic files."
        }
        C::Update => {
            e.store = None;
            e.may_start_daemon = false;
            e.filesystem = Conditional;
            e.processes = Conditional;
            e.remote = Read;
            "Checks release metadata and may replace the installed binary; --check limits the operation to checking."
        }
        C::LaneBudget => {
            e.store = None;
            e.may_start_daemon = false;
            "Reads local process/tmux lane capacity and available verifier status; never starts a daemon for this query."
        }
        C::CommitSync => {
            e.store = Write;
            e.processes = Conditional;
            e.event_hooks_may_run = true;
            "Scans local Git commits and records references; this is not a push or remote synchronization."
        }
        C::Attachment => {
            if sub != "list" {
                e.filesystem = Write;
            }
            if matches!(sub, "add" | "remove") {
                e.store = Write;
                e.event_hooks_may_run = true;
            }
            "Adds/lists/removes stored attachments, or saves attachment bytes to a requested path."
        }
        C::PrCheck => {
            e.store = Conditional;
            e.remote = Read;
            e.processes = Conditional;
            "Reads linked pull-request state; requires the github-pr build feature and may apply configured close-on-merge behavior."
        }
        C::Hooks => {
            if sub != "list" {
                e.filesystem = Conditional;
                e.processes = Conditional;
                e.store = Conditional;
                e.remote = Conditional;
            }
            "Installs/removes hook files, lists them, or executes a configured hook for test. Hook execution may have arbitrary configured effects."
        }
        C::Scaffold => {
            "Renders scaffold text; writing it to a file is the caller's redirection. Project/store resolution may still occur."
        }
        C::Plugin => {
            e.filesystem = Conditional;
            e.processes = Conditional;
            e.store = Conditional;
            e.remote = Conditional;
            if sub == "run" {
                e.may_start_daemon = false;
            }
            "Installs/removes provider integration or delegates a helper. Delegated helpers own their runtime and may start their own daemon or perform remote work."
        }
        C::Store => {
            e.store = Write;
            e.filesystem = Write;
            if sub == "new" {
                e.may_start_daemon = false;
            }
            "Creates a specifically named store, or writes a backup of the ambient store. Store creation does not resolve/open the ambient store first."
        }
        C::Daemon | C::Web | C::Token => {
            e.store = None;
            e.may_start_daemon = matches!(sub, "start" | "restart" | "--serve");
            if !matches!(sub, "status" | "logs" | "token" | "list") {
                e.filesystem = Conditional;
                e.processes = Conditional;
            }
            if sub == "--serve" {
                e.store = Conditional;
            }
            "Local daemon, login-agent, browser, clipboard or token management. Foreground --serve opens/serves its store; status/log/token reads do not start a daemon."
        }
        C::Github => {
            e.store = None;
            e.may_start_daemon = false;
            e.processes = Conditional;
            e.remote = if sub == "merge" {
                Write
            } else if matches!(sub, "exec" | "git") {
                Conditional
            } else {
                Read
            };
            e.filesystem = Conditional;
            "Delegated Git/GitHub authority protocol. Resolve/observe reads identity; fetch may write local refs, merge writes remotely, and exec/git effects depend on arguments. It never opens the StoryHook store."
        }
        C::Tui => {
            e.store = Conditional;
            e.filesystem = Conditional;
            e.processes = Conditional;
            e.remote = Conditional;
            "Interactive terminal application; user actions can edit stories, control processes and invoke remote operations."
        }
    };
    if matches!(path.grammar.kind, FormKind::Group | FormKind::Retired) {
        e = Effects {
            store: None,
            filesystem: None,
            processes: None,
            remote: None,
            may_start_daemon: false,
            event_hooks_may_run: false,
            detail: "",
        };
    }
    (e, detail)
}

fn envelope() -> OutputContract {
    OutputContract {
        class: OutputClass::Envelope,
        selection: "--json selects JSON; otherwise the existing human rendering",
        schema: "result:'ok' plus the command-specific story, stories, message or report payload; see story help json-format",
        empty: "Command-specific empty payload; next preserves its legacy zero/one/many alternatives.",
        stdout: "Success result; --json errors also use stdout.",
        stderr: "Plain-mode errors and applicable diagnostic notices; JSON error rendering leaves stderr empty.",
        errors: "--json: result:'error', error and exit_code; state conflict: result:'conflict', expected and actual. Otherwise error-prefixed text on stderr.",
        exit_status: "0 success; AppError's documented nonzero exit code on failure; usage 2, state conflict 9, deadline 12.",
        quiet: "Suppresses successful ordinary rendered output, never errors.",
        follow: false,
    }
}
fn raw(schema: &'static str, selection: &'static str, quiet: &'static str) -> OutputContract {
    OutputContract {
        class: OutputClass::RawDocument,
        schema,
        selection,
        quiet,
        ..envelope()
    }
}
fn outputs(path: &CommandPath) -> Vec<OutputContract> {
    use OutputClass::*;
    if path.grammar.kind == FormKind::Group {
        return Vec::new();
    }
    if path.grammar.kind == FormKind::Retired {
        let mut o = envelope();
        o.schema = "No success value; actionable usage refusal.";
        return vec![o];
    }
    let sub = path.words.get(1).copied().unwrap_or("");
    match path.command {
        C::Describe => vec![raw(
            "storyhook.command-discovery schema_version:1; commands array",
            "Always JSON; --json is accepted",
            "Ignored for this raw discovery document.",
        )],
        C::Export => vec![raw(
            "Project export document accepted by import-project, including project definitions and stories",
            "Always JSON, independent of --json",
            "Ignored for raw export success.",
        )],
        C::LoadContext => vec![
            envelope(),
            raw(
                "Context document with project counts, readiness, verifier and requested story review context",
                "--format json; otherwise Markdown in the normal rendering",
                "Ignored when --format json produces a raw document.",
            ),
        ],
        C::Internal | C::Continuation | C::SessionEligibility => vec![raw(
            "Protocol-specific receipt/status object documented by this command's help topic; no universal envelope",
            "Always JSON",
            "Ignored for raw protocol success.",
        )],
        C::SessionStart => {
            let mut o = raw(
                "Hook context object, empty {} when there is nothing to report, or an unavailable-context annotation",
                "Always raw hook JSON",
                "Ignored for raw hook success.",
            );
            o.errors = "Context-dispatch failures become unavailable hook output plus a warning on stderr; pre-dispatch failures retain normal error handling.";
            o.exit_status = "Context failure fallback exits 0 with unavailable hook output and a warning; pre-dispatch errors retain their AppError status.";
            vec![o]
        }
        C::LaneBudget => vec![raw(
            "LaneBudgetView with capacity, counts, provider defaults and applicable verifier status",
            "JSON document in both output modes",
            "Suppresses success (this uses the ordinary renderer).",
        )],
        C::Verifier if sub == "measure-gate-class" => {
            let mut o = envelope();
            o.class = DelegatedHelper;
            o.selection = "Local collector stdout/stderr; --json does not wrap helper output";
            o.schema = "Progress lines; durable measurement reports live in the output directory";
            o.empty = "An early refusal may have no stdout.";
            o.quiet = "Does not suppress delegated streams.";
            o.errors = "Preparation errors use the normal contract; after exec the helper owns diagnostics and exit status.";
            o.exit_status = "Collector exit status; success is measurement completion, never production certification.";
            vec![o]
        }
        C::Verifier if matches!(sub, "repair-admit" | "gate-config" | "landing") => vec![raw(
            "Admission/configuration receipt, landing-intent array, or landing-release object according to this exact subcommand",
            "Always JSON",
            "Ignored for raw protocol success.",
        )],
        C::Daemon if sub == "logs" => {
            let mut o = envelope();
            o.class = JsonLines;
            o.selection = "--json produces one complete JSON object per successful log record; otherwise human log lines";
            o.schema = "Activity journal record per line";
            o.empty = "No records means empty stdout.";
            o.quiet = "Ignored by this local log reader.";
            o.follow = true;
            o.errors = "Terminal read failures use the normal error contract; a JSON error may be multi-line, so inspect exit status before treating all output as successful JSONL.";
            vec![o]
        }
        C::Github | C::Plugin if path.command == C::Github || sub == "run" => {
            let mut o = envelope();
            o.class = DelegatedHelper;
            o.selection = "Helper-owned stdout/stderr, without a universal JSON wrapper";
            o.schema = "Git/GitHub command or provider-helper-specific bytes";
            o.empty = "Helper-defined, including empty output.";
            o.quiet = "Does not suppress delegated streams.";
            o.stdout = "Delegated stdout bytes; successful GitHub protocol result bytes are returned directly.";
            o.stderr = "Delegated diagnostics, or local helper refusal.";
            o.errors = if path.command == C::Github {
                "Raw GitHub local refusals use plain stderr; delegated arguments do not select the StoryHook JSON envelope."
            } else {
                "Child streams/status are forwarded; launcher failures use the ordinary error contract."
            };
            o.exit_status = if path.command == C::Github {
                "0 on success; mapped AppError exit on Git/GitHub refusal/failure (not a promise to preserve gh's original status)."
            } else {
                "Child status is forwarded; signal-only termination maps to 1; launcher failures use AppError codes."
            };
            vec![o]
        }
        C::Tui => {
            let mut o = envelope();
            o.class = Terminal;
            o.selection = "Interactive terminal";
            o.schema = "Terminal screen updates";
            o.empty = "Terminal-owned.";
            o.stdout = "Terminal-owned screen/control sequences.";
            o.stderr = "Terminal errors.";
            o.quiet = "Does not suppress the terminal application.";
            o.errors = "Terminal error text, no JSON envelope.";
            vec![o]
        }
        C::Daemon | C::Web if sub == "--serve" => {
            let mut o = envelope();
            o.class = Terminal;
            o.selection = "Long-running foreground process";
            o.schema = "Runtime diagnostics, not a result document";
            o.stdout = "No one-result stdout contract.";
            o.stderr = "Startup/runtime diagnostics.";
            o.quiet = "Does not suppress runtime diagnostics.";
            o.errors = "Foreground startup errors are plain diagnostics with AppError status.";
            vec![o]
        }
        C::Next => {
            let mut o = envelope();
            o.schema = "Legacy next: zero gives result:'ok' and message; nonempty with requested --count 1 gives story; requested --count above 1 gives stories even if only one is available.";
            vec![o]
        }
        _ => vec![envelope()],
    }
}
