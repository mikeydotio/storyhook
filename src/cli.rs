/// Offline command discovery.
pub mod discovery;
/// Automatic dispatch policy commands.
pub mod dispatch_policy;
/// Shared command registration and grammar.
pub mod model;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{normalize_labels, validate_dispatch_option_token};
use crate::error::AppError;
use crate::service::engine::MAX_ENGINE_LANES;
use crate::store::{EngineAgent, EngineSpeed};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HooksAction {
    Install,
    Uninstall,
    List,
    Test { event_type: String },
}

mod continuation;
pub use continuation::ContinuationAction;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphMode {
    Overview,
    CriticalPath,
    BlockedBy(String),
    ParallelGroups,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseAction {
    List,
    Show {
        phase: String,
    },
    Add {
        id: String,
        phase: String,
    },
    Remove {
        id: String,
    },
    Create {
        phase: String,
        title: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeAction {
    List,
    Add {
        slug: String,
        description: Option<String>,
        emoji: Option<String>,
    },
    Set {
        slug: String,
        description: Option<String>,
        /// `--no-description`, which clears rather than sets.
        clear_description: bool,
        emoji: Option<String>,
        /// `--no-emoji`, which clears rather than sets.
        clear_emoji: bool,
    },
    Remove {
        slug: String,
    },
}

/// The `story state …` subcommands, grouped the same way [`TypeAction`]
/// groups `story type …`.
///
/// Values stay as the raw strings the user typed; `app::run` parses and
/// validates them, like every other invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateAction {
    List,
    Add {
        slug: String,
        superstate: String,
        role: Option<String>,
        description: Option<String>,
    },
    Set {
        slug: String,
        superstate: Option<String>,
        /// `--role active` sets the role, `--role none` clears it, absent
        /// leaves it alone. `none` is unambiguous because `active` is the
        /// only role the tool recognizes.
        role: Option<String>,
        description: Option<String>,
        /// `--no-description`, which clears rather than sets.
        clear_description: bool,
        /// `--move-stories-to <slug>`: where open stories go when this edit
        /// reclassifies the state they are sitting in.
        move_stories_to: Option<String>,
    },
    Remove {
        slug: String,
        move_stories_to: Option<String>,
    },
    Reorder {
        order: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpicAction {
    List,
    Show { id: String },
    Create { title: String },
    Add { epic_id: String, story_id: String },
}

/// Controls under `story engine`.
///
/// Run ids are opaque engine identities. Epic and adoption selectors are story ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineAction {
    /// Refuse dispatch of a story owned by an unfinished reset.
    ResetCheck {
        /// Story whose resource ownership is being checked.
        story: String,
    },
    /// Read one current reset reservation; internal cleanup-helper protocol.
    ResetTarget {
        /// Exact run owning the reservation.
        run: String,
        /// Exact operation identity, never a request to create a reset.
        token: String,
    },
    /// Bind manually dispatched stories to existing run capacity.
    Adopt {
        /// Current run when omitted.
        run: Option<String>,
        /// Story selectors, canonicalized before dispatch.
        ids: Vec<String>,
    },
    /// Patch the future-dispatch settings of a live run.
    Configure {
        /// Explicit run selector, or the current live run.
        run: Option<String>,
        /// Only settings explicitly supplied by the caller.
        patch: crate::service::engine::ConfigurePatch,
    },
    Start {
        /// Optional epic subtree; absent means the whole project.
        epic: Option<String>,
        /// Number of homogeneous lanes to operate.
        lanes: u32,
        /// Provider used by every lane.
        agent: EngineAgent,
        /// Optional provider model selection.
        model: Option<String>,
        /// Optional provider reasoning-effort selection.
        effort: Option<String>,
        /// Optional provider speed selection.
        speed: Option<EngineSpeed>,
    },
    Status {
        run: Option<String>,
    },
    Pause {
        run: Option<String>,
    },
    Resume {
        run: Option<String>,
    },
    Stop {
        run: Option<String>,
        now: bool,
        #[serde(
            default,
            skip_serializing_if = "crate::service::reset::ResetCaller::is_empty"
        )]
        caller: crate::service::reset::ResetCaller,
    },
    Ack {
        run: Option<String>,
    },
}

/// The controls under `story verifier` (SH-666).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifierAction {
    /// Inspect the selected project's pending landing authority.
    LandingShow,
    /// Release an exact pending intent (and every batch member), with a reason.
    LandingRelease {
        /// Exact intent id from landing show.
        intent_id: String,
        /// Operator reason retained on every affected story.
        reason: String,
    },
    /// Read durable submission and gate-cost history without changing admission.
    Evidence {
        /// Story whose own and shared batch executions are requested.
        story_id: String,
    },
    /// Private subprocess callback bound to the daemon's live verification owner.
    RepairAdmit {
        /// Exact story owned by the verifier.
        story_id: String,
        /// Unique live verifier attempt token.
        attempt_id: String,
        /// Exact submission generation; zero and negative values are invalid.
        generation: i64,
        /// Immutable proposed Git input, without any certification authority.
        input: crate::service::project_recovery::RepairInput,
    },
    /// Read one durable project fault recovery in the selected project.
    RepairShow {
        /// Exact stable recovery identity.
        recovery_id: String,
    },
    /// Accept a versioned scope decision under managed assessment authority.
    RepairDecide {
        /// Exact stable recovery identity.
        recovery_id: String,
        /// JSON file resolved against the caller's working directory.
        input: String,
    },
    /// Accept an operator's statement that an External recovery's
    /// prerequisite is satisfied (SH-849); a dispatched agent is refused.
    RepairSatisfy {
        /// Exact stable recovery identity.
        recovery_id: String,
        /// JSON file resolved against the caller's working directory.
        input: String,
    },
    /// Inspect gate configuration in a pinned proposed merge without the store.
    GateConfig {
        /// Repository containing the pinned parents.
        checkout: std::path::PathBuf,
        /// Pinned base commit.
        base: String,
        /// Pinned proposed head commit.
        head: String,
        /// Expected proposed merge tree.
        tree: String,
    },
    /// Local, store-free verifier scheduling experiment.
    MeasureGateClass {
        /// Clean source checkout.
        checkout: std::path::PathBuf,
        /// Pinned commit.
        commit: String,
        /// Private retained output directory.
        output: std::path::PathBuf,
    },
    /// Read durable permission, incidents, recovery and live ownership.
    Status,
    /// Enable admission without clearing a halt.
    Start,
    /// Disable admission and cancel owned work.
    Stop,
    /// Disable admission and finish owned work.
    Drain,
    /// Clear an exact incident and keep admission disabled.
    AckLeaveStopped {
        /// Exact current incident identity.
        incident_id: String,
    },
    /// Acknowledge one exact halted infrastructure incident so the verifier
    /// queue may run again. The id is the one the halt comment prints.
    Ack {
        /// The incident to acknowledge, verbatim.
        incident_id: String,
    },
}

pub use model::HELP_TEXT;

/// Which story `story claim` (SH-476) is about.
///
/// An enum rather than an `Option<String>` beside a `bool`, so the two forms
/// cannot both be set and cannot both be absent. `--phase` lives inside
/// [`Next`](Self::Next) for the same reason: `story claim <id> --phase 1` has
/// nowhere to put the phase, so the parser refuses it at the one point it
/// could have been written and no later layer ever meets the combination.
///
/// This is the better version of the rule SH-344's claiming mode had to state
/// as a refusal: bolted onto `story next`, it needed a guard rejecting
/// `--count` other than 1, where a dedicated verb makes the impossible
/// combination unrepresentable rather than merely rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimTarget {
    /// `story claim <id>` — that named story, claimed atomically.
    Story(String),
    /// `story claim --next [--phase <N>] [--epic <id>]
    /// [--exclude-label <csv>]` — whatever `story next` would answer,
    /// selected and claimed inside one write transaction.
    Next {
        /// `--phase <N>` — narrow the selection to one phase label,
        /// exactly as `story next --phase` narrows the same query.
        phase: Option<String>,
        /// `--epic <id>` — narrow selection to the epic's descendant subtree.
        epic: Option<String>,
        /// `--exclude-label <csv>` — omit stories carrying any named label.
        exclude_label: Option<String>,
    },
}

/// What `story claim` posts alongside the claim (SH-476).
///
/// Three states, not `Option<String>`, because "the caller said nothing" and
/// "the caller said not to" are different instructions and only one of them
/// can be answered by composing text. The comment is **not** opt-in: a claim
/// comments by default, `--comment <text>` replaces that text, and
/// `--no-comment` suppresses it.
///
/// # Why [`Default`](Self::Default) is resolved client-side
///
/// The default sentence names the *caller's* host and tmux window, and the
/// daemon is in neither: `story` parses locally and sends an
/// [`crate::invoke::InvokeRequest`] over `/api/v1/invoke`, `$TMUX` is
/// per-process, and one daemon serves every client of its store. So
/// `Default` is resolved by [`crate::claim_comment::resolve`] after parsing
/// and before the request is built — the same place, and for the same
/// reason, that `$STORYHOOK_ACTOR`, the piped stdin and the GitHub
/// credential are read.
///
/// Resolving it in the parser instead was not an option: `parse_invocation`
/// is pure, which is what lets `tests/trailing_arguments.rs` provoke every
/// verb in the grammar — `story daemon install` included — with no side
/// effects at all.
///
/// A `Default` that still reaches the daemon is *refused*, never filled in
/// there. See [`crate::claim_comment::UNRESOLVED_REFUSAL`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimComment {
    /// Neither flag given: the client composes the sentence.
    Default,
    /// `--comment <text>` — this text instead of the default one.
    Custom(String),
    /// `--no-comment` — no comment at all.
    Suppressed,
}

/// What `story unclaim` posts alongside the release (SH-483).
///
/// The same three shapes as [`ClaimComment`], and deliberately **not** the
/// same type, because the two disagree about the one thing that matters:
/// where [`Default`](Self::Default) is resolved.
///
/// A claim's default sentence names the *caller's* host and tmux window, so
/// only the client can compose it and a `Default` reaching the daemon is
/// refused ([`crate::claim_comment::UNRESOLVED_REFUSAL`]). An unclaim's
/// default names the state the story is being restored to and whether that
/// was the state it was claimed from or the `todo` fallback — two facts that
/// do not exist until the write transaction is already open, and that the
/// client structurally cannot know. So this `Default` travels to the store
/// on purpose and is composed there.
///
/// Collapsing the two onto one enum would make each one's promise about
/// `Default` unstatable, since they are opposite promises. They share the
/// shape and never the contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnclaimComment {
    /// Neither flag given: the store composes the sentence.
    Default,
    /// `--comment <text>` — this text instead of the default one.
    ///
    /// The caller's own sentence, never edited. A fallback restoration is
    /// still reported in the *result*; it is not spliced into text somebody
    /// else wrote.
    Custom(String),
    /// `--no-comment` — no comment at all.
    Suppressed,
}

/// Every command storyhook can execute, fully parsed and validated for
/// *shape* — field values stay as the raw strings the user typed, because
/// interpreting them needs project data the parser cannot see.
///
/// This is the request half of the wire envelope (the response half is
/// [`crate::output::Response`]): it is what a client sends and a server
/// executes, so it must stay serializable and free of anything
/// process-local. Every field is currently a `String`, `bool`, `usize`,
/// `u16`, `PathBuf` or a collection of those.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Invocation {
    /// Durable autonomous context and administrative handoffs.
    Continuation {
        /// Canonical target story, empty only for capabilities.
        id: String,
        /// Lifecycle operation.
        action: ContinuationAction,
    },
    Help,
    Project {
        action: ProjectAction,
    },
    /// Installation or project automatic dispatch settings.
    DispatchPolicy {
        /// Select installation scope instead of the current project.
        global: bool,
        /// Read, preview, set, or reset policy.
        action: dispatch_policy::PolicyAction,
    },
    New {
        title: String,
        state: Option<String>,
        story_type: Option<String>,
        description: Option<String>,
        priority: Option<String>,
        /// Explicit story complexity.
        complexity: Option<String>,
        labels: Option<Vec<String>>,
        /// Creates the story as a draft (SH-175) — `story new --draft`.
        draft: bool,
        /// Stories this one is blocked by — `story new --blocked-by <id>`,
        /// repeatable (SH-779). Recorded in the creation transaction, so the
        /// story is never ready before its blockers are.
        #[serde(default)]
        blocked_by: Vec<String>,
    },
    /// Makes a draft story live — `story publish <id>` (SH-175). One-way;
    /// idempotent on a story that is already live.
    Publish {
        id: String,
    },
    State {
        action: StateAction,
    },
    List {
        state: Option<String>,
        flagged: bool,
        priority: Option<String>,
        label: Option<String>,
        created_after: Option<String>,
        updated_after: Option<String>,
        blocked: bool,
        ready: bool,
        stale: Option<String>,
        phase: Option<String>,
        story_type: Option<String>,
        /// Narrows to drafts only (SH-175) — mirrors `--flagged`/`--blocked`'s
        /// narrow-only semantics; drafts are otherwise shown inline.
        drafts: bool,
        /// Narrows to stories nobody has assessed (SH-359). Distinct from
        /// `--priority none`, which also matches every story parked there on
        /// purpose.
        unassessed: bool,
        /// `--include-closed` (SH-409): widens the default OPEN-only
        /// visibility to also show closed, unarchived stories.
        include_closed: bool,
        /// `--include-archived` (SH-409): widens the default visibility to
        /// also show archived (`story hide`d) stories. Implies
        /// `include_closed`.
        include_archived: bool,
    },
    Search {
        query: String,
    },
    /// `story next [--count <n>] [--phase <N>] [--epic <id>]
    /// [--exclude-label <csv>]` — a pure read (SH-477, SH-455).
    ///
    /// Answers a question and writes nothing. Taking the answer is
    /// [`Claim`](Self::Claim)'s job: SH-344's claiming mode lived here as a
    /// `--claim` flag until SH-477 removed it, because two spellings for one
    /// atomic operation is drift, and only the dedicated verb can honor a
    /// caller-named id as well as `--next`.
    Next {
        count: usize,
        phase: Option<String>,
        epic: Option<String>,
        exclude_label: Option<String>,
    },
    /// `story claim (<id> | --next)` (SH-476) — the one atomic claim verb.
    ///
    /// One of the two forms is required and they are mutually exclusive, which
    /// [`ClaimTarget`] makes unrepresentable rather than merely refused: a
    /// bare `story claim` has no `ClaimTarget` to build and is answered with
    /// the usage line. That is deliberate for a *mutating* verb — a script
    /// that dropped its id argument must never silently claim whatever
    /// happened to sort first.
    Claim {
        /// Which story: a named one, or whatever `story next` would answer.
        target: ClaimTarget,
        /// What to post alongside the claim.
        comment: ClaimComment,
        /// `--dry-run`: read for real, write symbolically.
        dry_run: bool,
    },
    /// `story unclaim <id>` (SH-483) — the inverse of [`Claim`](Self::Claim),
    /// and the store half of it: the state change and its comment, never a
    /// tmux window and never a worktree, which belong to
    /// `plugins/story/bin/story.sh` and are SH-484's.
    ///
    /// No `--next` form and no pseudo-target enum. "Whichever story I happen
    /// to be holding" is not a question the store can answer — a store serves
    /// every client at once and nothing records which of them claimed what —
    /// so this names its story, always.
    Unclaim {
        /// The story to release.
        id: String,
        /// What to post alongside the release.
        comment: UnclaimComment,
        /// `--dry-run`: read for real, write symbolically.
        dry_run: bool,
    },
    /// Removes owned worktree resources and returns an open ordinary story to Todo.
    Reset {
        /// Terminal identity captured by the client.
        #[serde(default)]
        caller: crate::service::reset::ResetCaller,
        /// Canonical or project-relative story identifier.
        id: String,
        /// Compatibility flag; reset always discards owned local work.
        force: bool,
    },
    /// Read-only reset preview. A separate wire variant ensures an older daemon
    /// rejects this request rather than ignoring a flag and executing a reset.
    ResetPreview {
        /// Canonical or project-relative story identifier.
        id: String,
        /// Terminal identity captured by the client.
        caller: crate::service::reset::ResetCaller,
    },
    /// Revokes unattempted terminal effects before a managed session replacement.
    /// The caller holds workspace exclusion through the replacement itself.
    SupersedeBlockDeliveries {
        /// Canonical or project-relative story identifier.
        id: String,
    },
    /// Supersedes the context-handoff chain a manual resume replaces (SH-850).
    /// The caller holds workspace exclusion through the replacement launch.
    SupersedeContinuations {
        /// Canonical or project-relative story identifier.
        id: String,
    },
    /// `story engine start|status|pause|resume|stop|ack` (SH-467).
    Engine {
        action: EngineAction,
    },
    /// Project verifier status and operator controls (SH-703).
    Verifier {
        action: VerifierAction,
    },
    /// Read-only identity inventory for one story's existing resources.
    Resources {
        /// Canonical or abbreviated story identifier.
        id: String,
        /// Explicit evidence and additional legacy discovery hints.
        options: crate::service::resources::ResourceOptions,
    },
    /// `story cleanup [--dry-run]` — safely reclaim StoryHook-owned workspaces.
    Cleanup {
        /// Preview eligible removals without changing Git or the filesystem.
        dry_run: bool,
    },
    Summary,
    Report {
        html: bool,
    },
    Doctor {
        fix: bool,
    },
    /// `story doctor abandoned [clear (--all | <request-id>)]` — the ledger
    /// of commands `story daemon stop --force` or a crashed daemon's own
    /// successor abandoned mid-flight. Separate from `Doctor` because it
    /// needs neither a project nor a store: it reads and writes one file
    /// under the daemon's own state directory.
    /// `story doctor install` — what is installed on this machine, and how far
    /// the checkout has run ahead of it (SH-530).
    ///
    /// Store-free on purpose. The single most important thing this can report
    /// is that the store will not open, or opens read-only, so a verb that
    /// needed the store first could never deliver its own headline.
    DoctorInstall,
    /// `story lane-budget` — an informational census of live agent
    /// windows (SH-672). Measures the caller's own tmux server and adds verifier
    /// notices from an existing daemon without starting one or opening a store.
    LaneBudget,
    DoctorAbandoned {
        action: AbandonedAction,
    },
    /// `story doctor crashes [clear (--all | <crash-id>)]` — the ledger of
    /// crashes a daemon's successor noticed on relaunch, and what became of
    /// each: filed as a bug, folded into one already filed, or withheld with
    /// a reason (SH-287). The same shape as [`Self::DoctorAbandoned`] and for
    /// the same reason: neither a project nor a store, one file under the
    /// daemon's own state directory.
    DoctorCrashes {
        action: CrashesAction,
    },
    Show {
        id: String,
    },
    /// One story's write history: what happened to it, when, and what wrote it
    /// (SH-246).
    ///
    /// **Named `log`, not `history`, and that is deliberate.** [`History`] below
    /// already exists as the TUI's undo primitive and is unreachable from the
    /// command line on purpose. The two answer different questions — one is a
    /// raw log to put *back*, this is a rendering to *read* — and giving them
    /// the same word would leave two "history" concepts in one codebase for
    /// every future reader to disentangle. `log` is also what every neighbouring
    /// tool calls an append-only trail: `git log`, `journalctl`, `docker logs`.
    ///
    /// A verb rather than a flag on [`Show`](Self::Show) because this parser
    /// reserves verbs for a distinct data *shape* and flags for narrowing an
    /// existing view — and `show` takes no flags at all today.
    Log {
        id: String,
    },
    Comment {
        id: String,
        text: String,
    },
    SetState {
        id: String,
        state: String,
        comment: Option<String>,
        if_state: Option<String>,
        /// An `awaiting` reason to set atomically with the state change
        /// (SH-205) — `story move <id> blocked --reason "<text>"`. Strictly
        /// opt-in: `None` on every unmodified caller (scripts, agents, CI),
        /// so the bare transition's non-interactive contract is unchanged.
        awaiting: Option<String>,
    },
    SetAwaiting {
        id: String,
        /// `None` only when `on` is non-empty — `story block <id> --on
        /// <blocker>` needs no prose at all. Every other caller (REST, TUI,
        /// and `story block <id> "<reason>"` with no `--on`) still sets
        /// this and leaves `on` empty, unchanged from before SH-398.
        awaiting: Option<String>,
        /// Stories to record as `blocked-by` edges, atomically with
        /// `awaiting` — SH-398's `story block <id> --on <blocker> ...`.
        /// Empty for every caller that predates it.
        on: Vec<String>,
    },
    ClearAwaiting {
        id: String,
        /// `blocked-by` edges to remove instead of clearing `awaiting` —
        /// SH-398's `story unblock <id> --on <blocker> ...`. Empty means the
        /// pre-existing behaviour: clear the prose reason.
        on: Vec<String>,
    },
    SetPriority {
        id: String,
        priority: String,
    },
    SetLabels {
        id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    Reopen {
        id: String,
    },
    /// Hides a closed story from the primary UI — the "Archive" action
    /// (SH-43). Refuses an open story; reversed by [`Unhide`](Self::Unhide).
    Hide {
        id: String,
    },
    /// The inverse of [`Hide`](Self::Hide) — "Unarchive".
    Unhide {
        id: String,
    },
    /// Archives every story in a CLOSED-superstate column — the dashboard's
    /// bulk "Archive" button, and its CLI equivalent (SH-43). An unforced call
    /// answers with what it would hide and writes nothing.
    HideState {
        state: String,
        force: bool,
    },
    Delete {
        id: String,
        /// Whether the confirmation has already been given. An unforced delete
        /// answers with `Response::ConfirmationRequired` and writes nothing.
        force: bool,
    },
    BulkUpdate {
        updates: Vec<(String, String)>,
    },
    Import {
        file: Option<String>,
    },
    Decompose {
        file: Option<String>,
        stdin: bool,
        dry_run: bool,
    },
    Export,
    ImportProject {
        file: String,
        /// The operator's assertion that `file` predates event kind #18, so
        /// its `[git] <sha>: <subject>` comments are legacy link records
        /// rather than prose a user typed (SH-70). `false` leaves them as
        /// prose, unchanged from before this field existed.
        legacy_links: bool,
    },
    /// Move a legacy `.storyhook` tree into the store.
    ///
    /// Additive and one-way: it reads the tree, never writes to it, and refuses
    /// to run twice against one checkout. The legacy directory is left exactly
    /// as it was, because it is the operator's rollback.
    Migrate {
        /// The checkout holding `.storyhook`. `None` walks up from the working
        /// directory, so the command works from anywhere inside the repository.
        path: Option<String>,
        /// Report what would be imported and write nothing.
        dry_run: bool,
    },
    /// Read the tracker facts authorizing an existing autonomous session to continue.
    SessionEligibility {
        /// The story whose active, unblocked state is being checked.
        id: String,
    },
    Context {
        /// Output format; omission preserves the ordinary Markdown briefing.
        format: Option<String>,
        /// Include complete obviation-review evidence relative to this story.
        story: Option<String>,
    },
    Handoff {
        since: Option<String>,
    },
    Phase {
        action: PhaseAction,
    },
    Type {
        action: TypeAction,
    },
    Epic {
        action: EpicAction,
    },
    Graph {
        mode: GraphMode,
    },
    SetFields {
        id: String,
        title: Option<String>,
        state: Option<String>,
        priority: Option<String>,
        /// Explicit story complexity.
        complexity: Option<String>,
        labels: Option<String>,
        blocked: Option<String>,
        unblocked: bool,
        json: Option<String>,
        story_type: Option<String>,
        description: Option<String>,
    },
    Relate {
        a: String,
        relation: String,
        b: String,
        remove: bool,
    },
    Hooks {
        action: HooksAction,
    },
    Scaffold {
        kind: String,
    },
    CommitSync {
        since: Option<String>,
    },
    /// `story link-pr <id> <url> [--no-close-on-merge]` — links a GitHub pull
    /// request to a story (SH-49). Never touches GitHub: parsing a URL and
    /// recording a link needs no network access, so this arm runs in every
    /// build.
    LinkPr {
        id: String,
        url: String,
        /// Whether merging this pull request should close the story.
        /// Defaults to `true`; `--no-close-on-merge` clears it.
        close_on_merge: bool,
    },
    /// `story unlink-pr <id> <url>` — the inverse of [`LinkPr`](Self::LinkPr).
    UnlinkPr {
        id: String,
        url: String,
    },
    /// `story pr-check [<id>]` — asks GitHub about every (or, with an id, one
    /// story's) open linked pull request, closing a story whose merged link
    /// asked to be closed on merge. Feature-gated: this is the one PR-link
    /// operation that spends a GitHub credential.
    PrCheck {
        id: Option<String>,
    },
    HelpTopic {
        topic: String,
    },
    HelpCompact,
    HelpAll,
    Plugin {
        action: PluginAction,
    },
    Web {
        action: WebAction,
    },
    /// `story token new|list|revoke` (SH-255) — the named, persistent,
    /// revocable credential that authenticates the dashboard. Process
    /// management, like [`Self::Daemon`] and [`Self::Web`]: a token record
    /// lives in the daemon's own state directory, not in the store, so this
    /// needs a running daemon rather than a project.
    Token {
        action: TokenAction,
    },
    /// The storyhook daemon: the one process that owns the store and serves
    /// everything that talks to it.
    Daemon {
        action: DaemonAction,
    },
    Store {
        action: StoreAction,
    },
    SessionStart,
    Update {
        check: bool,
        force: bool,
        #[serde(default)]
        source: Option<String>,
    },
    Version,
    /// Everything a long-lived client needs to render a project, in one
    /// round trip.
    ///
    /// Not reachable from the command line, and deliberately so: a shell user
    /// asking for the whole project already has `story list` and `story
    /// export`. This exists for clients that hold a *model* — the TUI, which
    /// rebuilds its board after every change, and the dashboard's resync after
    /// a dropped event stream. Without it those clients issue four or five
    /// reads per refresh and see a different instant in each one.
    ProjectSnapshot,
    /// Read or replace one story's raw event history.
    ///
    /// The TUI's undo primitive, and nothing else's: undo snapshots the exact
    /// log a story had before a mutation and puts it back afterwards, which is
    /// neither an append nor a compensating edit. It is here rather than in
    /// the TUI because the seam has to be the only way a client reaches
    /// project data — a client that keeps one filesystem call keeps all of
    /// them.
    ///
    /// Not reachable from the command line either. Reading a raw history is
    /// what `story export` is for, and rewriting one is not something a user
    /// should be able to ask for by accident.
    History {
        action: HistoryAction,
    },
    /// `story attachment add|list|remove|save` — image attachments on a story
    /// (SH-315).
    Attachment {
        action: AttachmentAction,
    },
}

impl Invocation {
    /// The same invocation, with its confirmation already given.
    ///
    /// The second half of the two-step a destructive command runs: the first
    /// invocation answers `Response::ConfirmationRequired` and writes nothing,
    /// the client asks the user, and this is what it sends back. The
    /// invocation is otherwise untouched — the *same* target, resolved the
    /// same way — so the thing that gets destroyed is the thing that was
    /// described.
    ///
    /// An invocation with nothing to confirm is returned unchanged, which is
    /// what makes this safe to call unconditionally.
    ///
    /// # Why every arm is exhaustive
    ///
    /// The `Project` arm used to be `ProjectAction::Deinit { force, .. }`
    /// beside a `_ => {}`, and a destructive project verb added later would
    /// have fallen through it silently — the client would ask the user, get a
    /// yes, re-send an invocation that is still unforced, and be answered with
    /// the same question forever. A confirmation loop with no error and no
    /// compile failure. The top level then kept exactly that wildcard, and
    /// `HideState` fell through it for as long as it existed (SH-638). Listing every variant of every
    /// level means the next one is a compile error here instead.
    ///
    /// `DaemonAction::Stop { force }` is deliberately **not** set: that flag
    /// means "signal the process", not "skip a confirmation", and `stop`
    /// never asks one.
    #[must_use]
    pub fn forced(mut self) -> Self {
        match &mut self {
            Self::DispatchPolicy { .. } => {}
            Self::Project { action } => match action {
                ProjectAction::Delete { force } => *force = true,
                ProjectAction::SetPrefix { force, .. } => *force = true,
                ProjectAction::New(_)
                | ProjectAction::List
                | ProjectAction::Show
                | ProjectAction::Link(_)
                | ProjectAction::Unlink(_)
                | ProjectAction::Settings(_) => {}
            },
            Self::Delete { force, .. } => *force = true,
            Self::Reset { .. } | Self::ResetPreview { .. } => {}
            // Answers `ConfirmationRequired` too, and until SH-638 was never
            // forced on the re-run: `story archive-state` at a terminal
            // printed its plan twice and archived nothing.
            Self::HideState { force, .. } => *force = true,
            Self::Daemon { action } => match action {
                DaemonAction::Logs { .. }
                | DaemonAction::Serve { .. }
                | DaemonAction::Start { .. }
                | DaemonAction::Restart
                | DaemonAction::Stop { .. }
                | DaemonAction::Status
                | DaemonAction::Install { .. }
                | DaemonAction::Uninstall
                | DaemonAction::Token => {}
                DaemonAction::Gc { force } => *force = true,
            },
            Self::Help
            | Self::New { .. }
            | Self::Publish { .. }
            | Self::State { .. }
            | Self::List { .. }
            | Self::Search { .. }
            | Self::Next { .. }
            | Self::Claim { .. }
            | Self::Unclaim { .. }
            | Self::SupersedeBlockDeliveries { .. }
            | Self::SupersedeContinuations { .. }
            | Self::Engine { .. }
            | Self::Verifier { .. }
            | Self::Cleanup { .. }
            | Self::Resources { .. }
            | Self::Summary
            | Self::Report { .. }
            | Self::Doctor { .. }
            | Self::DoctorInstall
            | Self::LaneBudget
            | Self::DoctorAbandoned { .. }
            | Self::DoctorCrashes { .. }
            | Self::Show { .. }
            | Self::Log { .. }
            | Self::Comment { .. }
            | Self::SetState { .. }
            | Self::SetAwaiting { .. }
            | Self::ClearAwaiting { .. }
            | Self::SetPriority { .. }
            | Self::SetLabels { .. }
            | Self::Reopen { .. }
            | Self::Hide { .. }
            | Self::Unhide { .. }
            | Self::BulkUpdate { .. }
            | Self::Import { .. }
            | Self::Decompose { .. }
            | Self::Export
            | Self::ImportProject { .. }
            | Self::Migrate { .. }
            | Self::Continuation { .. }
            | Self::SessionEligibility { .. }
            | Self::Context { .. }
            | Self::Handoff { .. }
            | Self::Phase { .. }
            | Self::Type { .. }
            | Self::Epic { .. }
            | Self::Graph { .. }
            | Self::SetFields { .. }
            | Self::Relate { .. }
            | Self::Hooks { .. }
            | Self::Scaffold { .. }
            | Self::CommitSync { .. }
            | Self::LinkPr { .. }
            | Self::UnlinkPr { .. }
            | Self::PrCheck { .. }
            | Self::HelpTopic { .. }
            | Self::HelpCompact
            | Self::HelpAll
            | Self::Plugin { .. }
            | Self::Web { .. }
            | Self::Token { .. }
            | Self::Store { .. }
            | Self::SessionStart { .. }
            | Self::Update { .. }
            | Self::Version
            | Self::ProjectSnapshot { .. }
            | Self::History { .. }
            | Self::Attachment { .. } => {}
        }
        self
    }
}

/// The four forms of `story attachment` (SH-315).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentAction {
    /// `story attachment add <id> <path> [--name <text>]`.
    ///
    /// `path` is resolved by the daemon against the request's own `cwd`, the
    /// same way `story import-project <file>` is (see `invoke::
    /// resolve_against`) — never against this process's own working
    /// directory, which may not be the caller's when this runs across the
    /// wire.
    Add {
        id: String,
        path: String,
        /// Defaults to `path`'s own file name when omitted.
        name: Option<String>,
    },
    /// `story attachment list <id>`.
    List { id: String },
    /// `story attachment remove <id> <n>`.
    Remove { id: String, attachment_id: u32 },
    /// `story attachment save <id> <n> <path>` — writes the attachment's
    /// bytes to `path`, resolved the same way `Add`'s `path` is.
    Save {
        id: String,
        attachment_id: u32,
        path: String,
    },
}

/// The two halves of the undo primitive: snapshot a history, and put one back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryAction {
    /// Every event of one story, oldest first. An unknown story has an empty
    /// history rather than being an error: a client asking "what was there
    /// before?" about a story that was not there gets the honest answer.
    Read { id: String },
    /// Replaces one story's history with `events`.
    ///
    /// An **empty** `events` means "this story should not exist" — undoing a
    /// creation. Anything else is a verbatim replacement.
    Restore {
        id: String,
        events: Vec<crate::domain::StoryEvent>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginAction {
    Install {
        target: String,
    },
    Uninstall {
        target: String,
    },
    /// Reinstall the plugin for every provider that has the storyhook
    /// marketplace registered, from this binary's embedded release (SH-667).
    /// Takes no target: the providers' own configurations say which.
    Reinstall,
    /// Run the installed provider plugin's deterministic helper through the
    /// stable `story` binary. The Codex integration's unversioned launcher is
    /// the intended caller; handling this in the client keeps the helper's
    /// stdout, stderr, exit status, cwd, and terminal environment intact.
    Run {
        target: String,
        args: Vec<String>,
    },
}

/// `story daemon …`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonAction {
    /// Read today's operational journal without contacting a daemon.
    Logs {
        /// Continue reading new entries across UTC midnight.
        follow: bool,
        /// Explicit project journal directory; omitted for the store journal.
        #[serde(default)]
        directory: Option<PathBuf>,
    },
    /// Run the daemon in this process, in the foreground. What the background
    /// spawner execs, and what a launchd agent runs.
    Serve {
        /// Bind this port instead of the environment's preferred one.
        port: Option<u16>,
        /// What started this process — `launchd`, `fork-test-build`, or
        /// `fork-no-agent` — self-reported into the portfile (SH-784).
        /// Internal wiring: `spawn_child` and the installed plist always
        /// pass it; a human typing `daemon --serve` by hand never does, and
        /// that absence is itself meaningful (`ForkReason::Manual`). Not
        /// documented in `--help` for the same reason `--serve` itself is
        /// not.
        #[serde(default)]
        owner: Option<String>,
    },
    /// Start a daemon in the background, if one is not already running.
    Start {
        /// Bind this port instead of the environment's preferred one.
        port: Option<u16>,
    },
    /// Gracefully replace the running daemon, preserving its loopback port.
    Restart,
    /// Ask the running daemon to shut down.
    Stop {
        /// After a short grace period, signal the daemon's pid directly
        /// rather than waiting for it to drain on its own. Abandons
        /// whatever it was still serving.
        force: bool,
    },
    /// Report whether one is running, and where.
    Status,
    /// Register a launchd agent so the daemon starts at login.
    Install {
        /// Register the running binary even when it is not the `story` this
        /// machine's `$PATH` resolves — `--this-binary`.
        ///
        /// The way through
        /// [`crate::daemon::install_guard::Refusal::Disagrees`] and
        /// [`Unconfirmable`](crate::daemon::install_guard::Refusal::Unconfirmable),
        /// and deliberately **not** a way through
        /// [`Root`](crate::daemon::install_guard::Refusal::Root): it answers
        /// which binary, never which user.
        this_binary: bool,
    },
    /// Remove that agent.
    Uninstall,
    /// Print the running daemon's bearer token (SH-50) — the value a caller
    /// puts in `X-Storyhook-Token` to reach `/api/v1/*` or the dashboard's
    /// dispatch endpoint from off-loopback.
    Token,
    /// Reclaim the runtime directories of stores that no longer exist
    /// (SH-638). Answers with what it would remove and asks, unless forced.
    Gc {
        /// Remove without asking — `--force`, or the confirmation given.
        force: bool,
    },
}

/// `story doctor abandoned …`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbandonedAction {
    /// List every entry in the ledger.
    List,
    /// Forget one entry (`Some(request_id)`) or every entry (`None`, from
    /// `--all`) — a human's confirmation that they reviewed it, not a claim
    /// about whether the abandoned work actually landed.
    Clear { request_id: Option<String> },
}

/// `story doctor crashes …`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashesAction {
    /// List every entry in the ledger.
    List,
    /// Forget one entry (`Some(crash_id)`) or every entry (`None`, from
    /// `--all`) — a human's confirmation that they reviewed it, not a claim
    /// about whether it was ever actually filed.
    Clear { crash_id: Option<String> },
}

/// The `story project …` subcommands — a repository's whole lifecycle.
///
/// A verb group rather than loose top-level verbs because all of these are
/// about the *project* rather than about a story, and because the two that
/// change something need the same thing: a way to name a checkout other than
/// "wherever you happen to be standing".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectAction {
    /// Create a project, optionally attaching a checkout to it.
    ///
    /// The verb that replaced `init`, and differs from it in three ways and no
    /// others: the checkout is named by `--attach PATH` rather than by a
    /// positional nobody could tell from a name, `--no-attach` makes the
    /// filesystem opt-out sayable at all, and a prefix is *required* rather
    /// than silently defaulted to `SH` (SH-109).
    New(NewProjectRequest),
    /// Destroy a project and everything recorded against it.
    ///
    /// **No target of its own.** Which project this is comes from the ordinary
    /// selector — the working directory, `--project <slug>` or
    /// `STORYHOOK_PROJECT` — so it is answered against a resolved
    /// [`Ctx`](crate::service::Ctx) like [`Settings`](Self::Settings),
    /// [`Link`](Self::Link) and [`Unlink`](Self::Unlink).
    ///
    /// `deinit` took a bare word and had to decide whether it was a path or a
    /// slug. That guess is a fourth way of naming a project beside SH-116's
    /// three, and it is the one that could silently name the wrong one: a slug
    /// that happens to match a directory name resolved as the directory.
    Delete {
        /// Authorize the destruction without being asked.
        force: bool,
    },
    /// Rename a project's story-id prefix, everywhere it is embedded — the
    /// SH-109 verb that makes a rename safe to do at all.
    ///
    /// **No target of its own**, for the same reason as [`Delete`](Self::Delete):
    /// the ordinary selector names the project, this names only the new
    /// prefix.
    ///
    /// Rewrites the project row and every relationship any of its stories
    /// claim (a story's own rendered `id` self-heals on refold; `other_id`
    /// does not, and is rewritten by real compensating events). Free-text
    /// description and comment bodies are deliberately left alone — there is
    /// no grammar in this codebase for a story-id reference inside prose, so
    /// rewriting one would be a guess dressed up as a fact.
    SetPrefix {
        /// The prefix every id renders under from this point on.
        new_prefix: String,
        /// Authorize the rewrite without being asked.
        force: bool,
    },
    /// Every project the store knows, checkout or no checkout.
    List,
    /// This project: which one the ordinary selector resolved, and where its
    /// repo-side work runs.
    ///
    /// **The scoped singular to [`List`](Self::List)'s plural**, and the only
    /// machine-readable answer to "which project am I?" — a question nothing
    /// else in the CLI could answer, because `list` enumerates *every* project
    /// and is dispatched without a [`Ctx`](crate::service::Ctx) at all.
    ///
    /// Spelled `show` rather than `current` deliberately: there is no
    /// current-project state and no default (`tests/project_selection.rs`), so
    /// a verb whose name asserted stored state would be misread forever. It
    /// mirrors `story show <id>`, which is the scoped singular for a story.
    ///
    /// **It reports; it never judges.** A project with no linked checkout
    /// answers with `checkout: null` and exit 0, because this is the command
    /// someone runs to find out *why* a dispatch refused, and a diagnostic that
    /// refuses in the state being diagnosed is useless. The refusal belongs to
    /// the consumer that actually needs a directory — `story.sh dispatch`.
    Show,
    /// Attach one of this project's two optional git associations.
    Link(LinkTarget),
    /// Detach one.
    Unlink(UnlinkTarget),
    /// Read and write this project's settings.
    ///
    /// One of the arms of this family that names a project rather than
    /// creating, destroying or enumerating them — so it is answered against a
    /// resolved [`Ctx`](crate::service::Ctx) rather than by
    /// `dispatch_unscoped`. [`Link`](Self::Link) and [`Unlink`](Self::Unlink)
    /// are the others.
    Settings(SettingsAction),
}

/// What `story project new` was told, or that it was told nothing at all.
///
/// **The absence of every verb-local switch is itself the request.** A bare
/// `story project new` is somebody asking to be walked through it; the same
/// command with any switch on it is a script that has already decided. Nothing
/// else distinguishes the two, which is what makes the rule predictable from
/// either side — and it is the rule `main.rs::confirm()` already applies, so
/// the program has one rule about prompting rather than two.
///
/// [`Ask`](Self::Ask) is resolved into [`Stated`](Self::Stated) by the client,
/// which is the only process with a terminal. It is nonetheless a wire variant
/// rather than a client-side placeholder, because a request carrying it *can*
/// reach the dispatcher — a caller that went round `main.rs`, a hand-built
/// `InvokeRequest`, a future front-end — and the dispatcher must refuse it
/// loudly. The alternative, quietly supplying defaults for what it could not
/// ask about, is SH-109's silent `SH` wearing a new verb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewProjectRequest {
    /// No verb-local switch was given: ask, or refuse.
    Ask,
    /// Everything the verb needs, stated on the command line.
    Stated(NewProjectSpec),
}

/// A fully specified `story project new`.
///
/// Every field the verb needs is here and none of them is a promise the caller
/// did not make: [`prefix`](Self::prefix) is a `String` rather than an
/// `Option`, so "created without anybody choosing a prefix" is not a state this
/// type can hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewProjectSpec {
    /// What, if anything, the project is attached to.
    pub attach: Attach,
    /// The story-id prefix, already through
    /// [`domain::prefix::validate`](crate::domain::prefix::validate).
    pub prefix: String,
    /// The project's display name; `None` takes the attach target's basename.
    pub name: Option<String>,
    /// Skip generating `AGENTS.md`.
    pub no_agents_md: bool,
}

/// The checkout `story project new` attaches, or the decision not to.
///
/// One enum rather than `Option<String>` beside a `no_attach: bool`, because
/// those two fields can disagree — `--attach ./here --no-attach` is
/// representable in that shape and meaningless. Here the parser refuses the
/// contradiction once and nothing downstream can meet it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attach {
    /// The directory the client ran in — what `--attach` defaults to, and what
    /// a bare `story project new` does.
    Cwd,
    /// The named directory.
    ///
    /// Carried as text and left unresolved on purpose: a relative path resolves
    /// against the *client's* working directory, and over the daemon that is
    /// not this process's.
    Path(String),
    /// Nothing. The store record is written and no directory is touched,
    /// recorded or resolved.
    ///
    /// Deliberately **outside** SH-95's temp-store guard, because nothing is
    /// created at a path for that guard to judge — pinned as a recorded
    /// narrowing rather than left to be rediscovered as a hole.
    Nothing,
}

/// What `story project link` attaches.
///
/// **Two associations, and the asymmetry between them is the design.** An
/// origin is the *only* thing project selection ever consults; a checkout is
/// never consulted for resolution at all and answers a different question —
/// where this project's repo-side work runs. They are variants of one enum
/// because they share a verb, not because they are alike.
///
/// Not shared with [`UnlinkTarget`]: `unlink checkout` takes no argument,
/// because a project has at most one, and a single enum would have to spell
/// that as a field the parser accepts and the dispatcher throws away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// `link origin [URL]`.
    ///
    /// `None` means the origin *this directory's own repository* records. See
    /// [`origin_here`](crate::service::project::origin_here) for why "its own"
    /// is a condition rather than a description.
    Origin {
        /// The URL as the user typed it, unnormalized; `None` reads it from git.
        url: Option<String>,
    },
    /// `link checkout [PATH]`.
    ///
    /// Carried as text and left unresolved on purpose: a relative path resolves
    /// against the *client's* working directory, and over the daemon that is
    /// not this process's. `None` means that directory.
    Checkout {
        /// The directory, or `None` for the one the client ran in.
        path: Option<String>,
    },
}

/// What `story project unlink` detaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnlinkTarget {
    /// `unlink origin [URL]`, reading the working directory's origin when the
    /// URL is omitted, exactly as [`LinkTarget::Origin`] does.
    Origin {
        /// The URL as the user typed it; `None` reads it from git.
        url: Option<String>,
    },
    /// `unlink checkout` — no argument, because a project has at most one and
    /// naming it would be a second way of saying the same thing.
    Checkout,
}

/// The `story project settings …` forms.
///
/// Explicit subcommands rather than flags or a bare `key=value`, matching every
/// other verb group in this parser — and so that one missing shell word cannot
/// turn a read into a write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsAction {
    /// Every setting, with its effective value and where that value came from.
    List,
    /// One setting.
    Get {
        /// The dotted name, such as `sync.auto_transition`.
        key: String,
    },
    /// Write one setting.
    Set {
        /// The dotted name.
        key: String,
        /// The value, validated against the setting's kind.
        value: String,
    },
    /// Clear one setting, returning it to its default or to nothing.
    Unset {
        /// The dotted name.
        key: String,
    },
}

/// The `story store …` subcommands.
///
/// About *stores* rather than about anything inside one, which is what makes
/// them different from every other verb: `store new` names the store it creates,
/// so it must not resolve — let alone create — the ambient one on its way.
/// `backup` is the other shape a store-wide verb takes: it resolves the
/// *ambient* store like any ordinary command, rather than avoiding one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreAction {
    /// Create an empty store at a path nothing else owns.
    New {
        /// Where to put it. Resolved against the client's working directory.
        path: String,
    },
    /// Take a verified, on-demand backup of the ambient store — the safe
    /// alternative to hand-copying `store.db` before a risky operation
    /// (SH-135). Writes into
    /// [`crate::env::Environment::maintenance_backups_dir`], which the daily
    /// schedule never prunes, so the result survives by construction.
    Backup {
        /// Distinguishes this backup from every other in a shared, unpruned
        /// directory — `pre-sh130-purge`, say. Defaults to `manual` when
        /// omitted. Validated by [`crate::daemon::backup::validate_label`]
        /// before it becomes part of a filename.
        label: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebAction {
    /// `None` means "wherever the environment says", exactly as it does for
    /// [`DaemonAction::Start`] — these are the same command under two names, so
    /// an omitted `--port` has to mean the same thing in both (SH-249).
    Start {
        port: Option<u16>,
    },
    Stop,
    Status,
    Serve {
        port: Option<u16>,
    },
    Open,
    Address,
}

/// `story token …` (SH-255) — the named, persistent, revocable credential
/// that authenticates the dashboard.
///
/// Dispatched client-side against the control route, exactly like
/// [`WebAction::Open`] arming a coupon, and for the same reason: a token
/// record is daemon process/filesystem state (`tokens.json` in the daemon's
/// own state directory), not store data, so `/api/v1/invoke`'s
/// `StoreInvoker` has no handle to it. See [`crate::api::tokens`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenAction {
    /// Mints a fresh token named `name`, good for
    /// [`crate::api::tokens::DEFAULT_TTL`]. The raw secret is printed exactly
    /// once, on this response — nothing the daemon holds afterward can
    /// reconstruct it.
    New { name: String },
    /// Every live token: name, prefix, and both timestamps. Never a secret —
    /// the registry cannot produce one after mint time, so there is nothing
    /// to withhold by omission here, only by construction.
    List,
    /// Ends the named token immediately, whether or not it has expired.
    Revoke { name: String },
}

/// The global flags, and everything that is not one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobalFlags {
    /// `--json`: render the machine-readable envelope.
    pub json: bool,
    /// `--quiet`: suppress the human rendering.
    pub quiet: bool,
    /// `--no-hooks`: do not fire the project's event hooks.
    pub no_hooks: bool,
    /// `--store-path <file>`: run this command against a named store.
    ///
    /// Global because it has to be: the store is resolved before a verb is
    /// dispatched, and a flag that only some commands honoured would be a flag
    /// that silently stops applying somewhere in the middle of a script.
    ///
    /// It names a **file**, not a directory, which is what makes it a different
    /// lever from `$STORYHOOK_DATA_DIR`. `main` publishes the resolved path into
    /// `$STORYHOOK_STORE_PATH` so that everything this invocation starts — the
    /// daemon, a git hook, a `story` a hook itself runs — agrees about which
    /// store it is in.
    pub store_path: Option<PathBuf>,
    /// `--project <slug>`: act on this project, whatever directory this is.
    ///
    /// Global for a different reason than [`Self::store_path`], and the
    /// difference is worth keeping straight. A store is a process-wide fact and
    /// is *published* to children; a project is a fact about one invocation and
    /// is deliberately **not**, because exporting it for every descendant would
    /// be the "current project" state SH-116 exists to abolish.
    ///
    /// What makes it global is the parser: [`split_global_flags`] runs before a
    /// verb is read, so one entry here reaches every verb at once — and, because
    /// the token never survives to the verb's own arguments, SH-62's
    /// fail-closed gate never has to be told about it.
    pub project: Option<String>,
    /// `--deadline <seconds>`: give up waiting on the daemon after this long,
    /// rather than however long `lifecycle::ensure` and the invoker's own
    /// exchange bound would otherwise allow (up to `SPAWN_LOCK_DEADLINE` +
    /// `SERVED_DEADLINE` — 150s).
    ///
    /// A flag, not an environment variable, and deliberately so: a caller that
    /// cannot wait — a session hook Claude Code will kill regardless — states
    /// that once, for the one invocation asking. `$STORYHOOK_EXCHANGE_DEADLINE_SECS`
    /// (deleted, SH-174) showed the hazard the other shape has: a variable is
    /// set once and inherited by everything a shell starts afterward, so one
    /// `export` would silently abandon every write on the machine (SH-182).
    ///
    /// Expiry does not cancel the request: the daemon finishes whatever it
    /// accepted, and this process simply stops waiting for the answer and
    /// reports so. Applied in `main`, global because the bound has to cover
    /// the whole round trip, including starting a daemon that does not exist
    /// yet, which happens before a verb is dispatched.
    pub deadline: Option<std::time::Duration>,
}

/// Splits the global flags out of `args`, leaving the verb and its own
/// arguments behind.
///
/// Fallible only because `--store-path` takes a value: a flag whose value went
/// missing must not be dropped on the floor, because the command would then run
/// against a *different store* than the one the caller named.
pub fn split_global_flags(args: &[String]) -> Result<(GlobalFlags, Vec<String>), AppError> {
    let mut flags = GlobalFlags::default();
    let mut filtered = Vec::new();
    let mut explicit_set_input = false;
    let mut legacy_set_input_candidate = false;

    let mut i = 0;
    while i < args.len() {
        // A bare `--` ends option scanning for globals too. Without this the
        // terminator would be an escape that leaks: `story comment SH-1 --
        // --json is great` would still lose the word and still flip stdout to
        // an envelope, which is worse than having no escape at all. The
        // terminator itself is kept and removed later, so the verb parser can
        // still see where it was.
        if args[i] == "--" {
            filtered.extend_from_slice(&args[i..]);
            break;
        }
        match args[i].as_str() {
            "--input-json" if set_option_position(&filtered) => {
                explicit_set_input = true;
                filtered.push(args[i].clone());
                if let Some(value) = args.get(i + 1) {
                    // This is input even when empty or shaped like a global
                    // flag. Let the JSON validator report malformed input.
                    filtered.push(value.clone());
                    i += 2;
                    continue;
                }
            }
            "--json" => {
                if set_option_position(&filtered)
                    && args.get(i + 1).is_some_and(|value| !value.starts_with('-'))
                {
                    // In a set option position a following non-option is an
                    // attempted legacy input, even if malformed or empty.
                    // This affects only calls that opt into --input-json.
                    legacy_set_input_candidate = true;
                }
                // If --json is followed by a JSON object literal, treat it as a
                // subcommand-specific --json <value> (e.g. `story set SH-1 --json '{...}'`)
                // rather than the global JSON-output flag.
                if let Some(next) = args.get(i + 1)
                    && next.starts_with('{')
                {
                    filtered.push(args[i].clone());
                    filtered.push(next.clone());
                    i += 2;
                    continue;
                }
                flags.json = true;
            }
            "--quiet" => flags.quiet = true,
            "--no-hooks" => flags.no_hooks = true,
            "--store-path" => {
                let Some(value) = args.get(i + 1).filter(|value| !value.is_empty()) else {
                    return Err(AppError::Usage(
                        "--store-path needs the path of a store file, for example \
                         `--store-path /tmp/scratch/store.db`."
                            .to_string(),
                    ));
                };
                flags.store_path = Some(PathBuf::from(value));
                i += 2;
                continue;
            }
            other if other.starts_with("--store-path=") => {
                let value = other.trim_start_matches("--store-path=");
                if value.is_empty() {
                    return Err(AppError::Usage(
                        "--store-path= was given no path. It names a store file, for example \
                         `--store-path=/tmp/scratch/store.db`."
                            .to_string(),
                    ));
                }
                flags.store_path = Some(PathBuf::from(value));
            }
            "--project" => {
                let Some(value) = args.get(i + 1).filter(|value| !value.is_empty()) else {
                    return Err(AppError::Usage(
                        "--project needs the slug of a project, for example \
                         `--project storyhook`. `story project list` shows the slugs this \
                         machine's store has."
                            .to_string(),
                    ));
                };
                flags.project = Some(value.clone());
                i += 2;
                continue;
            }
            other if other.starts_with("--project=") => {
                let value = other.trim_start_matches("--project=");
                if value.is_empty() {
                    return Err(AppError::Usage(
                        "--project= was given no slug. It names a project, for example \
                         `--project=storyhook`. `story project list` shows the slugs this \
                         machine's store has."
                            .to_string(),
                    ));
                }
                flags.project = Some(value.to_string());
            }
            "--deadline" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(deadline_usage());
                };
                flags.deadline = Some(parse_deadline_secs(value)?);
                i += 2;
                continue;
            }
            other if other.starts_with("--deadline=") => {
                let value = other.trim_start_matches("--deadline=");
                flags.deadline = Some(parse_deadline_secs(value)?);
            }
            _ => filtered.push(args[i].clone()),
        }
        i += 1;
    }

    if explicit_set_input && legacy_set_input_candidate {
        return Err(set_json_input_conflict());
    }
    Ok((flags, filtered))
}

/// Is the next token an option, rather than a value, in `set <id> ...`?
/// Globals already removed from this prefix do not affect its positions.
fn set_option_position(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("set") || args.len() < 2 {
        return false;
    }
    let declared = declared_flags(args).unwrap_or(&[]);
    let mut index = 2;
    while index < args.len() {
        let Some(name) = args[index].strip_prefix("--") else {
            return false;
        };
        let Some(flag) = declared.iter().find(|flag| flag.name == name) else {
            return false;
        };
        index += if flag.takes_value { 2 } else { 1 };
    }
    index == args.len()
}

fn set_json_input_conflict() -> AppError {
    AppError::Usage(
        "choose one JSON input: --input-json <object> or legacy --json <object>, not both. \
         Use a separate --json with no value to request JSON output."
            .to_string(),
    )
}

/// The error for `--deadline` given no value at all.
fn deadline_usage() -> AppError {
    AppError::Usage(
        "--deadline needs a number of seconds to wait for the daemon, for example \
         `--deadline 3`."
            .to_string(),
    )
}

/// Parses `--deadline`'s value: a non-negative whole number of seconds.
///
/// `0` is accepted rather than refused — it is a legitimate (if extreme)
/// request to abandon the very first wait, and refusing it would be one more
/// special case for a caller to work around. What it must not be is negative
/// or fractional: both parse as a `u64` failing, which folds them into the
/// same "not a number of seconds" message rather than needing their own.
fn parse_deadline_secs(value: &str) -> Result<std::time::Duration, AppError> {
    value
        .parse::<u64>()
        .map(std::time::Duration::from_secs)
        .map_err(|_| {
            AppError::Usage(format!(
                "--deadline=`{value}` is not a whole number of seconds, for example \
             `--deadline 3`."
            ))
        })
}

/// Whether `args` — a whole invocation, verb included — asks a verb to
/// explain itself rather than to run.
///
/// A `-h`/`--help` anywhere after the verb is a help request. It is never
/// data: no flag in this grammar takes a value that begins with `--` (see
/// [`parse_dash_flags`]), and a title, query, or comment that must literally
/// start with `--help` was never expressible in the position the verb reads
/// it from anyway.
pub fn is_help_request(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .any(|arg| arg == "-h" || arg == "--help")
}

/// The help a recognized verb answers a help request with: its own topic
/// when one exists, otherwise the general help.
fn help_for_verb(verb: &str) -> Invocation {
    let topic = model::CommandId::find(verb).map_or(verb, |command| command.help_topic());
    match crate::help_topics::get_help_topic(topic) {
        Some(_) => Invocation::HelpTopic {
            topic: topic.to_string(),
        },
        None => Invocation::Help,
    }
}

/// Answers `story <verb> --help` before the verb's own parser ever sees the
/// token.
///
/// Verbs parse their own flags, so a rule applied per-verb is a rule some
/// verb will miss: `story new --help` read `--help` as the title and created
/// a story called `--help`, allocating an id, bumping `next-id`, and dirtying
/// a storyhook-tracked repo — in answer to the conventional way of asking
/// what a command does (SH-52). This is the one place ahead of all of them.
///
/// An unrecognized verb is left alone, so `story frobnicate --help` still
/// reports the unknown command rather than papering over a typo with usage
/// text. Recognition asks [`dispatch`] rather than consulting a second copy
/// of the verb list, which could fall out of step with the first.
fn verb_help_request(args: &[String]) -> Option<Invocation> {
    let verb = args.first()?.as_str();
    // `story help <topic> --help` is `parse_help`'s to answer.
    if verb == "help" || verb.starts_with('-') || !is_help_request(args) {
        return None;
    }
    verb_is_recognized(verb).then(|| help_for_verb(verb))
}

/// Whether `verb` names a command this binary has.
///
/// Asks [`dispatch`] rather than consulting a second copy of the verb list,
/// which could fall out of step with the first. Every caller wants the same
/// answer for the same reason — to decline to speak about a verb that does not
/// exist, so a typo is reported as an unknown *command* rather than being
/// answered with usage text or a complaint about one of its flags.
fn verb_is_recognized(verb: &str) -> bool {
    model::CommandId::find(verb).is_some_and(|command| command != model::CommandId::Github)
}

use model::{FLAG_PATHS as VERB_FLAGS, Flag};

/// Whether `token` is shaped like a long flag rather than like data.
///
/// Shape, not prefix, and the difference is the whole reason free text
/// survives this gate. A token containing whitespace is never flag-shaped, so a
/// quoted title or comment — `story new "--fix the ingest path"` — arrives as
/// one argv element with a space in it and is data, untouched. `---` and a bare
/// `--` are likewise not flag-shaped: the first is a rule line someone pasted,
/// the second is the end-of-options terminator.
fn is_flag_shaped(token: &str) -> bool {
    if token.chars().any(char::is_whitespace) {
        return false;
    }
    let Some(rest) = token.strip_prefix("--") else {
        return false;
    };
    let name = rest.split_once('=').map_or(rest, |(name, _)| name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// The flags declared for this invocation's verb path, or `None` when the verb
/// declares nothing — which, because the table fails closed, means every
/// flag-shaped token is unknown.
fn declared_flags(args: &[String]) -> Option<&'static [Flag]> {
    let verb = args.first()?.as_str();
    let subcommand = args
        .get(1)
        .map(String::as_str)
        .filter(|token| !is_flag_shaped(token));

    subcommand
        .and_then(|sub| {
            VERB_FLAGS.iter().find(|entry| {
                entry.command.names().contains(&verb) && entry.subcommand == Some(sub)
            })
        })
        .or_else(|| {
            VERB_FLAGS
                .iter()
                .find(|entry| entry.command.names().contains(&verb) && entry.subcommand.is_none())
        })
        .map(|entry| entry.flags)
}

/// Refuses a flag-shaped token the verb does not declare, before any parser can
/// read it as data.
///
/// This is the one place ahead of all of them, for the same reason
/// [`verb_help_request`] is: verbs parse their own flags, so a rule applied
/// per-verb is a rule some verb will miss. SH-52 was this defect for `--help`
/// and was fixed one token at a time; SH-62 is the rest of the flag space
/// arriving two waves later, with eight verbs measured writing junk silently
/// and four of them minting a durable object nobody asked for.
///
/// It runs **after** `verb_help_request`, so a help request still outranks a
/// complaint about a flag, and it declines to speak about an unrecognized verb,
/// so `story frobnicate --typo` still reports the unknown *command*.
fn reject_unknown_flags(args: &[String]) -> Result<(), AppError> {
    let Some(verb) = args.first() else {
        return Ok(());
    };
    // `story --help` and `story -V` are not verbs; `dispatch` answers them.
    if verb.starts_with('-') || !verb_is_recognized(verb) {
        return Ok(());
    }

    let declared = declared_flags(args).unwrap_or(&[]);
    let mut index = 1;
    while index < args.len() {
        let token = args[index].as_str();
        if token == "--" {
            return Ok(());
        }
        if !is_flag_shaped(token) {
            index += 1;
            continue;
        }
        let name = token
            .strip_prefix("--")
            .and_then(|rest| rest.split('=').next())
            .unwrap_or_default();
        let Some(flag) = declared.iter().find(|flag| flag.name == name) else {
            return Err(AppError::Usage(unknown_flag_message(args, token, declared)));
        };
        // `--flag=value` carries its value in the same token.
        index += if flag.takes_value && !token.contains('=') {
            2
        } else {
            1
        };
    }
    Ok(())
}

/// The refusal a user reads: the token, the verb's real flags, and how to say
/// it if it was text all along.
///
/// The escape advice is deliberately not a ready-made command. Half the verbs
/// this can fire on take no positional argument at all, so a synthesized
/// `story doctor "--typo …"` would be an example that does not work — worse
/// than no example, because the reader would try it.
fn unknown_flag_message(args: &[String], token: &str, declared: &[Flag]) -> String {
    let verb = args[0].as_str();
    // Only a declared subcommand belongs in the command path. Titles and IDs
    // are user data, even when they occupy the same argv position.
    let subcommand = args.get(1).filter(|word| {
        VERB_FLAGS.iter().any(|entry| {
            entry.command.names().contains(&verb) && entry.subcommand == Some(word.as_str())
        })
    });
    let path = subcommand.map_or_else(|| verb.to_owned(), |sub| format!("{verb} {sub}"));
    let help = match help_for_verb(verb) {
        Invocation::HelpTopic { topic } => format!("story help {topic}"),
        _ => "story --help".to_owned(),
    };
    let known = if declared.is_empty() {
        format!("`story {path}` takes no command-specific flags.")
    } else {
        format!(
            "`story {path}` takes: {}",
            declared
                .iter()
                .map(|flag| format!("--{}", flag.name))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    format!(
        "unknown flag `{token}` for `story {path}`.\n\n{known}\nRun `{help}` for usage and global options.\n\
         If `{token}` is literal text, put it after `--`. Shell quotes alone do not escape a flag."
    )
}

pub fn parse_invocation(args: &[String]) -> Result<Invocation, AppError> {
    if args.is_empty() {
        return Ok(Invocation::Help);
    }

    if let Some(help) = verb_help_request(args) {
        return Ok(help);
    }

    reject_unknown_flags(args)?;

    dispatch(&strip_terminator(args))
}

/// Removes the first bare `--`, so no verb parser ever sees the terminator.
///
/// Only the first: a second `--` is data, which is what `git` does and what a
/// title made of dashes needs.
fn strip_terminator(args: &[String]) -> Vec<String> {
    match args.iter().position(|arg| arg == "--") {
        Some(at) => {
            let mut kept = args.to_vec();
            kept.remove(at);
            kept
        }
        None => args.to_vec(),
    }
}

/// Parses the narrow protocol used while shell dispatch owns workspace exclusion.
fn parse_internal(args: &[String]) -> Result<Invocation, AppError> {
    match args {
        [_, operation, id]
            if model::InternalVerb::find(operation)
                == Some(model::InternalVerb::SupersedeBlockDeliveries)
                && !id.is_empty()
                && !id.starts_with('-') =>
        {
            Ok(Invocation::SupersedeBlockDeliveries { id: id.clone() })
        }
        [_, operation, id]
            if model::InternalVerb::find(operation)
                == Some(model::InternalVerb::SupersedeContinuations)
                && !id.is_empty()
                && !id.starts_with('-') =>
        {
            Ok(Invocation::SupersedeContinuations { id: id.clone() })
        }
        _ => Err(AppError::Usage(crate::cli::model::usage::INTERNAL_1.into())),
    }
}

/// Routes an invocation to its verb's parser. Pure: it inspects `args` and
/// builds an [`Invocation`], which is what lets [`verb_help_request`] use it
/// to ask whether a verb exists.
fn dispatch(args: &[String]) -> Result<Invocation, AppError> {
    match model::CommandId::find(&args[0]) {
        Some(command) => command.parse(args),
        None => Err(AppError::Usage(format!(
            "unknown command `{}`. Run `story --help` for usage.",
            args[0]
        ))),
    }
}

fn parse_resources(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::RESOURCES_1;
    // The client owns its terminal locator; the daemon must not supply its own.
    let tmux_socket = match std::env::var("TMUX") {
        Ok(value) => value
            .split(',')
            .next()
            .filter(|s| std::path::Path::new(s).is_absolute())
            .map(Into::into),
        Err(_) => {
            // SAFETY: geteuid has no preconditions and does not modify process state.
            let uid = unsafe { libc::geteuid() };
            Some(
                std::path::PathBuf::from(
                    std::env::var_os("TMUX_TMPDIR").unwrap_or_else(|| "/tmp".into()),
                )
                .join(format!("tmux-{uid}/default")),
            )
        }
    };
    let mut options = crate::service::resources::ResourceOptions {
        tmux_socket,
        ..Default::default()
    };
    let mut socket_seen = false;
    let mut id = None;
    let mut iter = args[1..].iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--location-only" => {
                if std::mem::replace(&mut options.location_only, true) {
                    return Err(AppError::Usage(usage.into()));
                }
            }
            "--lease-json" | "--window-name" | "--worktree-root" | "--tmux-socket" => {
                let value = iter
                    .next()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| AppError::Usage(usage.into()))?;
                let duplicate = match arg.as_str() {
                    "--lease-json" => options.lease_json.replace(value.clone()).is_some(),
                    "--window-name" => options.window_name.replace(value.clone()).is_some(),
                    "--worktree-root" => options.worktree_root.replace(value.into()).is_some(),
                    _ => {
                        if !std::path::Path::new(value).is_absolute() {
                            return Err(AppError::Usage(
                                "tmux socket path must be absolute".into(),
                            ));
                        }
                        options.tmux_socket = Some(value.into());
                        std::mem::replace(&mut socket_seen, true)
                    }
                };
                if duplicate {
                    return Err(AppError::Usage(usage.into()));
                }
            }
            value if !value.starts_with('-') && id.is_none() => id = Some(value.to_string()),
            _ => return Err(AppError::Usage(usage.into())),
        }
    }
    Ok(Invocation::Resources {
        id: id.ok_or_else(|| AppError::Usage(usage.into()))?,
        options,
    })
}

fn parse_cleanup(args: &[String]) -> Result<Invocation, AppError> {
    let mut dry_run = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            _ => return Err(AppError::Usage(crate::cli::model::usage::CLEANUP_1.into())),
        }
    }
    Ok(Invocation::Cleanup { dry_run })
}

const CLAIM_USAGE: &str = crate::cli::model::usage::CLAIM_1;

const UNCLAIM_USAGE: &str = crate::cli::model::usage::UNCLAIM_1;

const PROJECT_USAGE: &str = crate::cli::model::usage::PROJECT_1;

const PROJECT_SHOW_USAGE: &str = crate::cli::model::usage::PROJECT_2;

const PROJECT_DELETE_USAGE: &str = crate::cli::model::usage::PROJECT_3;

const PROJECT_SET_PREFIX_USAGE: &str = crate::cli::model::usage::PROJECT_4;

const PROJECT_NEW_USAGE: &str = crate::cli::model::usage::PROJECT_5;

const PROJECT_LINK_USAGE: &str = crate::cli::model::usage::PROJECT_6;

const PROJECT_UNLINK_USAGE: &str = crate::cli::model::usage::PROJECT_7;

const PROJECT_SETTINGS_USAGE: &str = crate::cli::model::usage::PROJECT_8;

fn parse_project(args: &[String]) -> Result<Invocation, AppError> {
    let action = args
        .get(1)
        .ok_or_else(|| AppError::Usage(PROJECT_USAGE.to_string()))?;
    match model::ProjectVerb::find(action.as_str()) {
        Some(model::ProjectVerb::New) => parse_project_new(args),
        Some(model::ProjectVerb::Delete) => parse_project_delete(args),
        Some(model::ProjectVerb::SetPrefix) => parse_project_set_prefix(args),
        // Redirects, never `unknown command`. Being told where a command went
        // is the whole difference from being told it never existed, and 34
        // files and five years of documents say `story project init`. Kept for
        // the life of the 2.x line, removed at 3.0.0, and listed as commands
        // nowhere — a redirect is a signpost, not a surface.
        //
        // Not aliases, deliberately. An alias would keep the drive-by creation
        // shape alive under a new name, and that shape is the thing being
        // retired: a positional nobody could tell from a name, and a prefix
        // minted silently into every id the project will ever have.
        Some(model::ProjectVerb::Init) => Err(AppError::Usage(
            "`story project init` is now `story project new`.\n\nIt takes no path: name the \
             checkout with `--attach <PATH>`, or `--no-attach` for a project with no checkout \
             here. `--prefix` is required — it is minted into every story id and cannot be \
             changed afterwards.\n\n  story project new --prefix <PREFIX> [--name <NAME>] \
             [--attach <PATH> | --no-attach]\n\nRun it with no flags at a terminal to be asked."
                .to_string(),
        )),
        Some(model::ProjectVerb::Deinit) => Err(AppError::Usage(
            "`story project deinit` is now `story project delete`.\n\nIt takes no path or slug: \
             it destroys the project this directory resolves to, or the one named by `--project \
             <slug>`. It no longer deletes `.storyhook.toml` or `AGENTS.md` from any checkout.\n\n\
             \x20 story project delete [--force]"
                .to_string(),
        )),
        Some(model::ProjectVerb::List) => {
            expect_no_more(&args[2..], PROJECT_USAGE)?;
            Ok(Invocation::Project {
                action: ProjectAction::List,
            })
        }
        // Refused with its own usage rather than the group's: a trailing word
        // here is most likely somebody reaching for a target this verb
        // deliberately does not take, and the message that helps says so.
        Some(model::ProjectVerb::Show) if args.len() == 2 => Ok(Invocation::Project {
            action: ProjectAction::Show,
        }),
        Some(model::ProjectVerb::Show) => Err(AppError::Usage(PROJECT_SHOW_USAGE.to_string())),
        Some(model::ProjectVerb::Link) => parse_project_link(args),
        Some(model::ProjectVerb::Unlink) => parse_project_unlink(args),
        Some(model::ProjectVerb::Settings) => parse_project_settings(args),
        None => Err(AppError::Usage(PROJECT_USAGE.to_string())),
    }
}

/// `story project link origin [URL]` / `story project link checkout [PATH]`.
///
/// The optional word is taken positionally rather than by flag because it *is*
/// the object of the verb; a flag would make `link origin` read as though the
/// URL were an option on some other operation.
fn parse_project_link(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_LINK_USAGE.to_string());
    if args.len() > 4 {
        return Err(usage());
    }
    let value = args.get(3).cloned();
    let target = match model::ProjectLinkVerb::find(args.get(2).ok_or_else(usage)?.as_str()) {
        Some(model::ProjectLinkVerb::Origin) => LinkTarget::Origin { url: value },
        Some(model::ProjectLinkVerb::Checkout) => LinkTarget::Checkout { path: value },
        None => return Err(usage()),
    };
    Ok(Invocation::Project {
        action: ProjectAction::Link(target),
    })
}

/// `story project unlink origin [URL]` / `story project unlink checkout`.
fn parse_project_unlink(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_UNLINK_USAGE.to_string());
    let target = match model::ProjectUnlinkVerb::find(args.get(2).ok_or_else(usage)?.as_str()) {
        Some(model::ProjectUnlinkVerb::Origin) if args.len() <= 4 => UnlinkTarget::Origin {
            url: args.get(3).cloned(),
        },
        // A path here is not ignored: a caller who typed one believes a project
        // has several checkouts and is about to be surprised by which one went.
        Some(model::ProjectUnlinkVerb::Checkout) if args.len() == 3 => UnlinkTarget::Checkout,
        None
        | Some(model::ProjectUnlinkVerb::Origin)
        | Some(model::ProjectUnlinkVerb::Checkout) => return Err(usage()),
    };
    Ok(Invocation::Project {
        action: ProjectAction::Unlink(target),
    })
}

/// `story project settings <list|get|set|unset> …`.
///
/// Every form takes a fixed number of words and nothing else. A trailing word
/// is refused rather than ignored: `settings set a b c` is more likely a
/// quoting mistake than a value the user meant to lose half of.
fn parse_project_settings(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_SETTINGS_USAGE.to_string());
    let word = |index: usize| args.get(index).cloned().ok_or_else(usage);

    let action = match model::ProjectSettingsVerb::find(args.get(2).ok_or_else(usage)?.as_str()) {
        Some(model::ProjectSettingsVerb::List) if args.len() == 3 => SettingsAction::List,
        Some(model::ProjectSettingsVerb::Get) if args.len() == 4 => {
            SettingsAction::Get { key: word(3)? }
        }
        Some(model::ProjectSettingsVerb::Set) if args.len() == 5 => SettingsAction::Set {
            key: word(3)?,
            value: word(4)?,
        },
        Some(model::ProjectSettingsVerb::Unset) if args.len() == 4 => {
            SettingsAction::Unset { key: word(3)? }
        }
        None
        | Some(model::ProjectSettingsVerb::List)
        | Some(model::ProjectSettingsVerb::Get)
        | Some(model::ProjectSettingsVerb::Set)
        | Some(model::ProjectSettingsVerb::Unset) => return Err(usage()),
    };

    Ok(Invocation::Project {
        action: ProjectAction::Settings(action),
    })
}

/// `story project delete [--force]`.
///
/// **No positional.** A bare word here would be a fourth way of naming a
/// project, and the one `deinit` had was the ambiguous kind: it resolved a slug
/// that happened to match a directory name as the directory. The refusal names
/// `--project` instead of guessing.
fn parse_project_delete(args: &[String]) -> Result<Invocation, AppError> {
    let mut force = false;

    for arg in &args[2..] {
        match arg.as_str() {
            "--force" | "-f" => force = true,
            _ => return Err(AppError::Usage(PROJECT_DELETE_USAGE.to_string())),
        }
    }

    Ok(Invocation::Project {
        action: ProjectAction::Delete { force },
    })
}

/// `story project set-prefix <NEW-PREFIX> [--force]`.
///
/// **Exactly one positional**, unlike `delete`: naming a project is still
/// `--project`'s job, but the new prefix has no flag to hide behind — it is
/// the one thing this verb exists to be told. Syntax is validated downstream,
/// by the same [`crate::domain::prefix::validate`] every other prefix passes
/// through, so the message a bad prefix gets is identical whichever verb
/// typed it.
fn parse_project_set_prefix(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_SET_PREFIX_USAGE.to_string());
    let new_prefix = args.get(2).ok_or_else(usage)?.clone();
    let mut force = false;

    for arg in &args[3..] {
        match arg.as_str() {
            "--force" | "-f" => force = true,
            _ => return Err(usage()),
        }
    }

    Ok(Invocation::Project {
        action: ProjectAction::SetPrefix { new_prefix, force },
    })
}

/// `story project new [--prefix P] [--name N] [--attach PATH | --no-attach]
/// [--no-agents-md]`.
///
/// **No positional of any kind.** A bare word after `new` could be a name or a
/// path with equal plausibility, and `deinit` already demonstrates what happens
/// when a parser has to guess: it is refused, naming both flags, rather than
/// resolved by a rule the user would have to know.
///
/// The absence of *every* switch is the interactive request. Only when at least
/// one is present does `--prefix` become required, because at that point
/// nobody is going to be asked for it and the alternative is minting `SH-1` in
/// a project the user never named `SH`.
fn parse_project_new(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(PROJECT_NEW_USAGE.to_string());
    let mut attach: Option<String> = None;
    let mut no_attach = false;
    let mut prefix: Option<String> = None;
    let mut name = None;
    let mut no_agents_md = false;
    let mut index = 2;
    // The first switch seen, kept so a refusal can name what made this
    // invocation non-interactive. "Pass --prefix" is advice; "you passed
    // --name, so I will not ask" is a diagnosis.
    let mut trigger: Option<String> = None;

    let value = |index: usize, flag: &str| {
        args.get(index + 1)
            .cloned()
            .ok_or_else(|| AppError::Usage(format!("{flag} requires a value")))
    };

    while index < args.len() {
        let word = args[index].as_str();
        if trigger.is_none() {
            trigger = Some(word.to_string());
        }
        match word {
            "--attach" => {
                attach = Some(value(index, "--attach")?);
                index += 2;
            }
            "--no-attach" => {
                no_attach = true;
                index += 1;
            }
            "--prefix" => {
                prefix = Some(value(index, "--prefix")?);
                index += 2;
            }
            "--name" => {
                name = Some(value(index, "--name")?);
                index += 2;
            }
            "--no-agents-md" => {
                no_agents_md = true;
                index += 1;
            }
            _ => return Err(usage()),
        }
    }

    // Nothing was said, so nothing is assumed. The client decides whether it
    // can ask; a request that reaches the dispatcher still carrying this is
    // refused there rather than defaulted.
    if args.len() == 2 {
        return Ok(Invocation::Project {
            action: ProjectAction::New(NewProjectRequest::Ask),
        });
    }

    let attach = match (attach, no_attach) {
        (Some(_), true) => {
            return Err(AppError::Usage(
                "`--attach` and `--no-attach` contradict each other: pass one or neither."
                    .to_string(),
            ));
        }
        (Some(path), false) => Attach::Path(path),
        (None, true) => Attach::Nothing,
        (None, false) => Attach::Cwd,
    };
    let prefix = prefix.ok_or_else(|| {
        AppError::Usage(format!(
            "`story project new` needs `--prefix <PREFIX>`. You passed `{}`, and any switch \
             means this invocation is being driven by a script rather than a person, so nothing \
             will be asked. A prefix is minted into every story id this project ever creates and \
             cannot be changed afterwards, so it is not defaulted.\n\n{PROJECT_NEW_USAGE}",
            trigger.as_deref().unwrap_or("a switch"),
        ))
    })?;
    let prefix = crate::domain::prefix::validate(&prefix)?;

    Ok(Invocation::Project {
        action: ProjectAction::New(NewProjectRequest::Stated(NewProjectSpec {
            attach,
            prefix,
            name,
            no_agents_md,
        })),
    })
}

fn parse_new(args: &[String]) -> Result<Invocation, AppError> {
    let mut state = None;
    let mut story_type = None;
    let mut description = None;
    let mut priority = None;
    let mut complexity = None;
    let mut labels: Vec<String> = Vec::new();
    let mut title_parts = Vec::new();
    let mut draft = false;
    let mut blocked_by: Vec<String> = Vec::new();
    let mut index = 1;
    let usage = crate::cli::model::usage::NEW_1;
    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            "--description" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                description = Some(value.clone());
                index += 2;
            }
            "--complexity" => {
                complexity = Some(
                    args.get(index + 1)
                        .ok_or_else(|| AppError::Usage(usage.to_string()))?
                        .clone(),
                );
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels.push(value.clone());
                index += 2;
            }
            "--labels" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels.push(value.clone());
                index += 2;
            }
            "--draft" => {
                draft = true;
                index += 1;
            }
            "--blocked-by" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                blocked_by.push(value.clone());
                index += 2;
            }
            _ => {
                title_parts.push(args[index].clone());
                index += 1;
            }
        }
    }
    if title_parts.is_empty() {
        return Err(AppError::Usage(usage.to_string()));
    }
    // `--label` and `--labels` are the same delimiter-splitting sink, whether
    // repeated (`--label a --label b`) or comma-joined (`--labels a,b`) or a
    // single value that itself carries a comma (`--label "a,b"`, the SH-164
    // repro) — every raw value collected above is split on `,` here rather
    // than only the ones that arrived via `--labels`.
    let labels = normalize_labels(labels);
    Ok(Invocation::New {
        title: title_parts.join(" "),
        state,
        story_type,
        description,
        priority,
        complexity,
        labels: if labels.is_empty() {
            None
        } else {
            Some(labels)
        },
        draft,
        blocked_by,
    })
}

fn parse_publish(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::PUBLISH_1.to_string(),
        ));
    }
    Ok(Invocation::Publish {
        id: args[1].clone(),
    })
}

const TYPE_USAGE: &str = crate::cli::model::usage::TYPE_1;
const TYPE_ADD_USAGE: &str = crate::cli::model::usage::TYPE_2;
const TYPE_SET_USAGE: &str = crate::cli::model::usage::TYPE_3;
const TYPE_REMOVE_USAGE: &str = crate::cli::model::usage::TYPE_4;

const STATE_USAGE: &str = crate::cli::model::usage::STATE_1;
const STATE_ADD_USAGE: &str = crate::cli::model::usage::STATE_2;
const STATE_SET_USAGE: &str = crate::cli::model::usage::STATE_3;
const STATE_REMOVE_USAGE: &str = crate::cli::model::usage::STATE_4;
const STATE_REORDER_USAGE: &str = crate::cli::model::usage::STATE_5;

/// Splits `--flag value` / `--flag=value` / `--flag` into (name, value)
/// pairs. A value that itself starts with `--` is read as the next flag, so
/// a missing value is reported rather than silently swallowing the flag that
/// followed it; pass `--flag=--value` when a value really does start with
/// dashes.
fn parse_dash_flags(
    args: &[String],
    usage: &str,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    let mut flags = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let Some(rest) = args[index].strip_prefix("--") else {
            return Err(AppError::Usage(usage.to_string()));
        };
        if let Some((name, value)) = rest.split_once('=') {
            flags.push((name.to_string(), Some(value.to_string())));
            index += 1;
        } else {
            let value = args
                .get(index + 1)
                .filter(|next| !next.starts_with("--"))
                .cloned();
            index += if value.is_some() { 2 } else { 1 };
            flags.push((rest.to_string(), value));
        }
    }
    Ok(flags)
}

/// The value of a flag that requires one.
fn flag_value(value: Option<String>, flag: &str, usage: &str) -> Result<String, AppError> {
    value.ok_or_else(|| AppError::Usage(format!("--{flag} needs a value\n{usage}")))
}

fn parse_state(args: &[String]) -> Result<Invocation, AppError> {
    let subcommand = args
        .get(1)
        .ok_or_else(|| AppError::Usage(STATE_USAGE.to_string()))?;

    let action = match model::StateVerb::find(subcommand.as_str()) {
        Some(model::StateVerb::List) => {
            expect_no_more(&args[2..], STATE_USAGE)?;
            StateAction::List
        }

        Some(model::StateVerb::Add) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_ADD_USAGE.to_string()))?;
            let mut superstate = None;
            let mut role = None;
            let mut description = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_ADD_USAGE)? {
                match flag.as_str() {
                    "super" => superstate = Some(flag_value(value, "super", STATE_ADD_USAGE)?),
                    "role" => role = Some(flag_value(value, "role", STATE_ADD_USAGE)?),
                    "description" => {
                        description = Some(flag_value(value, "description", STATE_ADD_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_ADD_USAGE.to_string())),
                }
            }
            StateAction::Add {
                slug,
                superstate: superstate
                    .ok_or_else(|| AppError::Usage(STATE_ADD_USAGE.to_string()))?,
                role,
                description,
            }
        }

        Some(model::StateVerb::Set) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_SET_USAGE.to_string()))?;
            let mut superstate = None;
            let mut role = None;
            let mut description = None;
            let mut clear_description = false;
            let mut move_stories_to = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_SET_USAGE)? {
                match flag.as_str() {
                    "super" => superstate = Some(flag_value(value, "super", STATE_SET_USAGE)?),
                    "role" => role = Some(flag_value(value, "role", STATE_SET_USAGE)?),
                    "description" => {
                        description = Some(flag_value(value, "description", STATE_SET_USAGE)?)
                    }
                    "no-description" => clear_description = true,
                    "move-stories-to" => {
                        move_stories_to =
                            Some(flag_value(value, "move-stories-to", STATE_SET_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_SET_USAGE.to_string())),
                }
            }
            if description.is_some() && clear_description {
                return Err(AppError::Usage(
                    "--description and --no-description contradict each other".to_string(),
                ));
            }
            StateAction::Set {
                slug,
                superstate,
                role,
                description,
                clear_description,
                move_stories_to,
            }
        }

        Some(model::StateVerb::Remove) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(STATE_REMOVE_USAGE.to_string()))?;
            let mut move_stories_to = None;
            for (flag, value) in parse_dash_flags(&args[3..], STATE_REMOVE_USAGE)? {
                match flag.as_str() {
                    "move-stories-to" => {
                        move_stories_to =
                            Some(flag_value(value, "move-stories-to", STATE_REMOVE_USAGE)?)
                    }
                    _ => return Err(AppError::Usage(STATE_REMOVE_USAGE.to_string())),
                }
            }
            StateAction::Remove {
                slug,
                move_stories_to,
            }
        }

        // Accepts both `reorder a,b,c` and `reorder a b c`, so the order can
        // be pasted from `story state list` output either way.
        Some(model::StateVerb::Reorder) => {
            let order: Vec<String> = args[1..]
                .iter()
                .skip(1)
                .flat_map(|arg| arg.split(','))
                .map(str::trim)
                .filter(|slug| !slug.is_empty())
                .map(str::to_string)
                .collect();
            if order.is_empty() {
                return Err(AppError::Usage(STATE_REORDER_USAGE.to_string()));
            }
            StateAction::Reorder { order }
        }

        None => return Err(AppError::Usage(STATE_USAGE.to_string())),
    };

    Ok(Invocation::State { action })
}

fn parse_list(args: &[String]) -> Result<Invocation, AppError> {
    let mut state = None;
    let mut flagged = false;
    let mut priority = None;
    let mut label = None;
    let mut created_after = None;
    let mut updated_after = None;
    let mut blocked = false;
    let mut ready = false;
    let mut stale = None;
    let mut phase = None;
    let mut story_type = None;
    let mut drafts = false;
    let mut unassessed = false;
    let mut include_closed = false;
    let mut include_archived = false;
    let mut index = 1;
    let usage = crate::cli::model::usage::LIST_1;

    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                label = Some(value.clone());
                index += 2;
            }
            "--created-after" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                created_after = Some(value.clone());
                index += 2;
            }
            "--updated-after" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                updated_after = Some(value.clone());
                index += 2;
            }
            "--flagged" => {
                flagged = true;
                index += 1;
            }
            "--blocked" => {
                blocked = true;
                index += 1;
            }
            "--ready" => {
                ready = true;
                index += 1;
            }
            "--stale" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                stale = Some(value.clone());
                index += 2;
            }
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                phase = Some(value.clone());
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            "--unassessed" => {
                unassessed = true;
                index += 1;
            }
            "--drafts" => {
                drafts = true;
                index += 1;
            }
            "--include-closed" => {
                include_closed = true;
                index += 1;
            }
            "--include-archived" => {
                include_archived = true;
                index += 1;
            }
            "--all" => {
                // Sugar, collapsed here rather than carried as its own
                // `Invocation::List` field: `--all` and
                // `--include-closed --include-archived` must parse to the
                // exact same `Invocation`, which a third field could only
                // drift from.
                include_closed = true;
                include_archived = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }

    Ok(Invocation::List {
        state,
        flagged,
        priority,
        label,
        created_after,
        updated_after,
        blocked,
        ready,
        stale,
        phase,
        story_type,
        drafts,
        unassessed,
        include_closed,
        include_archived,
    })
}

/// `story next [--count <n>] [--phase <N>] [--epic <id>]
/// [--exclude-label <csv>]` — a pure read.
///
/// SH-344's `--claim` was removed here by SH-477; claiming is
/// [`parse_claim`]'s verb. Nothing replaces the flag and no deprecation arm
/// survives: the word is simply not declared, so the ordinary unknown-flag
/// refusal names it, which is the same answer any other typo gets.
fn parse_next(args: &[String]) -> Result<Invocation, AppError> {
    let mut count = 1;
    let mut phase = None;
    let mut epic = None;
    let mut exclude_label = None;
    let mut index = 1;
    let usage = crate::cli::model::usage::NEXT_1;

    while index < args.len() {
        match args[index].as_str() {
            "--count" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                count = value.parse::<usize>().map_err(|_| {
                    AppError::Usage("--count must be a positive integer".to_string())
                })?;
                if count == 0 {
                    return Err(AppError::Usage(
                        "--count must be a positive integer".to_string(),
                    ));
                }
                index += 2;
            }
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                phase = Some(value.clone());
                index += 2;
            }
            "--epic" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                epic = Some(value.clone());
                index += 2;
            }
            "--exclude-label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                exclude_label = Some(value.clone());
                index += 2;
            }
            _ => break,
        }
    }
    expect_no_more(&args[index..], usage)?;

    Ok(Invocation::Next {
        count,
        phase,
        epic,
        exclude_label,
    })
}

/// `story claim (<id> | --next) [--phase <N>] [--epic <id>]
/// [--exclude-label <csv>] [--comment <text> | --no-comment] [--dry-run]`
/// (SH-476, SH-455).
///
/// The two forms are checked against each other *after* the flag loop rather
/// than as the loop runs, so `story claim --next SH-1` and `story claim SH-1
/// --next` are refused identically — flag order is never meaning here.
fn parse_claim(args: &[String]) -> Result<Invocation, AppError> {
    let mut id: Option<String> = None;
    let mut next = false;
    let mut phase: Option<String> = None;
    let mut epic: Option<String> = None;
    let mut exclude_label: Option<String> = None;
    let mut comment: Option<String> = None;
    let mut no_comment = false;
    let mut dry_run = false;
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            "--next" => {
                next = true;
                index += 1;
            }
            "--phase" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(CLAIM_USAGE.to_string()))?;
                phase = Some(value.clone());
                index += 2;
            }
            "--epic" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(CLAIM_USAGE.to_string()))?;
                epic = Some(value.clone());
                index += 2;
            }
            "--exclude-label" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(CLAIM_USAGE.to_string()))?;
                exclude_label = Some(value.clone());
                index += 2;
            }
            // The value is required, never optional: an optional-value
            // `--comment` would read `story claim SH-1 --comment --json` as a
            // comment saying `--json`, which is the SH-357 shape one layer
            // over — a word that lands somewhere nobody meant it to.
            "--comment" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(CLAIM_USAGE.to_string()))?;
                comment = Some(value.clone());
                index += 2;
            }
            "--no-comment" => {
                no_comment = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            word if id.is_none() && !word.starts_with('-') => {
                id = Some(word.to_string());
                index += 1;
            }
            _ => break,
        }
    }
    expect_no_more(&args[index..], CLAIM_USAGE)?;

    let target = match (id, next) {
        (Some(_), true) => {
            return Err(AppError::Usage(format!(
                "`story claim <id>` and `story claim --next` are two different requests; \
                 name one\n{CLAIM_USAGE}"
            )));
        }
        // A bare `story claim` is refused rather than resolved to `--next`.
        // This is a mutating call: a script whose id argument came out empty
        // must not silently claim whatever happened to sort first.
        (None, false) => {
            return Err(AppError::Usage(format!(
                "`story claim` needs a story id or `--next`\n{CLAIM_USAGE}"
            )));
        }
        (Some(id), false) => {
            if phase.is_some() {
                return Err(AppError::Usage(format!(
                    "`--phase` narrows what `--next` picks and means nothing beside an \
                     explicit id\n{CLAIM_USAGE}"
                )));
            }
            if epic.is_some() {
                return Err(AppError::Usage(format!(
                    "`--epic` narrows what `--next` picks and means nothing beside an \
                     explicit id\n{CLAIM_USAGE}"
                )));
            }
            if exclude_label.is_some() {
                return Err(AppError::Usage(format!(
                    "`--exclude-label` narrows what `--next` picks and means nothing beside an \
                     explicit id\n{CLAIM_USAGE}"
                )));
            }
            ClaimTarget::Story(id)
        }
        (None, true) => ClaimTarget::Next {
            phase,
            epic,
            exclude_label,
        },
    };

    let comment = match (comment, no_comment) {
        (Some(_), true) => {
            return Err(AppError::Usage(format!(
                "`--comment` and `--no-comment` say opposite things; name one\n{CLAIM_USAGE}"
            )));
        }
        (Some(text), false) => ClaimComment::Custom(text),
        (None, true) => ClaimComment::Suppressed,
        (None, false) => ClaimComment::Default,
    };

    Ok(Invocation::Claim {
        target,
        comment,
        dry_run,
    })
}

/// `story unclaim <id> [--comment <text> | --no-comment] [--dry-run]`
/// (SH-483).
///
/// [`parse_claim`]'s shape, minus the two things unclaim has no use for:
/// there is no `--next` form to be mutually exclusive with, and no `--phase`,
/// because neither selects anything here — the story is named or the command
/// is refused.
///
/// A word that lands nowhere is refused by the loop's own catch-all, which is
/// the same guarantee [`expect_no_more`] gives a fixed-arity arm (SH-357).
fn parse_unclaim(args: &[String]) -> Result<Invocation, AppError> {
    let mut id: Option<String> = None;
    let mut comment: Option<String> = None;
    let mut no_comment = false;
    let mut dry_run = false;
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            // The value is required, never optional — see `parse_claim`.
            "--comment" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(UNCLAIM_USAGE.to_string()))?;
                comment = Some(value.clone());
                index += 2;
            }
            "--no-comment" => {
                no_comment = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            word if id.is_none() && !word.starts_with('-') => {
                id = Some(word.to_string());
                index += 1;
            }
            _ => return Err(AppError::Usage(UNCLAIM_USAGE.to_string())),
        }
    }

    let Some(id) = id else {
        return Err(AppError::Usage(format!(
            "`story unclaim` needs a story id\n{UNCLAIM_USAGE}"
        )));
    };

    let comment = match (comment, no_comment) {
        (Some(_), true) => {
            return Err(AppError::Usage(format!(
                "`--comment` and `--no-comment` say opposite things; name one\n{UNCLAIM_USAGE}"
            )));
        }
        (Some(text), false) => UnclaimComment::Custom(text),
        (None, true) => UnclaimComment::Suppressed,
        (None, false) => UnclaimComment::Default,
    };

    Ok(Invocation::Unclaim {
        id,
        comment,
        dry_run,
    })
}

const ENGINE_START_USAGE: &str = crate::cli::model::usage::ENGINE_1;
const ENGINE_STATUS_USAGE: &str = crate::cli::model::usage::ENGINE_2;
const ENGINE_PAUSE_USAGE: &str = crate::cli::model::usage::ENGINE_3;
const ENGINE_RESUME_USAGE: &str = crate::cli::model::usage::ENGINE_4;
const ENGINE_STOP_USAGE: &str = crate::cli::model::usage::ENGINE_5;
const ENGINE_ACK_USAGE: &str = crate::cli::model::usage::ENGINE_6;

/// `story engine start|status|pause|resume|stop|ack` (SH-467).
///
/// Every complete arm delegates to a parser that calls [`expect_no_more`]
/// with that arm's own usage string. The separation is deliberate: a shared
/// catch-all would report `engine`'s family usage and lose which command
/// rejected the word, the exact SH-357 parser contract this family inherits.
fn parse_engine(args: &[String]) -> Result<Invocation, AppError> {
    let Some(action) = args.get(1).map(String::as_str) else {
        return Err(AppError::Usage(
            crate::cli::model::usage::ENGINE_7.to_string(),
        ));
    };
    let action = match model::EngineVerb::find(action) {
        Some(model::EngineVerb::ResetCheck) => {
            let usage = crate::cli::model::usage::ENGINE_8;
            if args.len() != 3 {
                return Err(AppError::Usage(usage.into()));
            }
            EngineAction::ResetCheck {
                story: args[2].clone(),
            }
        }
        Some(model::EngineVerb::ResetTarget) => {
            let usage = crate::cli::model::usage::ENGINE_9;
            if args.len() != 6 || args[2] != "--run" || args[4] != "--token" {
                return Err(AppError::Usage(usage.into()));
            }
            EngineAction::ResetTarget {
                run: args[3].clone(),
                token: args[5].clone(),
            }
        }
        Some(model::EngineVerb::Start) => parse_engine_start(args)?,
        Some(model::EngineVerb::Configure) => parse_engine_configure(args)?,
        Some(model::EngineVerb::Adopt) => parse_engine_adopt(args)?,
        Some(model::EngineVerb::Status) => EngineAction::Status {
            run: parse_engine_run(args, ENGINE_STATUS_USAGE)?,
        },
        Some(model::EngineVerb::Pause) => EngineAction::Pause {
            run: parse_engine_run(args, ENGINE_PAUSE_USAGE)?,
        },
        Some(model::EngineVerb::Resume) => EngineAction::Resume {
            run: parse_engine_run(args, ENGINE_RESUME_USAGE)?,
        },
        Some(model::EngineVerb::Stop) => parse_engine_stop(args)?,
        Some(model::EngineVerb::Ack) => EngineAction::Ack {
            run: parse_engine_run(args, ENGINE_ACK_USAGE)?,
        },
        None => {
            return Err(AppError::Usage(
                crate::cli::model::usage::ENGINE_7.to_string(),
            ));
        }
    };
    Ok(Invocation::Engine { action })
}

const ENGINE_ADOPT_USAGE: &str = crate::cli::model::usage::ENGINE_10;

fn parse_engine_adopt(args: &[String]) -> Result<EngineAction, AppError> {
    let mut run = None;
    let mut ids = Vec::new();
    let mut index = 2;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--run" && run.is_none() {
            run = Some(
                args.get(index + 1)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| AppError::Usage(ENGINE_ADOPT_USAGE.into()))?
                    .clone(),
            );
            index += 2;
        } else if arg.starts_with('-') {
            return Err(AppError::Usage(ENGINE_ADOPT_USAGE.into()));
        } else {
            ids.push(arg.clone());
            index += 1;
        }
    }
    if ids.is_empty() {
        return Err(AppError::Usage(ENGINE_ADOPT_USAGE.into()));
    }
    Ok(EngineAction::Adopt { run, ids })
}

const ENGINE_CONFIGURE_USAGE: &str = crate::cli::model::usage::ENGINE_11;

fn parse_engine_configure(args: &[String]) -> Result<EngineAction, AppError> {
    use crate::service::engine::ConfigurePatch;
    let mut patch = ConfigurePatch::default();
    let mut run = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 2;
    while index < args.len() {
        let name = args[index].as_str();
        if !seen.insert(name) {
            return Err(AppError::Usage(ENGINE_CONFIGURE_USAGE.into()));
        }
        let raw = args
            .get(index + 1)
            .filter(|v| !v.starts_with("--"))
            .ok_or_else(|| AppError::Usage(ENGINE_CONFIGURE_USAGE.into()))?;
        match name {
            "--run" => run = Some(raw.clone()),
            "--lanes" => {
                let lanes = raw
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=MAX_ENGINE_LANES).contains(n))
                    .ok_or_else(|| {
                        AppError::Usage(format!(
                            "--lanes must be an integer from 1 through {MAX_ENGINE_LANES}"
                        ))
                    })?;
                patch.lanes = Some(lanes);
            }
            "--model" | "--effort" => {
                validate_dispatch_option_token(raw)
                    .map_err(|reason| AppError::Usage(format!("{name} {reason}")))?;
                if name == "--model" {
                    patch.model = Some(raw.clone());
                } else {
                    patch.effort = Some(raw.clone());
                }
            }
            "--speed" => {
                patch.speed = Some(EngineSpeed::parse(raw).ok_or_else(|| {
                    AppError::Usage("--speed must be `standard` or `fast`".into())
                })?)
            }
            _ => return Err(AppError::Usage(ENGINE_CONFIGURE_USAGE.into())),
        }
        index += 2;
    }
    if patch == ConfigurePatch::default() {
        return Err(AppError::Usage(ENGINE_CONFIGURE_USAGE.into()));
    }
    Ok(EngineAction::Configure { run, patch })
}

fn parse_engine_start(args: &[String]) -> Result<EngineAction, AppError> {
    let mut epic = None;
    let mut lanes = 1_u32;
    let mut agent = EngineAgent::Claude;
    let mut model = None;
    let mut effort = None;
    let mut speed = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--epic" => {
                epic = Some(
                    args.get(index + 1)
                        .ok_or_else(|| AppError::Usage(ENGINE_START_USAGE.to_string()))?
                        .clone(),
                );
                index += 2;
            }
            "--lanes" => {
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(ENGINE_START_USAGE.to_string()))?;
                lanes = raw.parse::<u32>().map_err(|_| {
                    AppError::Usage(format!(
                        "--lanes must be an integer from 1 through {MAX_ENGINE_LANES}"
                    ))
                })?;
                if !(1..=MAX_ENGINE_LANES).contains(&lanes) {
                    return Err(AppError::Usage(format!(
                        "--lanes must be an integer from 1 through {MAX_ENGINE_LANES}"
                    )));
                }
                index += 2;
            }
            "--agent" => {
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(ENGINE_START_USAGE.to_string()))?;
                agent = EngineAgent::parse(raw).ok_or_else(|| {
                    AppError::Usage("--agent must be `claude` or `codex`".to_string())
                })?;
                index += 2;
            }
            "--model" | "--effort" => {
                let name = args[index].trim_start_matches("--");
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(ENGINE_START_USAGE.to_string()))?
                    .clone();
                validate_dispatch_option_token(&raw)
                    .map_err(|reason| AppError::Usage(format!("--{name} {reason}")))?;
                if name == "model" {
                    model = Some(raw);
                } else {
                    effort = Some(raw);
                }
                index += 2;
            }
            "--speed" => {
                let raw = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(ENGINE_START_USAGE.to_string()))?;
                speed = Some(EngineSpeed::parse(raw).ok_or_else(|| {
                    AppError::Usage("--speed must be `standard` or `fast`".to_string())
                })?);
                index += 2;
            }
            _ => break,
        }
    }
    expect_no_more(&args[index..], ENGINE_START_USAGE)?;
    Ok(EngineAction::Start {
        epic,
        lanes,
        agent,
        model,
        effort,
        speed,
    })
}

fn parse_engine_run(args: &[String], usage: &str) -> Result<Option<String>, AppError> {
    let mut run = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--run" => {
                run = Some(
                    args.get(index + 1)
                        .ok_or_else(|| AppError::Usage(usage.to_string()))?
                        .clone(),
                );
                index += 2;
            }
            _ => break,
        }
    }
    expect_no_more(&args[index..], usage)?;
    Ok(run)
}

const VERIFIER_ACK_USAGE: &str = crate::cli::model::usage::VERIFIER_1;

/// `story verifier ack <incident-id>` (SH-666).
///
/// The incident id is positional and required on purpose: the halt comment and
/// the dashboard banner both print it, and an acknowledgement that named no
/// incident would clear whichever one is current — the stale-page hazard the
/// REST door already refuses. Same SH-357 contract as `parse_engine`: every
/// complete arm ends in [`expect_no_more`] with its own usage string.
fn parse_verifier(args: &[String]) -> Result<Invocation, AppError> {
    let Some(action) = args.get(1).map(String::as_str) else {
        return Err(AppError::Usage(
            crate::cli::model::usage::VERIFIER_2.to_string(),
        ));
    };
    let action = match model::VerifierVerb::find(action) {
        Some(model::VerifierVerb::Landing) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_3;
            match args
                .get(2)
                .and_then(|word| model::VerifierLandingVerb::find(word))
            {
                Some(model::VerifierLandingVerb::Show) if args.len() == 3 => {
                    VerifierAction::LandingShow
                }
                Some(model::VerifierLandingVerb::Release)
                    if args.len() == 6
                        && args[4] == "--reason"
                        && !is_flag_shaped(&args[3])
                        && !args[5].trim().is_empty() =>
                {
                    VerifierAction::LandingRelease {
                        intent_id: args[3].clone(),
                        reason: args[5].clone(),
                    }
                }
                None
                | Some(model::VerifierLandingVerb::Show)
                | Some(model::VerifierLandingVerb::Release) => {
                    return Err(AppError::Usage(USAGE.into()));
                }
            }
        }
        Some(model::VerifierVerb::Evidence) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_4;
            if args.len() != 3 || args[2].trim().is_empty() || is_flag_shaped(&args[2]) {
                return Err(AppError::Usage(USAGE.into()));
            }
            VerifierAction::Evidence {
                story_id: args[2].clone(),
            }
        }
        Some(model::VerifierVerb::RepairAdmit) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_5;
            if args.len() != 9
                || args[2..]
                    .iter()
                    .any(|s| s.trim().is_empty() || is_flag_shaped(s))
            {
                return Err(AppError::Usage(USAGE.into()));
            }
            let generation = args[4]
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| AppError::Usage(USAGE.into()))?;
            let input = crate::service::project_recovery::RepairInput {
                base: args[5].clone(),
                head: args[6].clone(),
                head_tree: args[7].clone(),
                tree: args[8].clone(),
            };
            input.validate()?;
            VerifierAction::RepairAdmit {
                story_id: args[2].clone(),
                attempt_id: args[3].clone(),
                generation,
                input,
            }
        }
        Some(model::VerifierVerb::Repair) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_6;
            let id = args
                .get(3)
                .filter(|s| !is_flag_shaped(s))
                .ok_or_else(|| AppError::Usage(USAGE.into()))?;
            match args
                .get(2)
                .and_then(|word| model::VerifierRepairVerb::find(word))
            {
                Some(model::VerifierRepairVerb::Show) if args.len() == 4 => {
                    VerifierAction::RepairShow {
                        recovery_id: id.clone(),
                    }
                }
                Some(model::VerifierRepairVerb::Decide)
                    if args.len() == 6 && args[4] == "--input" && !is_flag_shaped(&args[5]) =>
                {
                    VerifierAction::RepairDecide {
                        recovery_id: id.clone(),
                        input: args[5].clone(),
                    }
                }
                Some(model::VerifierRepairVerb::Satisfy)
                    if args.len() == 6 && args[4] == "--input" && !is_flag_shaped(&args[5]) =>
                {
                    VerifierAction::RepairSatisfy {
                        recovery_id: id.clone(),
                        input: args[5].clone(),
                    }
                }
                None
                | Some(model::VerifierRepairVerb::Show)
                | Some(model::VerifierRepairVerb::Decide)
                | Some(model::VerifierRepairVerb::Satisfy) => {
                    return Err(AppError::Usage(USAGE.into()));
                }
            }
        }
        Some(model::VerifierVerb::MeasureGateClass) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_MEASUREMENT;
            if args.len() != 6
                || args[4] != "--output"
                || [2, 3, 5]
                    .iter()
                    .any(|&i| args[i].is_empty() || is_flag_shaped(&args[i]))
            {
                return Err(AppError::Usage(USAGE.into()));
            }
            VerifierAction::MeasureGateClass {
                checkout: args[2].clone().into(),
                commit: args[3].clone(),
                output: args[5].clone().into(),
            }
        }
        Some(model::VerifierVerb::GateConfig) => {
            const USAGE: &str = crate::cli::model::usage::VERIFIER_7;
            if args.len() != 6 {
                return Err(AppError::Usage(USAGE.into()));
            }
            VerifierAction::GateConfig {
                checkout: args[2].clone().into(),
                base: args[3].clone(),
                head: args[4].clone(),
                tree: args[5].clone(),
            }
        }
        Some(model::VerifierVerb::Status)
        | Some(model::VerifierVerb::Start)
        | Some(model::VerifierVerb::Stop)
        | Some(model::VerifierVerb::Drain) => {
            expect_no_more(&args[2..], crate::cli::model::usage::VERIFIER_8)?;
            match action {
                "status" => VerifierAction::Status,
                "start" => VerifierAction::Start,
                "stop" => VerifierAction::Stop,
                _ => VerifierAction::Drain,
            }
        }
        Some(model::VerifierVerb::Ack) => {
            let Some(incident_id) = args.get(2).filter(|word| !is_flag_shaped(word)) else {
                return Err(AppError::Usage(format!(
                    "`story verifier ack` needs the incident id the halt comment printed\n{VERIFIER_ACK_USAGE}"
                )));
            };
            if args.get(3).map(String::as_str) == Some("--leave-stopped") {
                expect_no_more(&args[4..], VERIFIER_ACK_USAGE)?;
                VerifierAction::AckLeaveStopped {
                    incident_id: incident_id.clone(),
                }
            } else {
                expect_no_more(&args[3..], VERIFIER_ACK_USAGE)?;
                VerifierAction::Ack {
                    incident_id: incident_id.clone(),
                }
            }
        }
        None => {
            return Err(AppError::Usage(
                crate::cli::model::usage::VERIFIER_2.to_string(),
            ));
        }
    };
    Ok(Invocation::Verifier { action })
}

fn parse_engine_stop(args: &[String]) -> Result<EngineAction, AppError> {
    let mut run = None;
    let mut now = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--run" => {
                run = Some(
                    args.get(index + 1)
                        .ok_or_else(|| AppError::Usage(ENGINE_STOP_USAGE.to_string()))?
                        .clone(),
                );
                index += 2;
            }
            "--now" => {
                now = true;
                index += 1;
            }
            _ => break,
        }
    }
    expect_no_more(&args[index..], ENGINE_STOP_USAGE)?;
    Ok(EngineAction::Stop {
        run,
        now,
        caller: crate::service::reset::ResetCaller::capture(),
    })
}

fn parse_report(args: &[String]) -> Result<Invocation, AppError> {
    let mut html = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--html" => {
                html = true;
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(
                    crate::cli::model::usage::REPORT_1.to_string(),
                ));
            }
        }
    }
    Ok(Invocation::Report { html })
}

fn parse_search(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::SEARCH_1.to_string(),
        ));
    }
    Ok(Invocation::Search {
        query: join_tokens(&args[1..]),
    })
}

fn parse_import(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() > 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::IMPORT_1.to_string(),
        ));
    }
    let file = args.get(1).cloned();
    Ok(Invocation::Import { file })
}

fn parse_decompose(args: &[String]) -> Result<Invocation, AppError> {
    let mut file = None;
    let mut stdin = false;
    let mut dry_run = false;
    let mut index = 1;
    let usage = crate::cli::model::usage::DECOMPOSE_1;

    while index < args.len() {
        match args[index].as_str() {
            "--stdin" => {
                stdin = true;
                index += 1;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            _ if file.is_none() && !args[index].starts_with("--") => {
                file = Some(args[index].clone());
                index += 1;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }

    if file.is_none() && !stdin {
        return Err(AppError::Usage(usage.to_string()));
    }

    Ok(Invocation::Decompose {
        file,
        stdin,
        dry_run,
    })
}

fn parse_import_project(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::IMPORT_PROJECT_1;
    let mut file = None;
    let mut legacy_links = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--legacy-links" => {
                legacy_links = true;
                index += 1;
            }
            _ if file.is_none() && !args[index].starts_with("--") => {
                file = Some(args[index].clone());
                index += 1;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    let file = file.ok_or_else(|| AppError::Usage(usage.to_string()))?;
    Ok(Invocation::ImportProject { file, legacy_links })
}

/// `story migrate [<path>] [--dry-run]`.
///
/// The positional argument is optional because the common case is running it
/// from inside the repository being migrated; when it is absent the invocation
/// carries `None` and the dispatcher walks up from the working directory.
fn parse_migrate(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::MIGRATE_1;
    let mut path = None;
    let mut dry_run = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            value if !value.starts_with('-') && path.is_none() => {
                path = Some(value.to_string());
                index += 1;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::Migrate { path, dry_run })
}

fn parse_context(args: &[String]) -> Result<Invocation, AppError> {
    let mut format = None;
    let mut story = None;
    let mut index = 1;
    let usage = crate::cli::model::usage::LOAD_CONTEXT_1;
    while index < args.len() {
        match args[index].as_str() {
            "--format" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                format = Some(value.clone());
                index += 2;
            }
            "--story" if story.is_none() => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    Ok(Invocation::Context { format, story })
}

fn validate_phase_number(s: &str) -> Result<(), AppError> {
    s.parse::<u32>()
        .map_err(|_| AppError::Validation(format!("phase must be a positive integer, got `{s}`")))
        .and_then(|n| {
            if n == 0 {
                Err(AppError::Validation("phase must be >= 1".to_string()))
            } else {
                Ok(())
            }
        })
}

fn parse_phase(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::PHASE_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match model::PhaseVerb::find(args[1].as_str()) {
        Some(model::PhaseVerb::List) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Phase {
                action: PhaseAction::List,
            })
        }
        Some(model::PhaseVerb::Show) => {
            let phase = args
                .get(2)
                .ok_or_else(|| AppError::Usage(crate::cli::model::usage::PHASE_2.to_string()))?
                .clone();
            validate_phase_number(&phase)?;
            Ok(Invocation::Phase {
                action: PhaseAction::Show { phase },
            })
        }
        Some(model::PhaseVerb::Add) => {
            if args.len() < 4 {
                return Err(AppError::Usage(
                    crate::cli::model::usage::PHASE_3.to_string(),
                ));
            }
            validate_phase_number(&args[3])?;
            Ok(Invocation::Phase {
                action: PhaseAction::Add {
                    id: args[2].clone(),
                    phase: args[3].clone(),
                },
            })
        }
        Some(model::PhaseVerb::Remove) => {
            let remove_usage = crate::cli::model::usage::PHASE_4;
            let id = args
                .get(2)
                .ok_or_else(|| AppError::Usage(remove_usage.to_string()))?
                .clone();
            expect_no_more(&args[3..], remove_usage)?;
            Ok(Invocation::Phase {
                action: PhaseAction::Remove { id },
            })
        }
        Some(model::PhaseVerb::Create) => {
            let phase = args
                .get(2)
                .ok_or_else(|| AppError::Usage(crate::cli::model::usage::PHASE_5.to_string()))?
                .clone();
            validate_phase_number(&phase)?;
            let title = if args.len() > 3 {
                Some(args[3..].join(" "))
            } else {
                None
            };
            Ok(Invocation::Phase {
                action: PhaseAction::Create { phase, title },
            })
        }
        None => Err(AppError::Usage(usage.to_string())),
    }
}

fn parse_type(args: &[String]) -> Result<Invocation, AppError> {
    let subcommand = args
        .get(1)
        .ok_or_else(|| AppError::Usage(TYPE_USAGE.to_string()))?;

    let action = match model::TypeVerb::find(subcommand.as_str()) {
        Some(model::TypeVerb::List) => {
            expect_no_more(&args[2..], TYPE_USAGE)?;
            TypeAction::List
        }

        Some(model::TypeVerb::Add) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_ADD_USAGE.to_string()))?;
            let mut description = None;
            let mut emoji = None;
            for (flag, value) in parse_dash_flags(&args[3..], TYPE_ADD_USAGE)? {
                match flag.as_str() {
                    "description" => {
                        description = Some(flag_value(value, "description", TYPE_ADD_USAGE)?)
                    }
                    "emoji" => emoji = Some(flag_value(value, "emoji", TYPE_ADD_USAGE)?),
                    _ => return Err(AppError::Usage(TYPE_ADD_USAGE.to_string())),
                }
            }
            TypeAction::Add {
                slug,
                description,
                emoji,
            }
        }

        Some(model::TypeVerb::Set) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_SET_USAGE.to_string()))?;
            let mut description = None;
            let mut clear_description = false;
            let mut emoji = None;
            let mut clear_emoji = false;
            for (flag, value) in parse_dash_flags(&args[3..], TYPE_SET_USAGE)? {
                match flag.as_str() {
                    "description" => {
                        description = Some(flag_value(value, "description", TYPE_SET_USAGE)?)
                    }
                    "no-description" => clear_description = true,
                    "emoji" => emoji = Some(flag_value(value, "emoji", TYPE_SET_USAGE)?),
                    "no-emoji" => clear_emoji = true,
                    _ => return Err(AppError::Usage(TYPE_SET_USAGE.to_string())),
                }
            }
            if description.is_some() && clear_description {
                return Err(AppError::Usage(
                    "--description and --no-description contradict each other".to_string(),
                ));
            }
            if emoji.is_some() && clear_emoji {
                return Err(AppError::Usage(
                    "--emoji and --no-emoji contradict each other".to_string(),
                ));
            }
            TypeAction::Set {
                slug,
                description,
                clear_description,
                emoji,
                clear_emoji,
            }
        }

        Some(model::TypeVerb::Remove) => {
            let slug = args
                .get(2)
                .cloned()
                .ok_or_else(|| AppError::Usage(TYPE_REMOVE_USAGE.to_string()))?;
            expect_no_more(&args[3..], TYPE_REMOVE_USAGE)?;
            TypeAction::Remove { slug }
        }

        None => return Err(AppError::Usage(TYPE_USAGE.to_string())),
    };

    Ok(Invocation::Type { action })
}

fn parse_epic(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::EPIC_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match model::EpicVerb::find(args[1].as_str()) {
        Some(model::EpicVerb::List) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Epic {
                action: EpicAction::List,
            })
        }
        Some(model::EpicVerb::Show) => {
            let show_usage = crate::cli::model::usage::EPIC_2;
            let id = args
                .get(2)
                .ok_or_else(|| AppError::Usage(show_usage.to_string()))?
                .clone();
            expect_no_more(&args[3..], show_usage)?;
            Ok(Invocation::Epic {
                action: EpicAction::Show { id },
            })
        }
        Some(model::EpicVerb::Create) => {
            if args.len() < 3 {
                return Err(AppError::Usage(
                    crate::cli::model::usage::EPIC_3.to_string(),
                ));
            }
            let title = join_tokens(&args[2..]);
            if title.is_empty() {
                return Err(AppError::Usage(
                    crate::cli::model::usage::EPIC_3.to_string(),
                ));
            }
            Ok(Invocation::Epic {
                action: EpicAction::Create { title },
            })
        }
        Some(model::EpicVerb::Add) => {
            let add_usage = crate::cli::model::usage::EPIC_4;
            if args.len() < 4 {
                return Err(AppError::Usage(add_usage.to_string()));
            }
            expect_no_more(&args[4..], add_usage)?;
            Ok(Invocation::Epic {
                action: EpicAction::Add {
                    epic_id: args[2].clone(),
                    story_id: args[3].clone(),
                },
            })
        }
        None => Err(AppError::Usage(usage.to_string())),
    }
}

fn parse_handoff(args: &[String]) -> Result<Invocation, AppError> {
    let mut since = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--since" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    AppError::Usage(crate::cli::model::usage::HANDOFF_1.to_string())
                })?;
                since = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(
                    crate::cli::model::usage::HANDOFF_1.to_string(),
                ));
            }
        }
    }
    Ok(Invocation::Handoff { since })
}

fn parse_graph(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() == 1 {
        return Ok(Invocation::Graph {
            mode: GraphMode::Overview,
        });
    }
    match args[1].as_str() {
        "--critical-path" => Ok(Invocation::Graph {
            mode: GraphMode::CriticalPath,
        }),
        "--blocked-by" => {
            let id = args
                .get(2)
                .ok_or_else(|| AppError::Usage(crate::cli::model::usage::GRAPH_1.to_string()))?;
            Ok(Invocation::Graph {
                mode: GraphMode::BlockedBy(id.clone()),
            })
        }
        "--parallel-groups" => Ok(Invocation::Graph {
            mode: GraphMode::ParallelGroups,
        }),
        _ => Err(AppError::Usage(
            crate::cli::model::usage::GRAPH_2.to_string(),
        )),
    }
}

fn parse_doctor(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() >= 2 && model::DoctorVerb::find(&args[1]) == Some(model::DoctorVerb::Abandoned) {
        return parse_doctor_abandoned(args);
    }
    if args.len() >= 2 && model::DoctorVerb::find(&args[1]) == Some(model::DoctorVerb::Crashes) {
        return parse_doctor_crashes(args);
    }
    if args.len() >= 2 && model::DoctorVerb::find(&args[1]) == Some(model::DoctorVerb::Install) {
        expect_no_more(&args[2..], crate::cli::model::usage::DOCTOR_1)?;
        return Ok(Invocation::DoctorInstall);
    }

    if args.len() == 1 {
        return Ok(Invocation::Doctor { fix: false });
    }

    if args.len() == 2 && args[1] == "--fix" {
        return Ok(Invocation::Doctor { fix: true });
    }

    Err(AppError::Usage(
        crate::cli::model::usage::DOCTOR_2.to_string(),
    ))
}

fn parse_doctor_abandoned(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::DOCTOR_3;
    let action = match &args[2..] {
        [] => AbandonedAction::List,
        [clear, target]
            if model::DoctorAbandonedVerb::find(clear)
                == Some(model::DoctorAbandonedVerb::Clear)
                && target == "--all" =>
        {
            AbandonedAction::Clear { request_id: None }
        }
        [clear, id]
            if model::DoctorAbandonedVerb::find(clear)
                == Some(model::DoctorAbandonedVerb::Clear) =>
        {
            AbandonedAction::Clear {
                request_id: Some(id.clone()),
            }
        }
        // `clear` with nothing after it is refused rather than treated as
        // `--all`: forgetting the whole ledger should never be the default
        // reading of a token a user might have forgotten to finish typing.
        _ => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::DoctorAbandoned { action })
}

fn parse_doctor_crashes(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::DOCTOR_4;
    let action = match &args[2..] {
        [] => CrashesAction::List,
        [clear, target]
            if model::DoctorCrashesVerb::find(clear) == Some(model::DoctorCrashesVerb::Clear)
                && target == "--all" =>
        {
            CrashesAction::Clear { crash_id: None }
        }
        [clear, id]
            if model::DoctorCrashesVerb::find(clear) == Some(model::DoctorCrashesVerb::Clear) =>
        {
            CrashesAction::Clear {
                crash_id: Some(id.clone()),
            }
        }
        // `clear` with nothing after it is refused rather than treated as
        // `--all`, the same reasoning `parse_doctor_abandoned` uses.
        _ => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::DoctorCrashes { action })
}

fn parse_update(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::UPDATE_1;
    let mut check = false;
    let mut force = false;
    let mut source = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--check" => {
                check = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--source" if source.is_none() => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.into()))?;
                crate::github_access::ReleaseSource::parse(value)?;
                source = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    if check && force {
        return Err(AppError::Usage(format!(
            "{usage} (--check and --force are mutually exclusive)"
        )));
    }
    Ok(Invocation::Update {
        check,
        force,
        source,
    })
}

fn parse_hooks(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::HOOKS_1;
    let test_usage = crate::cli::model::usage::HOOKS_2;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match model::HooksVerb::find(args[1].as_str()) {
        Some(model::HooksVerb::Install) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Hooks {
                action: HooksAction::Install,
            })
        }
        Some(model::HooksVerb::Uninstall) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Hooks {
                action: HooksAction::Uninstall,
            })
        }
        Some(model::HooksVerb::List) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Hooks {
                action: HooksAction::List,
            })
        }
        Some(model::HooksVerb::Test) => {
            let event_type = args
                .get(2)
                .ok_or_else(|| AppError::Usage(test_usage.to_string()))?;
            expect_no_more(&args[3..], test_usage)?;
            Ok(Invocation::Hooks {
                action: HooksAction::Test {
                    event_type: event_type.clone(),
                },
            })
        }
        None => Err(AppError::Usage(format!(
            "unknown hooks action: {}",
            args[1]
        ))),
    }
}

fn parse_scaffold(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::SCAFFOLD_1.to_string(),
        ));
    }
    let kind = args[1].clone();
    if model::ScaffoldVerb::find(&kind).is_none() {
        return Err(AppError::Usage(
            crate::cli::model::usage::SCAFFOLD_1.to_string(),
        ));
    }
    Ok(Invocation::Scaffold { kind })
}

fn parse_commit_sync(args: &[String]) -> Result<Invocation, AppError> {
    let mut since = None;
    let mut index = 1;
    let usage = crate::cli::model::usage::COMMIT_SYNC_1;
    while index < args.len() {
        match args[index].as_str() {
            "--since" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                since = Some(value.clone());
                index += 2;
            }
            _ => {
                return Err(AppError::Usage(usage.to_string()));
            }
        }
    }
    Ok(Invocation::CommitSync { since })
}

fn parse_link_pr(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::LINK_PR_1;
    if args.len() < 3 || args.len() > 4 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let close_on_merge = match args.get(3).map(String::as_str) {
        None => true,
        Some("--no-close-on-merge") => false,
        Some(_) => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::LinkPr {
        id: args[1].clone(),
        url: args[2].clone(),
        close_on_merge,
    })
}

fn parse_unlink_pr(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            crate::cli::model::usage::UNLINK_PR_1.to_string(),
        ));
    }
    Ok(Invocation::UnlinkPr {
        id: args[1].clone(),
        url: args[2].clone(),
    })
}

const ATTACHMENT_USAGE: &str = crate::cli::model::usage::ATTACHMENT_1;

/// `story attachment add|list|remove|save` (SH-315).
fn parse_attachment(args: &[String]) -> Result<Invocation, AppError> {
    let usage = || AppError::Usage(ATTACHMENT_USAGE.to_string());
    if args.len() < 2 {
        return Err(usage());
    }
    match model::AttachmentVerb::find(args[1].as_str()) {
        Some(model::AttachmentVerb::Add) => {
            let mut positionals: Vec<String> = Vec::new();
            let mut name = None;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--name" => {
                        let value = args.get(index + 1).ok_or_else(usage)?;
                        name = Some(value.clone());
                        index += 2;
                    }
                    _ => {
                        positionals.push(args[index].clone());
                        index += 1;
                    }
                }
            }
            if positionals.len() != 2 {
                return Err(usage());
            }
            Ok(Invocation::Attachment {
                action: AttachmentAction::Add {
                    id: positionals[0].clone(),
                    path: positionals[1].clone(),
                    name,
                },
            })
        }
        Some(model::AttachmentVerb::List) => {
            let id = args.get(2).ok_or_else(usage)?.clone();
            expect_no_more(&args[3..], ATTACHMENT_USAGE)?;
            Ok(Invocation::Attachment {
                action: AttachmentAction::List { id },
            })
        }
        Some(model::AttachmentVerb::Remove) => {
            let id = args.get(2).ok_or_else(usage)?.clone();
            let attachment_id = parse_attachment_id(args.get(3))?;
            expect_no_more(&args[4..], ATTACHMENT_USAGE)?;
            Ok(Invocation::Attachment {
                action: AttachmentAction::Remove { id, attachment_id },
            })
        }
        Some(model::AttachmentVerb::Save) => {
            let id = args.get(2).ok_or_else(usage)?.clone();
            let attachment_id = parse_attachment_id(args.get(3))?;
            let path = args.get(4).ok_or_else(usage)?.clone();
            expect_no_more(&args[5..], ATTACHMENT_USAGE)?;
            Ok(Invocation::Attachment {
                action: AttachmentAction::Save {
                    id,
                    attachment_id,
                    path,
                },
            })
        }
        None => Err(usage()),
    }
}

/// Parses an attachment id — a positive integer, matching
/// [`crate::domain::Attachment::id`]'s own type.
fn parse_attachment_id(raw: Option<&String>) -> Result<u32, AppError> {
    let usage = || AppError::Usage(ATTACHMENT_USAGE.to_string());
    let raw = raw.ok_or_else(usage)?;
    let id: u32 = raw.parse().map_err(|_| {
        AppError::Usage(format!(
            "attachment id must be a positive integer, got `{raw}`"
        ))
    })?;
    if id == 0 {
        return Err(AppError::Usage(
            "attachment id must be a positive integer".to_string(),
        ));
    }
    Ok(id)
}

fn parse_pr_check(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() > 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::PR_CHECK_1.to_string(),
        ));
    }
    Ok(Invocation::PrCheck {
        id: args.get(1).cloned(),
    })
}

fn parse_help(args: &[String]) -> Result<Invocation, AppError> {
    let flags: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| a.starts_with("--"))
        .map(|a| a.as_str())
        .collect();
    let positional: Vec<&str> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .map(|a| a.as_str())
        .collect();

    // Checked ahead of the flags, so that a second word is refused whichever
    // form it was written in: `story help move delete` and `story help --all
    // delete` were both answering about the first word and discarding the
    // second (SH-357).
    expect_no_more(
        positional.get(1..).unwrap_or_default(),
        crate::cli::model::usage::HELP_1,
    )?;

    let has_compact = flags.contains(&"--compact");
    let has_all = flags.contains(&"--all");

    // If both flags given, --compact wins (no crash)
    if has_compact {
        return Ok(Invocation::HelpCompact);
    }
    if has_all {
        return Ok(Invocation::HelpAll);
    }

    if let Some(topic) = positional.first() {
        return Ok(Invocation::HelpTopic {
            topic: (*topic).to_string(),
        });
    }

    Ok(Invocation::Help)
}

fn parse_plugin(args: &[String]) -> Result<Invocation, AppError> {
    const USAGE: &str = crate::cli::model::usage::PLUGIN_1;
    let Some(action) = args.get(1).map(String::as_str) else {
        return Err(AppError::Usage(USAGE.to_string()));
    };
    match model::PluginVerb::find(action) {
        Some(model::PluginVerb::Install) | Some(model::PluginVerb::Uninstall)
            if args.len() != 3 =>
        {
            Err(AppError::Usage(USAGE.to_string()))
        }
        Some(model::PluginVerb::Install) => Ok(Invocation::Plugin {
            action: PluginAction::Install {
                target: args[2].clone(),
            },
        }),
        Some(model::PluginVerb::Uninstall) => Ok(Invocation::Plugin {
            action: PluginAction::Uninstall {
                target: args[2].clone(),
            },
        }),
        Some(model::PluginVerb::Reinstall) if args.len() != 2 => {
            Err(AppError::Usage(USAGE.to_string()))
        }
        Some(model::PluginVerb::Reinstall) => Ok(Invocation::Plugin {
            action: PluginAction::Reinstall,
        }),
        Some(model::PluginVerb::Run) if args.len() < 4 => Err(AppError::Usage(USAGE.to_string())),
        Some(model::PluginVerb::Run) => Ok(Invocation::Plugin {
            action: PluginAction::Run {
                target: args[2].clone(),
                args: args[3..].to_vec(),
            },
        }),
        None => Err(AppError::Usage(format!(
            "unknown plugin action: {}. {USAGE}",
            action
        ))),
    }
}

/// `story store new <path> | backup [--label <text>]`.
fn parse_store(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::STORE_1;
    let action = match args
        .get(1)
        .map(String::as_str)
        .and_then(model::StoreVerb::find)
    {
        Some(model::StoreVerb::New) => {
            let action = match args.get(2) {
                Some(path) if !path.is_empty() && !path.starts_with('-') => {
                    StoreAction::New { path: path.clone() }
                }
                _ => {
                    return Err(AppError::Usage(format!(
                        "{usage}\n\n`store new` names the file to create, for example \
                         `story store new /tmp/scratch/store.db`."
                    )));
                }
            };
            if args.len() > 3 {
                return Err(AppError::Usage(usage.to_string()));
            }
            action
        }
        Some(model::StoreVerb::Backup) => {
            let mut label = None;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--label" => {
                        let value = args.get(index + 1).ok_or_else(|| {
                            AppError::Usage(format!(
                                "{usage}\n\n`--label` takes a value, for example \
                                 `story store backup --label pre-migration`."
                            ))
                        })?;
                        label = Some(value.clone());
                        index += 2;
                    }
                    other => {
                        return Err(AppError::Usage(format!(
                            "{usage}\n\nunrecognized argument `{other}`."
                        )));
                    }
                }
            }
            StoreAction::Backup { label }
        }
        _ => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::Store { action })
}

fn parse_daemon(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::DAEMON_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let action = match model::DaemonVerb::find(args[1].as_str()) {
        Some(model::DaemonVerb::Logs) => {
            let mut follow = false;
            let mut directory = None;
            let mut rest = args[2..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--follow" if !follow => follow = true,
                    "--directory" if directory.is_none() => {
                        directory = Some(PathBuf::from(
                            rest.next().ok_or_else(|| AppError::Usage(usage.into()))?,
                        ));
                    }
                    _ => return Err(AppError::Usage(usage.into())),
                }
            }
            DaemonAction::Logs { follow, directory }
        }
        Some(model::DaemonVerb::Start) => DaemonAction::Start {
            port: parse_port_flag(&args[2..], usage)?,
        },
        Some(model::DaemonVerb::Restart) => {
            expect_no_more(&args[2..], usage)?;
            DaemonAction::Restart
        }
        // Spelled as a flag rather than a subcommand because it is not one a
        // user runs: it is what the spawner execs, and what a launchd agent
        // runs, and both of those are storyhook talking to itself.
        Some(model::DaemonVerb::Serve) => {
            let (port, owner) = parse_serve_flags(&args[2..], usage)?;
            DaemonAction::Serve { port, owner }
        }
        Some(model::DaemonVerb::Stop) => DaemonAction::Stop {
            force: match &args[2..] {
                [] => false,
                [flag] if flag == "--force" => true,
                _ => return Err(AppError::Usage(usage.to_string())),
            },
        },
        Some(model::DaemonVerb::Gc) => DaemonAction::Gc {
            force: match &args[2..] {
                [] => false,
                [flag] if flag == "--force" => true,
                _ => return Err(AppError::Usage(usage.to_string())),
            },
        },
        Some(model::DaemonVerb::Status) => {
            expect_no_more(&args[2..], usage)?;
            DaemonAction::Status
        }
        Some(model::DaemonVerb::Install) => {
            let this_binary = matches!(&args[2..], [flag] if flag == "--this-binary");
            if !this_binary {
                expect_no_more(&args[2..], usage)?;
            }
            DaemonAction::Install { this_binary }
        }
        Some(model::DaemonVerb::Uninstall) => {
            expect_no_more(&args[2..], usage)?;
            DaemonAction::Uninstall
        }
        // `token` prints the master bearer token. The named-token verbs are
        // `story token new|list|revoke`; anything written after `daemon token`
        // is reaching for one of those, and printing the master credential
        // instead is the worst available answer (SH-357).
        Some(model::DaemonVerb::Token) => {
            expect_no_more(&args[2..], usage)?;
            DaemonAction::Token
        }
        None => return Err(AppError::Usage(usage.to_string())),
    };
    Ok(Invocation::Daemon { action })
}

/// Refuses `rest` unless it is empty, naming the first word that has no
/// meaning where it was written.
///
/// Every arm that is already a complete command ends with this call. What it
/// guards is not a typo but a *silence* (SH-357): `story daemon token new
/// psamathe` used to print the daemon's master bearer token and exit 0,
/// because the arm never looked past `token`, and the output of a command
/// that ignored half its arguments is indistinguishable from the output of
/// one that understood them. On a credential-printing command that
/// manufactured the specific wrong belief "I minted a scoped, revocable
/// token".
///
/// The class is pinned by `tests/trailing_arguments.rs`, which derives every
/// command word from this file and asks the parser itself whether appending a
/// meaningless word leaves the [`Invocation`] unchanged — so a new arm that
/// forgets this call fails the suite the day it is written, rather than
/// waiting to be found by a user.
///
/// # Errors
///
/// [`AppError::Usage`] naming the unexpected word, above `usage`.
fn expect_no_more<Word: AsRef<str>>(rest: &[Word], usage: &str) -> Result<(), AppError> {
    match rest.first() {
        None => Ok(()),
        Some(unexpected) => Err(AppError::Usage(format!(
            "unexpected argument `{}`\n{usage}",
            unexpected.as_ref()
        ))),
    }
}

/// An optional trailing `--port <PORT>`, refusing anything else.
///
/// Port 0 is accepted here and nowhere else in the CLI: it means "let the kernel
/// choose", which is exactly what a test harness wants and what a user never
/// does deliberately.
fn parse_port_flag(rest: &[String], usage: &str) -> Result<Option<u16>, AppError> {
    match rest {
        [] => Ok(None),
        [flag, value] if flag == "--port" => value
            .parse::<u16>()
            .map(Some)
            .map_err(|_| AppError::Usage(format!("invalid port: {value}"))),
        [flag] if flag == "--port" => Err(AppError::Usage("--port requires a value".to_string())),
        _ => Err(AppError::Usage(usage.to_string())),
    }
}

/// The literal `--owner` values `daemon --serve` accepts (SH-784). Kept next
/// to [`parse_serve_flags`] rather than exported: nothing outside this
/// process's own internal callers (`spawn_child`, the installed plist) is
/// meant to type this flag, so there is no reason to name the values twice.
const OWNER_VALUES: [&str; 5] = [
    "launchd",
    "systemd",
    "fork-test-build",
    "fork-no-agent",
    "fork-no-manager",
];

/// `--serve`'s own flags: an optional `--port <PORT>` and an optional
/// `--owner <VALUE>`, in either order, each at most once.
///
/// Deliberately narrower than a general flag parser: `--serve` is internal
/// wiring (see [`DaemonAction::Serve`]'s own doc), so unknown flags and
/// repeats are refused rather than tolerated, the same as [`parse_port_flag`]
/// already refuses anything but `--port`.
fn parse_serve_flags(
    rest: &[String],
    usage: &str,
) -> Result<(Option<u16>, Option<String>), AppError> {
    let mut port = None;
    let mut owner = None;
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--port" if port.is_none() => {
                let value = rest
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage("--port requires a value".to_string()))?;
                port = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| AppError::Usage(format!("invalid port: {value}")))?,
                );
                index += 2;
            }
            "--owner" if owner.is_none() => {
                let value = rest
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage("--owner requires a value".to_string()))?;
                if !OWNER_VALUES.contains(&value.as_str()) {
                    return Err(AppError::Usage(format!(
                        "invalid --owner value `{value}`; expected one of {OWNER_VALUES:?}"
                    )));
                }
                owner = Some(value.clone());
                index += 2;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok((port, owner))
}

fn parse_web(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::WEB_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }

    match model::WebVerb::find(args[1].as_str()) {
        Some(model::WebVerb::Start) => {
            let port = parse_port_flag(&args[2..], usage)?;
            // Refused here and accepted on `--serve` below, which is not an
            // inconsistency: `--serve` is what the spawner execs, and "let the
            // kernel choose" is a thing a program means. Asking a *person* to
            // start their bookmarked dashboard on a port nobody can predict is
            // not.
            if port == Some(0) {
                return Err(AppError::Usage("invalid port: 0".to_string()));
            }
            Ok(Invocation::Web {
                action: WebAction::Start { port },
            })
        }
        Some(model::WebVerb::Stop) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Web {
                action: WebAction::Stop,
            })
        }
        Some(model::WebVerb::Status) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Web {
                action: WebAction::Status,
            })
        }
        Some(model::WebVerb::Open) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Web {
                action: WebAction::Open,
            })
        }
        Some(model::WebVerb::Address) => {
            expect_no_more(&args[2..], usage)?;
            Ok(Invocation::Web {
                action: WebAction::Address,
            })
        }
        // Retired (SH-255): a scoped dashboard capability no longer exists to
        // revoke. `story token revoke <name>` ends one named token; there is
        // no single command for "every token this daemon has issued" — naming
        // one deliberately, so revoking a token you did not mean to takes a
        // name, not a blast radius.
        Some(model::WebVerb::Revoke) => Err(AppError::Usage(
            "`story web revoke` is retired: named tokens replaced the scoped dashboard \
             capability it used to end. Run `story token list` to see what is live, then \
             `story token revoke <name>` to end one."
                .to_string(),
        )),
        // Internal: `story web --serve [--port N]`, what the spawner execs.
        Some(model::WebVerb::Serve) => Ok(Invocation::Web {
            action: WebAction::Serve {
                port: parse_port_flag(&args[2..], usage)?,
            },
        }),
        None => Err(AppError::Usage(usage.to_string())),
    }
}

/// The longest a token name may be, and its alphabet: letters, digits, `-`
/// and `_`. The same restriction [`crate::daemon::backup::validate_label`]
/// applies to a backup label, for the reason [`crate::api::tokens::intercept`]'s
/// own `query_param` doc names: a name in this shape never needs
/// percent-decoding on the wire, and it appears verbatim in a URL path
/// segment (`DELETE /api/v1/tokens/{name}`) and a query parameter
/// (`POST /api/v1/tokens?name=...`) alike.
const MAX_TOKEN_NAME_LEN: usize = 64;

/// Checks a user-supplied token name and returns it unchanged.
///
/// # Errors
///
/// [`AppError::Validation`] naming the offending value and the rule.
fn validate_token_name(raw: &str) -> Result<&str, AppError> {
    let refuse = |because: &str| {
        Err(AppError::Validation(format!(
            "invalid token name `{raw}`: {because}. A token name is 1-{MAX_TOKEN_NAME_LEN} \
             characters of letters, digits, `-` and `_`."
        )))
    };
    if raw.is_empty() {
        return refuse("it is empty");
    }
    if raw.chars().count() > MAX_TOKEN_NAME_LEN {
        return refuse("it is too long");
    }
    if let Some(bad) = raw
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return refuse(&format!(
            "`{bad}` is neither a letter, a digit, `-` nor `_`"
        ));
    }
    Ok(raw)
}

/// `story token new <name> | list | revoke <name>` (SH-255).
fn parse_token(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::TOKEN_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    match model::TokenVerb::find(args[1].as_str()) {
        Some(model::TokenVerb::New) => {
            let Some(name) = args.get(2) else {
                return Err(AppError::Usage(format!(
                    "{usage}\n\n`token new` names the token to mint."
                )));
            };
            if args.len() > 3 {
                return Err(AppError::Usage(usage.to_string()));
            }
            Ok(Invocation::Token {
                action: TokenAction::New {
                    name: validate_token_name(name)?.to_string(),
                },
            })
        }
        Some(model::TokenVerb::List) => {
            if args.len() > 2 {
                return Err(AppError::Usage(usage.to_string()));
            }
            Ok(Invocation::Token {
                action: TokenAction::List,
            })
        }
        Some(model::TokenVerb::Revoke) => {
            let Some(name) = args.get(2) else {
                return Err(AppError::Usage(format!(
                    "{usage}\n\n`token revoke` names the token to end."
                )));
            };
            if args.len() > 3 {
                return Err(AppError::Usage(usage.to_string()));
            }
            Ok(Invocation::Token {
                action: TokenAction::Revoke {
                    name: validate_token_name(name)?.to_string(),
                },
            })
        }
        None => Err(AppError::Usage(format!(
            "unknown token action: {}. {usage}",
            args[1]
        ))),
    }
}

fn parse_show(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::SHOW_1.to_string(),
        ));
    }
    Ok(Invocation::Show {
        id: args[1].clone(),
    })
}

fn parse_log(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(crate::cli::model::usage::LOG_1.to_string()));
    }
    Ok(Invocation::Log {
        id: args[1].clone(),
    })
}

fn parse_comment(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(
            crate::cli::model::usage::COMMENT_1.to_string(),
        ));
    }
    Ok(Invocation::Comment {
        id: args[1].clone(),
        text: join_tokens(&args[2..]),
    })
}

fn parse_move(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::MOVE_1;
    if args.len() < 3 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let id = args[1].clone();
    let state = args[2].clone();

    // `--if-state` and `--reason` are recognized only as a contiguous run of
    // flag+value pairs immediately following <state> (starting at args[3]),
    // in either order, each at most once — never scanned for anywhere else in
    // the trailing args. What that protects against is an unrelated
    // `--if-state`/`--reason` substring inside an unquoted multi-word comment
    // being silently spliced out mid-comment and mistaken for a real flag.
    // That reasoning is unchanged and this position-pinning stays; SH-205
    // only widens it from one recognized flag to two.
    //
    // What *has* changed (SH-62) is the sentence this comment used to carry
    // next: that a comment beginning with `--` must never fail as an
    // unrecognized flag, because comments have always been unrestricted free
    // text. The council that settled SH-62 overturned that in the letter and
    // kept it in the spirit, and the distinction is worth stating here because
    // this is where a reader will come looking.
    //
    // A flag-shaped token is now refused ahead of every parser — but *shape*,
    // not prefix: a token containing whitespace is never flag-shaped. So a real
    // comment (`story move SH-1 done "--sprint-23 wrapped"`) is one argv
    // element with spaces in it and is still unrestricted free text, exactly as
    // before. Only the unquoted single-token form (`… done --sprint-23`) now
    // errors, and it does so naming the token and offering `--`. A repo-wide
    // search for that form found one hit: SH-62's own defect description.
    let mut if_state = None;
    let mut awaiting = None;
    let mut comment_start = 3;
    loop {
        match args.get(comment_start).map(String::as_str) {
            Some("--if-state") if if_state.is_none() => {
                let value = args
                    .get(comment_start + 1)
                    .ok_or_else(|| AppError::Usage("--if-state requires a value".to_string()))?;
                if_state = Some(value.clone());
                comment_start += 2;
            }
            Some("--reason") if awaiting.is_none() => {
                let value = args
                    .get(comment_start + 1)
                    .ok_or_else(|| AppError::Usage("--reason requires a value".to_string()))?;
                awaiting = Some(value.clone());
                comment_start += 2;
            }
            _ => break,
        }
    }

    let comment = if comment_start < args.len() {
        Some(join_tokens(&args[comment_start..]))
    } else {
        None
    };

    Ok(Invocation::SetState {
        id,
        state,
        comment,
        if_state,
        awaiting,
    })
}

/// `story close <id> "<reason>"` (SH-505) — retire a story that was
/// deliberately not completed, keeping it and everything it records.
///
/// Sugar over [`Invocation::SetState`], not an invocation of its own. The state
/// it moves to is a real one ([`crate::domain::DROPPED_STATE_SLUG`]) and the
/// reason is a real comment, so this needs no new event kind, no new snapshot
/// field, no dispatch arm, and no additional wire surface — `story move <id> dropped
/// "<reason>"` does exactly the same thing and is the same story afterwards.
///
/// What the sugar adds is the requirement: `move` takes an optional comment,
/// and here the reason is the whole point of the verb, so a bare `story close
/// <id>` is a usage error rather than a silently unexplained abandonment.
///
/// Deliberately NOT idempotent, inheriting `set_state`'s own rule: an
/// already-closed story is refused, exactly as `story move <id> done` is. The
/// reason travels as the comment rather than as `--reason`, because
/// `set_state` refuses an `awaiting` reason on a CLOSED target.
fn parse_close(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::CLOSE_1;
    if args.len() < 3 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let reason = join_tokens(&args[2..]);
    if reason.is_empty() {
        return Err(AppError::Usage(usage.to_string()));
    }
    Ok(Invocation::SetState {
        id: args[1].clone(),
        state: crate::domain::DROPPED_STATE_SLUG.to_string(),
        comment: Some(reason),
        if_state: None,
        awaiting: None,
    })
}

const BLOCK_USAGE: &str = crate::cli::model::usage::BLOCK_1;
const UNBLOCK_USAGE: &str = crate::cli::model::usage::UNBLOCK_1;

/// `story block <id> --on <blocker> [--on <blocker>]... ["<reason>"]`, or
/// `story block <id> "<reason>"` (SH-398).
///
/// `--on` names a story as the blocker of record — written as a `blocked-by`
/// edge, which clears itself when that story closes, unlike prose. A reason
/// is required only when no `--on` was given at all, matching the pre-SH-398
/// contract for every caller that never learns about `--on`.
fn parse_block(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage(BLOCK_USAGE.to_string()));
    }
    let id = args[1].clone();
    let mut on: Vec<String> = Vec::new();
    let mut reason_start = 2;
    while reason_start < args.len() && args[reason_start] == "--on" {
        let value = args
            .get(reason_start + 1)
            .ok_or_else(|| AppError::Usage("--on requires a value".to_string()))?;
        on.push(value.clone());
        reason_start += 2;
    }
    let awaiting = if reason_start < args.len() {
        Some(join_tokens(&args[reason_start..]))
    } else {
        None
    };
    if on.is_empty() && awaiting.is_none() {
        return Err(AppError::Usage(BLOCK_USAGE.to_string()));
    }
    Ok(Invocation::SetAwaiting { id, awaiting, on })
}

/// `story unblock <id> [--on <blocker>]...` (SH-398).
///
/// No `--on`: clears the prose `awaiting` reason (pre-SH-398 behaviour).
/// One or more `--on`: removes just those `blocked-by` edges and leaves
/// `awaiting` untouched — a story can be unblocked from one dependency while
/// still waiting on another, or on its own prose reason.
fn parse_unblock(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 2 {
        return Err(AppError::Usage(UNBLOCK_USAGE.to_string()));
    }
    let id = args[1].clone();
    let mut on: Vec<String> = Vec::new();
    let mut index = 2;
    while index < args.len() && args[index] == "--on" {
        let value = args
            .get(index + 1)
            .ok_or_else(|| AppError::Usage("--on requires a value".to_string()))?;
        on.push(value.clone());
        index += 2;
    }
    expect_no_more(&args[index..], UNBLOCK_USAGE)?;
    Ok(Invocation::ClearAwaiting { id, on })
}

fn parse_prioritize(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            crate::cli::model::usage::PRIORITIZE_1.to_string(),
        ));
    }
    Ok(Invocation::SetPriority {
        id: args[1].clone(),
        priority: args[2].clone(),
    })
}

fn parse_label(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            crate::cli::model::usage::LABEL_1.to_string(),
        ));
    }
    let add = normalize_labels([&args[2]]);
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add,
        remove: Vec::new(),
    })
}

fn parse_unlabel(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 3 {
        return Err(AppError::Usage(
            crate::cli::model::usage::UNLABEL_1.to_string(),
        ));
    }
    let remove = normalize_labels([&args[2]]);
    Ok(Invocation::SetLabels {
        id: args[1].clone(),
        add: Vec::new(),
        remove,
    })
}

fn parse_reopen_verb(args: &[String]) -> Result<Invocation, AppError> {
    let id = args
        .get(1)
        .ok_or_else(|| AppError::Usage(crate::cli::model::usage::REOPEN_1.to_string()))?;
    expect_no_more(&args[2..], crate::cli::model::usage::REOPEN_1)?;
    Ok(Invocation::Reopen { id: id.clone() })
}

/// The retired `story purge` spelling. A redirect rather than an alias: the
/// old verb is named so scripts get a repair, but no second deletion surface
/// survives beside `story delete`.
fn parse_purge_verb(_args: &[String]) -> Result<Invocation, AppError> {
    Err(AppError::Usage(
        "`story purge` is retired; use `story delete <id> [--force]`.".to_string(),
    ))
}

/// `story archive <id>` — the "Archive" action (SH-43).
fn parse_hide(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::ARCHIVE_1.to_string(),
        ));
    }
    Ok(Invocation::Hide {
        id: args[1].clone(),
    })
}

/// `story unarchive <id>` — the inverse of [`parse_hide`].
fn parse_unhide(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 2 {
        return Err(AppError::Usage(
            crate::cli::model::usage::UNARCHIVE_1.to_string(),
        ));
    }
    Ok(Invocation::Unhide {
        id: args[1].clone(),
    })
}

/// `story archive-state <state> [--force]` — the CLOSED-superstate column's
/// bulk "Archive" (SH-43). Shaped like [`parse_delete_verb`]: unforced
/// answers with what it would hide and writes
/// nothing.
fn parse_hide_state(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::ARCHIVE_STATE_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let state = args[1].clone();
    let mut force = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--force" => force = true,
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::HideState { state, force })
}

fn parse_delete_verb(args: &[String]) -> Result<Invocation, AppError> {
    let usage = crate::cli::model::usage::DELETE_1;
    if args.len() < 2 {
        return Err(AppError::Usage(usage.to_string()));
    }
    let id = args[1].clone();
    let mut force = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--force" => force = true,
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }
    Ok(Invocation::Delete { id, force })
}

fn parse_relate(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 4 {
        return Err(AppError::Usage(
            crate::cli::model::usage::RELATE_1.to_string(),
        ));
    }
    Ok(Invocation::Relate {
        a: args[1].clone(),
        relation: args[2].clone(),
        b: args[3].clone(),
        remove: false,
    })
}

fn parse_unrelate(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() != 4 {
        return Err(AppError::Usage(
            crate::cli::model::usage::UNRELATE_1.to_string(),
        ));
    }
    Ok(Invocation::Relate {
        a: args[1].clone(),
        relation: args[2].clone(),
        b: args[3].clone(),
        remove: true,
    })
}

fn parse_set(args: &[String]) -> Result<Invocation, AppError> {
    if args.len() < 3 {
        return Err(AppError::Usage(crate::cli::model::usage::SET_1.to_string()));
    }
    let id = args[1].clone();
    let mut title = None;
    let mut state = None;
    let mut priority = None;
    let mut complexity = None;
    let mut labels = None;
    let mut blocked = None;
    let mut unblocked = false;
    let mut json = None;
    let mut explicit_json = false;
    let mut story_type = None;
    let mut description = None;
    let mut index = 2;
    let usage = crate::cli::model::usage::SET_2;

    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                title = Some(value.clone());
                index += 2;
            }
            "--state" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                state = Some(value.clone());
                index += 2;
            }
            "--complexity" => {
                complexity = Some(
                    args.get(index + 1)
                        .ok_or_else(|| AppError::Usage(usage.to_string()))?
                        .clone(),
                );
                index += 2;
            }
            "--priority" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                priority = Some(value.clone());
                index += 2;
            }
            "--labels" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                labels = Some(value.clone());
                index += 2;
            }
            "--blocked" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                blocked = Some(value.clone());
                index += 2;
            }
            "--unblocked" => {
                unblocked = true;
                index += 1;
            }
            "--json" | "--input-json" => {
                let is_explicit = args[index] == "--input-json";
                if json.is_some() && (explicit_json || is_explicit) {
                    return Err(set_json_input_conflict());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                json = Some(value.clone());
                explicit_json = is_explicit;
                index += 2;
            }
            "--type" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                story_type = Some(value.clone());
                index += 2;
            }
            "--description" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| AppError::Usage(usage.to_string()))?;
                description = Some(value.clone());
                index += 2;
            }
            _ => return Err(AppError::Usage(usage.to_string())),
        }
    }

    if title.is_none()
        && state.is_none()
        && priority.is_none()
        && complexity.is_none()
        && labels.is_none()
        && blocked.is_none()
        && !unblocked
        && json.is_none()
        && story_type.is_none()
        && description.is_none()
    {
        return Err(AppError::Usage(
            "no fields specified. Usage: story set <id> --<field> <value> ...".to_string(),
        ));
    }

    Ok(Invocation::SetFields {
        id,
        title,
        state,
        priority,
        complexity,
        labels,
        blocked,
        unblocked,
        json,
        story_type,
        description,
    })
}

fn join_tokens(tokens: &[String]) -> String {
    tokens.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        BLOCK_USAGE, ClaimComment, ClaimTarget, EpicAction, Invocation, PluginAction, TypeAction,
        UnclaimComment, parse_invocation,
    };
    use crate::error::AppError;

    /// Every verb that answers `ConfirmationRequired` must come back forced,
    /// or the client asks, hears yes, and re-sends the same question
    /// (SH-638: `story archive-state` printed its plan twice and archived
    /// nothing). `daemon stop --force` is the negative control: that flag
    /// signals a process rather than skipping a prompt, and `forced()` must
    /// never invent it.
    #[test]
    fn forced_authorizes_every_confirming_verb_and_nothing_else() {
        fn parse(args: &[&str]) -> Invocation {
            parse_invocation(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
                .expect("a well-formed invocation")
        }
        let forced = parse(&["archive-state", "done"]).forced();
        assert_eq!(
            forced,
            Invocation::HideState {
                state: "done".into(),
                force: true
            }
        );
        assert!(matches!(
            parse(&["delete", "SH-1"]).forced(),
            Invocation::Delete { force: true, .. }
        ));
        assert!(matches!(
            parse(&["project", "delete"]).forced(),
            Invocation::Project {
                action: super::ProjectAction::Delete { force: true }
            }
        ));
        assert!(matches!(
            parse(&["project", "set-prefix", "NEW"]).forced(),
            Invocation::Project {
                action: super::ProjectAction::SetPrefix { force: true, .. }
            }
        ));
        assert_eq!(
            parse(&["daemon", "stop"]).forced(),
            Invocation::Daemon {
                action: super::DaemonAction::Stop { force: false }
            }
        );
        let untouched = parse(&["list"]);
        assert_eq!(untouched.clone().forced(), untouched);
    }

    #[test]
    fn routes_move_command() {
        let invocation = parse_invocation(&[
            "move".to_string(),
            "SH-1".to_string(),
            "in-progress".to_string(),
        ])
        .unwrap();
        assert!(matches!(invocation, Invocation::SetState { .. }));
    }

    #[test]
    fn routes_show_command() {
        let invocation = parse_invocation(&["show".to_string(), "SH-1".to_string()]).unwrap();
        assert!(matches!(invocation, Invocation::Show { .. }));
    }

    #[test]
    fn unknown_command_errors() {
        let result = parse_invocation(&["SH-1".to_string(), "is".to_string(), "done".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn cleanup_accepts_only_its_bare_dry_run_flag() {
        assert_eq!(
            parse_invocation(&["cleanup".to_string()]).unwrap(),
            Invocation::Cleanup { dry_run: false }
        );
        assert_eq!(
            parse_invocation(&["cleanup".to_string(), "--dry-run".to_string()]).unwrap(),
            Invocation::Cleanup { dry_run: true }
        );
        assert!(
            parse_invocation(&["cleanup".to_string(), "--force".to_string()]).is_err(),
            "cleanup must not acquire a bypass for its safety gates"
        );
    }

    #[test]
    fn plugin_run_preserves_helper_flags_after_the_terminator() {
        let invocation = parse_invocation(&words(&[
            "plugin",
            "run",
            "codex",
            "--",
            "dispatch",
            "SH-9",
            "--agent=codex",
            "--auto",
        ]))
        .unwrap();
        assert_eq!(
            invocation,
            Invocation::Plugin {
                action: PluginAction::Run {
                    target: "codex".to_string(),
                    args: words(&["dispatch", "SH-9", "--agent=codex", "--auto"]),
                }
            }
        );
    }

    /// `reinstall` takes no target: the providers' own configurations say
    /// which are installed (SH-667). A target would invite `story plugin
    /// reinstall codex` to mean "install", which `install` already means.
    #[test]
    fn plugin_reinstall_takes_no_target() {
        assert_eq!(
            parse_invocation(&words(&["plugin", "reinstall"])).unwrap(),
            Invocation::Plugin {
                action: PluginAction::Reinstall
            }
        );
        let error = parse_invocation(&words(&["plugin", "reinstall", "codex"])).unwrap_err();
        assert!(
            error.to_string().contains("usage: story plugin"),
            "a stray target is a usage error, not a silent install: {error}"
        );
    }

    // --- `story claim` (SH-476) ---

    fn claim(args: &[&str]) -> Result<Invocation, AppError> {
        parse_invocation(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    /// The default is neither form, and neither is guessed at.
    #[test]
    fn a_bare_claim_names_both_forms_rather_than_picking_one() {
        let error = claim(&["claim"]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("--next"), "{message}");
        assert!(message.contains("story id"), "{message}");
    }

    #[test]
    fn claim_by_id_carries_no_phase_and_the_default_comment() {
        assert_eq!(
            claim(&["claim", "SH-1"]).unwrap(),
            Invocation::Claim {
                target: ClaimTarget::Story("SH-1".to_string()),
                comment: ClaimComment::Default,
                dry_run: false,
            }
        );
    }

    #[test]
    fn claim_next_carries_its_queue_filters() {
        assert_eq!(
            claim(&[
                "claim",
                "--next",
                "--phase",
                "2",
                "--epic",
                "SH-9",
                "--exclude-label",
                "no-auto,paused",
            ])
            .unwrap(),
            Invocation::Claim {
                target: ClaimTarget::Next {
                    phase: Some("2".to_string()),
                    epic: Some("SH-9".to_string()),
                    exclude_label: Some("no-auto,paused".to_string()),
                },
                comment: ClaimComment::Default,
                dry_run: false,
            }
        );
    }

    /// Flag order is never meaning: both spellings of the same mistake are
    /// refused identically.
    #[test]
    fn an_id_and_next_are_refused_in_either_order() {
        for args in [
            vec!["claim", "SH-1", "--next"],
            vec!["claim", "--next", "SH-1"],
        ] {
            let message = claim(&args).unwrap_err().to_string();
            assert!(message.contains("two different requests"), "{message}");
        }
    }

    /// `--phase` has nowhere to be stored beside an explicit id, so the
    /// parser is the last place it could have been written.
    #[test]
    fn phase_beside_an_id_is_refused() {
        let message = claim(&["claim", "SH-1", "--phase", "1"])
            .unwrap_err()
            .to_string();
        assert!(message.contains("--phase"), "{message}");
    }

    #[test]
    fn queue_filters_beside_an_id_are_refused() {
        for (flag, value) in [("--epic", "SH-9"), ("--exclude-label", "no-auto")] {
            let message = claim(&["claim", "SH-1", flag, value])
                .unwrap_err()
                .to_string();
            assert!(message.contains(flag), "{message}");
        }
    }

    #[test]
    fn next_carries_its_queue_filters() {
        let args = [
            "next",
            "--count",
            "3",
            "--phase",
            "2",
            "--epic",
            "SH-9",
            "--exclude-label",
            "no-auto",
        ]
        .map(str::to_string);
        assert_eq!(
            parse_invocation(&args).unwrap(),
            Invocation::Next {
                count: 3,
                phase: Some("2".to_string()),
                epic: Some("SH-9".to_string()),
                exclude_label: Some("no-auto".to_string()),
            }
        );
    }

    #[test]
    fn comment_and_no_comment_are_the_three_states() {
        let expect = |args: &[&str], comment: ClaimComment| {
            assert_eq!(
                claim(args).unwrap(),
                Invocation::Claim {
                    target: ClaimTarget::Story("SH-1".to_string()),
                    comment,
                    dry_run: false,
                }
            );
        };
        expect(&["claim", "SH-1"], ClaimComment::Default);
        expect(
            &["claim", "SH-1", "--comment", "mine"],
            ClaimComment::Custom("mine".to_string()),
        );
        expect(&["claim", "SH-1", "--no-comment"], ClaimComment::Suppressed);

        let message = claim(&["claim", "SH-1", "--comment", "x", "--no-comment"])
            .unwrap_err()
            .to_string();
        assert!(message.contains("opposite"), "{message}");
    }

    /// The value is required, always. An optional-value `--comment` would
    /// read `story claim SH-1 --comment --json` as a comment saying `--json`
    /// — SH-357's shape, a word landing where nobody meant it to.
    #[test]
    fn comment_takes_the_next_token_as_its_value() {
        assert!(claim(&["claim", "SH-1", "--comment"]).is_err());
        assert_eq!(
            claim(&["claim", "SH-1", "--comment", "--json"]).unwrap(),
            Invocation::Claim {
                target: ClaimTarget::Story("SH-1".to_string()),
                comment: ClaimComment::Custom("--json".to_string()),
                dry_run: false,
            }
        );
    }

    #[test]
    fn dry_run_is_a_bare_flag_on_both_forms() {
        assert!(matches!(
            claim(&["claim", "SH-1", "--dry-run"]).unwrap(),
            Invocation::Claim { dry_run: true, .. }
        ));
        assert!(matches!(
            claim(&["claim", "--next", "--dry-run"]).unwrap(),
            Invocation::Claim { dry_run: true, .. }
        ));
    }

    /// A second positional lands nowhere, so it is refused (SH-357).
    #[test]
    fn a_trailing_word_is_refused() {
        assert!(claim(&["claim", "SH-1", "junk"]).is_err());
        assert!(claim(&["claim", "--next", "junk"]).is_err());
    }

    // --- `story unclaim` (SH-483) ---

    fn unclaim(args: &[&str]) -> Result<Invocation, AppError> {
        parse_invocation(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn unclaim_needs_an_id_and_says_so() {
        let error = unclaim(&["unclaim"]).unwrap_err();
        assert!(
            error.to_string().contains("needs a story id"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn unclaim_by_id_carries_the_default_comment() {
        assert_eq!(
            unclaim(&["unclaim", "SH-1"]).unwrap(),
            Invocation::Unclaim {
                id: "SH-1".to_string(),
                comment: UnclaimComment::Default,
                dry_run: false,
            }
        );
    }

    #[test]
    fn unclaims_three_comment_states_are_all_reachable() {
        let expect = |args: &[&str], want: UnclaimComment| {
            let Invocation::Unclaim { comment, .. } = unclaim(args).unwrap() else {
                panic!("not an unclaim: {args:?}");
            };
            assert_eq!(comment, want, "for {args:?}");
        };
        expect(&["unclaim", "SH-1"], UnclaimComment::Default);
        expect(
            &["unclaim", "SH-1", "--comment", "back to you"],
            UnclaimComment::Custom("back to you".to_string()),
        );
        expect(
            &["unclaim", "SH-1", "--no-comment"],
            UnclaimComment::Suppressed,
        );
    }

    #[test]
    fn unclaims_comment_and_no_comment_together_are_refused() {
        let message = unclaim(&["unclaim", "SH-1", "--comment", "x", "--no-comment"])
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("say opposite things"),
            "unexpected refusal: {message}"
        );
    }

    /// The value is required, never optional — otherwise `story unclaim SH-1
    /// --comment --json` posts a comment saying `--json` (SH-357's shape).
    #[test]
    fn unclaims_comment_takes_the_next_token_as_its_value() {
        assert!(unclaim(&["unclaim", "SH-1", "--comment"]).is_err());
        assert_eq!(
            unclaim(&["unclaim", "SH-1", "--comment", "--json"]).unwrap(),
            Invocation::Unclaim {
                id: "SH-1".to_string(),
                comment: UnclaimComment::Custom("--json".to_string()),
                dry_run: false,
            }
        );
    }

    #[test]
    fn unclaims_dry_run_is_a_bare_flag() {
        assert!(matches!(
            unclaim(&["unclaim", "SH-1", "--dry-run"]).unwrap(),
            Invocation::Unclaim { dry_run: true, .. }
        ));
    }

    /// A second positional lands nowhere, so it is refused (SH-357). Unclaim
    /// has no `--next` form for a stray word to be read as, which makes the
    /// refusal the only correct answer rather than one of two.
    #[test]
    fn a_trailing_word_after_an_unclaim_is_refused() {
        assert!(unclaim(&["unclaim", "SH-1", "junk"]).is_err());
        assert!(unclaim(&["unclaim", "SH-1", "--next"]).is_err());
    }

    // --- `story block`/`story unblock` --on (SH-398) ---

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn block_with_only_a_reason_is_unchanged_from_before_sh_398() {
        let invocation =
            parse_invocation(&words(&["block", "SH-1", "waiting", "on", "SH-2"])).unwrap();
        match invocation {
            Invocation::SetAwaiting { id, awaiting, on } => {
                assert_eq!(id, "SH-1");
                assert_eq!(awaiting.as_deref(), Some("waiting on SH-2"));
                assert!(on.is_empty());
            }
            other => panic!("expected SetAwaiting, got {other:?}"),
        }
    }

    #[test]
    fn block_with_only_on_sets_no_reason() {
        let invocation = parse_invocation(&words(&["block", "SH-1", "--on", "SH-2"])).unwrap();
        match invocation {
            Invocation::SetAwaiting { id, awaiting, on } => {
                assert_eq!(id, "SH-1");
                assert_eq!(awaiting, None);
                assert_eq!(on, vec!["SH-2".to_string()]);
            }
            other => panic!("expected SetAwaiting, got {other:?}"),
        }
    }

    #[test]
    fn block_with_repeated_on_and_a_reason_carries_both() {
        let invocation = parse_invocation(&words(&[
            "block", "SH-1", "--on", "SH-2", "--on", "SH-3", "needs", "both",
        ]))
        .unwrap();
        match invocation {
            Invocation::SetAwaiting { id, awaiting, on } => {
                assert_eq!(id, "SH-1");
                assert_eq!(awaiting.as_deref(), Some("needs both"));
                assert_eq!(on, vec!["SH-2".to_string(), "SH-3".to_string()]);
            }
            other => panic!("expected SetAwaiting, got {other:?}"),
        }
    }

    #[test]
    fn block_with_neither_reason_nor_on_is_a_usage_error() {
        let error = parse_invocation(&words(&["block", "SH-1"])).unwrap_err();
        assert_eq!(error.to_string(), BLOCK_USAGE);
        assert!(error.to_string().contains("block <id> --on <blocker>"));
        assert!(error.to_string().contains("block <id> \"<reason>\""));
    }

    #[test]
    fn block_on_with_no_value_is_a_usage_error() {
        let error = parse_invocation(&words(&["block", "SH-1", "--on"])).unwrap_err();
        assert!(error.to_string().contains("--on requires a value"));
    }

    #[test]
    fn unblock_with_no_on_clears_the_reason() {
        let invocation = parse_invocation(&words(&["unblock", "SH-1"])).unwrap();
        match invocation {
            Invocation::ClearAwaiting { id, on } => {
                assert_eq!(id, "SH-1");
                assert!(on.is_empty());
            }
            other => panic!("expected ClearAwaiting, got {other:?}"),
        }
    }

    #[test]
    fn unblock_with_repeated_on_names_every_blocker() {
        let invocation =
            parse_invocation(&words(&["unblock", "SH-1", "--on", "SH-2", "--on", "SH-3"])).unwrap();
        match invocation {
            Invocation::ClearAwaiting { id, on } => {
                assert_eq!(id, "SH-1");
                assert_eq!(on, vec!["SH-2".to_string(), "SH-3".to_string()]);
            }
            other => panic!("expected ClearAwaiting, got {other:?}"),
        }
    }

    #[test]
    fn unblock_with_a_trailing_word_after_on_is_a_usage_error() {
        // SH-357: a word that lands nowhere is refused, not silently dropped.
        assert!(parse_invocation(&words(&["unblock", "SH-1", "--on", "SH-2", "extra"])).is_err());
    }

    // --- Type subcommand tests ---

    #[test]
    fn type_list() {
        let inv = parse_invocation(&["type".to_string(), "list".to_string()]).unwrap();
        assert!(matches!(
            inv,
            Invocation::Type {
                action: TypeAction::List
            }
        ));
    }

    #[test]
    fn type_add_slug_only() {
        let inv =
            parse_invocation(&["type".to_string(), "add".to_string(), "bug".to_string()]).unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "bug");
                assert_eq!(description, None);
                assert_eq!(emoji, None);
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_add_with_description() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "add".to_string(),
            "epic".to_string(),
            "--description".to_string(),
            "A large body of work".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "epic");
                assert_eq!(description.as_deref(), Some("A large body of work"));
                assert_eq!(emoji, None);
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_add_with_emoji() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "add".to_string(),
            "epic".to_string(),
            "--emoji".to_string(),
            "📚".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Add {
                        slug,
                        description,
                        emoji,
                    },
            } => {
                assert_eq!(slug, "epic");
                assert_eq!(description, None);
                assert_eq!(emoji.as_deref(), Some("📚"));
            }
            other => panic!("expected Type::Add, got {:?}", other),
        }
    }

    #[test]
    fn type_set_description_and_emoji() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--description".to_string(),
            "A defect".to_string(),
            "--emoji".to_string(),
            "🐞".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Set {
                        slug,
                        description,
                        clear_description,
                        emoji,
                        clear_emoji,
                    },
            } => {
                assert_eq!(slug, "bug");
                assert_eq!(description.as_deref(), Some("A defect"));
                assert!(!clear_description);
                assert_eq!(emoji.as_deref(), Some("🐞"));
                assert!(!clear_emoji);
            }
            other => panic!("expected Type::Set, got {:?}", other),
        }
    }

    #[test]
    fn type_set_no_emoji_clears_it() {
        let inv = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--no-emoji".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Type {
                action:
                    TypeAction::Set {
                        emoji, clear_emoji, ..
                    },
            } => {
                assert_eq!(emoji, None);
                assert!(clear_emoji);
            }
            other => panic!("expected Type::Set, got {:?}", other),
        }
    }

    #[test]
    fn type_set_emoji_and_no_emoji_contradict() {
        let result = parse_invocation(&[
            "type".to_string(),
            "set".to_string(),
            "bug".to_string(),
            "--emoji".to_string(),
            "🐞".to_string(),
            "--no-emoji".to_string(),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn type_remove() {
        let inv = parse_invocation(&["type".to_string(), "remove".to_string(), "bug".to_string()])
            .unwrap();
        match inv {
            Invocation::Type {
                action: TypeAction::Remove { slug },
            } => {
                assert_eq!(slug, "bug");
            }
            other => panic!("expected Type::Remove, got {:?}", other),
        }
    }

    #[test]
    fn type_no_subcommand_errors() {
        let result = parse_invocation(&["type".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_unknown_subcommand_errors() {
        let result = parse_invocation(&["type".to_string(), "rename".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_add_missing_slug_errors() {
        let result = parse_invocation(&["type".to_string(), "add".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn type_remove_missing_slug_errors() {
        let result = parse_invocation(&["type".to_string(), "remove".to_string()]);
        assert!(result.is_err());
    }

    // --- Epic subcommand tests ---

    #[test]
    fn epic_list() {
        let inv = parse_invocation(&["epic".to_string(), "list".to_string()]).unwrap();
        assert!(matches!(
            inv,
            Invocation::Epic {
                action: EpicAction::List
            }
        ));
    }

    #[test]
    fn epic_show() {
        let inv = parse_invocation(&["epic".to_string(), "show".to_string(), "SH-1".to_string()])
            .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Show { id },
            } => {
                assert_eq!(id, "SH-1");
            }
            other => panic!("expected Epic::Show, got {:?}", other),
        }
    }

    #[test]
    fn epic_create() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "create".to_string(),
            "My Epic Title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Create { title },
            } => {
                assert_eq!(title, "My Epic Title");
            }
            other => panic!("expected Epic::Create, got {:?}", other),
        }
    }

    #[test]
    fn epic_create_multi_word() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "create".to_string(),
            "My".to_string(),
            "Epic".to_string(),
            "Title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Create { title },
            } => {
                assert_eq!(title, "My Epic Title");
            }
            other => panic!("expected Epic::Create, got {:?}", other),
        }
    }

    #[test]
    fn epic_add() {
        let inv = parse_invocation(&[
            "epic".to_string(),
            "add".to_string(),
            "SH-1".to_string(),
            "SH-2".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::Epic {
                action: EpicAction::Add { epic_id, story_id },
            } => {
                assert_eq!(epic_id, "SH-1");
                assert_eq!(story_id, "SH-2");
            }
            other => panic!("expected Epic::Add, got {:?}", other),
        }
    }

    #[test]
    fn epic_no_subcommand_errors() {
        let result = parse_invocation(&["epic".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_unknown_subcommand_errors() {
        let result = parse_invocation(&["epic".to_string(), "rename".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_show_missing_id_errors() {
        let result = parse_invocation(&["epic".to_string(), "show".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_create_missing_title_errors() {
        let result = parse_invocation(&["epic".to_string(), "create".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn epic_add_missing_story_id_errors() {
        let result = parse_invocation(&["epic".to_string(), "add".to_string(), "SH-1".to_string()]);
        assert!(result.is_err());
    }

    // --- --type flag on new/list/set ---

    #[test]
    fn new_with_type_flag() {
        let inv = parse_invocation(&[
            "new".to_string(),
            "My story".to_string(),
            "--type".to_string(),
            "bug".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::New {
                title,
                state,
                story_type,
                ..
            } => {
                assert_eq!(title, "My story");
                assert_eq!(state, None);
                assert_eq!(story_type.as_deref(), Some("bug"));
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_with_state_and_type() {
        let inv = parse_invocation(&[
            "new".to_string(),
            "My story".to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--type".to_string(),
            "epic".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::New {
                title,
                state,
                story_type,
                ..
            } => {
                assert_eq!(title, "My story");
                assert_eq!(state.as_deref(), Some("open"));
                assert_eq!(story_type.as_deref(), Some("epic"));
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_without_type_flag() {
        let inv = parse_invocation(&["new".to_string(), "My story".to_string()]).unwrap();
        match inv {
            Invocation::New { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    // --- --blocked-by on new (SH-779) ---

    #[test]
    fn new_collects_every_blocked_by_in_order_and_keeps_the_title() {
        let args: Vec<String> = [
            "new",
            "Wait",
            "--blocked-by",
            "SH-2",
            "for",
            "--blocked-by",
            "5",
            "it",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        match parse_invocation(&args).unwrap() {
            Invocation::New {
                title, blocked_by, ..
            } => {
                assert_eq!(title, "Wait for it");
                assert_eq!(blocked_by, vec!["SH-2".to_string(), "5".to_string()]);
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_without_blocked_by_names_no_blocker() {
        match parse_invocation(&["new".to_string(), "Free".to_string()]).unwrap() {
            Invocation::New { blocked_by, .. } => assert!(blocked_by.is_empty()),
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn new_blocked_by_without_a_value_is_a_usage_error_naming_the_flag() {
        let error = parse_invocation(&[
            "new".to_string(),
            "Title".to_string(),
            "--blocked-by".to_string(),
        ])
        .unwrap_err();
        assert!(
            matches!(&error, AppError::Usage(message) if message.contains("--blocked-by <id>")),
            "got {error:?}"
        );
    }

    #[test]
    fn list_with_type_flag() {
        let inv = parse_invocation(&["list".to_string(), "--type".to_string(), "epic".to_string()])
            .unwrap();
        match inv {
            Invocation::List { story_type, .. } => {
                assert_eq!(story_type.as_deref(), Some("epic"));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn list_without_type_flag() {
        let inv = parse_invocation(&["list".to_string()]).unwrap();
        match inv {
            Invocation::List { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn set_with_type_flag() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--type".to_string(),
            "bug".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields { id, story_type, .. } => {
                assert_eq!(id, "SH-1");
                assert_eq!(story_type.as_deref(), Some("bug"));
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    #[test]
    fn set_with_type_and_other_fields() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--title".to_string(),
            "New title".to_string(),
            "--type".to_string(),
            "feature".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields {
                id,
                title,
                story_type,
                ..
            } => {
                assert_eq!(id, "SH-1");
                assert_eq!(title.as_deref(), Some("New title"));
                assert_eq!(story_type.as_deref(), Some("feature"));
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    #[test]
    fn set_without_type_flag() {
        let inv = parse_invocation(&[
            "set".to_string(),
            "SH-1".to_string(),
            "--title".to_string(),
            "New title".to_string(),
        ])
        .unwrap();
        match inv {
            Invocation::SetFields { story_type, .. } => {
                assert_eq!(story_type, None);
            }
            other => panic!("expected SetFields, got {:?}", other),
        }
    }

    mod flag_shape {
        use super::super::{VERB_FLAGS, declared_flags, is_flag_shaped, reject_unknown_flags};

        fn argv(words: &[&str]) -> Vec<String> {
            words.iter().map(|word| word.to_string()).collect()
        }

        /// The shape rule, case by case. Each row is a decision the gate makes
        /// before any verb sees the token.
        #[test]
        fn shape_distinguishes_a_flag_from_data() {
            for flag in [
                "--typo",
                "--dry-run",
                "--state=todo",
                "--x",
                "--a1",
                "--json={}",
            ] {
                assert!(is_flag_shaped(flag), "`{flag}` is shaped like a flag");
            }

            for data in [
                // The constraint-(a) case: quoted prose is one argv token with
                // a space in it, so it can never be mistaken for a flag.
                "--fix the ingest path",
                "-- leading terminator then text",
                // The terminator itself, and a pasted rule line.
                "--",
                "---",
                "-----",
                // Single dashes are out of scope by the council's decision.
                "-h",
                "-5",
                "-",
                "-typo",
                // Not a flag name: must start with a letter.
                "--1st",
                "---dash",
                "--=value",
                // Ordinary data.
                "SH-1",
                "todo",
                "",
            ] {
                assert!(!is_flag_shaped(data), "`{data}` is data, not a flag");
            }
        }

        /// The reported defect, at the level the gate decides it.
        #[test]
        fn an_undeclared_flag_is_refused() {
            let error = reject_unknown_flags(&argv(&["new", "--typo", "x"]))
                .expect_err("`story new --typo x` must be refused");
            let message = error.to_string();
            assert!(message.contains("--typo"), "names the token: {message}");
            assert!(message.contains("story new"), "names the verb: {message}");
            assert!(
                message.contains("--priority"),
                "lists the verb's real flags: {message}"
            );
            assert!(
                message.contains("--"),
                "offers the terminator escape: {message}"
            );
        }

        /// Every currently-valid invocation still parses. The gate may only
        /// reject what a parser would have swallowed.
        #[test]
        fn a_declared_flag_is_left_alone() {
            for invocation in [
                vec!["new", "A title", "--priority", "high"],
                vec!["new", "A title", "--labels", "a,b", "--type", "bug"],
                vec!["list", "--ready", "--state", "todo"],
                vec!["move", "SH-1", "done", "--if-state", "in-progress"],
                vec!["set", "SH-1", "--title", "New", "--unblocked"],
                vec!["state", "add", "review", "--super", "OPEN"],
                vec!["state", "set", "review", "--no-description"],
                vec!["state", "remove", "review", "--move-stories-to", "todo"],
                // Both retired verbs, and their entries are what makes the
                // redirect — rather than a complaint about a flag — what a
                // user meets. See the comment on `VERB_FLAGS`.
                vec!["project", "init", "--prefix", "AB", "--no-agents-md"],
                vec!["project", "deinit", "--force"],
                vec!["project", "new", "--prefix", "AB", "--no-agents-md"],
                vec!["project", "delete", "--force"],
                vec!["type", "add", "spike", "--description", "text"],
                vec!["graph", "--blocked-by", "SH-1"],
                vec!["help", "--compact"],
                vec!["decompose", "--stdin", "--dry-run"],
                // Subcommands spelled as flags, which the spawner execs.
                vec!["daemon", "--serve", "--port", "0"],
                vec!["web", "--serve", "--port", "0"],
                vec!["daemon", "start", "--port", "3456"],
            ] {
                reject_unknown_flags(&argv(&invocation))
                    .unwrap_or_else(|error| panic!("`story {}`: {error}", invocation.join(" ")));
            }
        }

        /// A flag's value is that flag's business, never the gate's — so the
        /// one flag in this grammar whose value may begin with dashes keeps
        /// working.
        #[test]
        fn the_token_after_a_value_taking_flag_is_never_judged() {
            reject_unknown_flags(&argv(&["new", "t", "--description", "--odd"]))
                .expect("a value-taking flag's value is not judged");
            reject_unknown_flags(&argv(&["new", "t", "--labels", "--weird"]))
                .expect("likewise for --labels");
        }

        /// `--` ends option scanning, which is what makes a dash-leading value
        /// expressible at all.
        #[test]
        fn a_terminator_ends_the_scan() {
            reject_unknown_flags(&argv(&["new", "--", "--typo", "x"]))
                .expect("everything after `--` is data");
            reject_unknown_flags(&argv(&["comment", "SH-1", "--", "--json", "is", "great"]))
                .expect("likewise in a comment");
        }

        /// The gate declines to speak about a verb that does not exist, so a
        /// mistyped command still reports the command rather than its flags.
        #[test]
        fn an_unrecognized_verb_is_left_to_dispatch() {
            reject_unknown_flags(&argv(&["frobnicate", "--typo"]))
                .expect("an unknown command is dispatch's to report");
            reject_unknown_flags(&argv(&["--help"])).expect("a global flag is not a verb");
            reject_unknown_flags(&argv(&["-V"])).expect("nor is a version request");
        }

        /// Fail-closed: a verb with no table entry refuses every flag-shaped
        /// token rather than inheriting the defect this gate exists to remove.
        #[test]
        fn a_verb_that_declares_nothing_refuses_every_flag() {
            for invocation in [
                vec!["comment", "SH-1", "--typo"],
                vec!["block", "SH-1", "--typo"],
                vec!["label", "SH-1", "--typo"],
                vec!["epic", "create", "--typo"],
                vec!["search", "--typo"],
                vec!["show", "--typo"],
            ] {
                let refused = reject_unknown_flags(&argv(&invocation));
                assert!(
                    refused.is_err(),
                    "`story {}` must be refused",
                    invocation.join(" ")
                );
            }
        }

        /// Every entry in the table is reachable: a two-token entry must name a
        /// verb that exists, so a typo in the table is caught here rather than
        /// by a flag silently ceasing to work.
        #[test]
        fn every_table_entry_names_a_real_verb() {
            for entry in VERB_FLAGS {
                let mut path = vec![entry.command.names()[0].to_string()];
                if let Some(sub) = entry.subcommand {
                    path.push(sub.to_string());
                }
                let found = declared_flags(&path).expect("the entry resolves");
                assert_eq!(
                    found.len(),
                    entry.flags.len(),
                    "`story {}` resolved to a different entry than its own",
                    path.join(" ")
                );
            }
        }

        /// A second token that is an *argument* must not be read as a
        /// subcommand — the bug that would silently disarm `move`'s only flag.
        #[test]
        fn an_argument_in_the_subcommand_slot_falls_back_to_the_verb() {
            let flags = declared_flags(&argv(&["move", "SH-1", "done"])).expect("move declares");
            assert!(flags.iter().any(|flag| flag.name == "if-state"));
        }
    }

    /// `story link-pr` / `story unlink-pr` / `story pr-check` / `story
    /// github-auth` — SH-49, SH-212.
    mod pr_link {
        use super::super::{Invocation, parse_invocation};

        const URL: &str = "https://github.com/acme/widgets/pull/7";

        fn parse(args: &[&str]) -> Result<Invocation, crate::error::AppError> {
            parse_invocation(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
        }

        #[test]
        fn link_pr_defaults_close_on_merge_to_true() {
            assert_eq!(
                parse(&["link-pr", "SH-1", URL]).expect("parses"),
                Invocation::LinkPr {
                    id: "SH-1".to_string(),
                    url: URL.to_string(),
                    close_on_merge: true,
                }
            );
        }

        #[test]
        fn link_pr_no_close_on_merge_flips_the_default() {
            assert_eq!(
                parse(&["link-pr", "SH-1", URL, "--no-close-on-merge"]).expect("parses"),
                Invocation::LinkPr {
                    id: "SH-1".to_string(),
                    url: URL.to_string(),
                    close_on_merge: false,
                }
            );
        }

        #[test]
        fn link_pr_with_no_arguments_is_a_usage_error() {
            let error = parse(&["link-pr"]).expect_err("refuses");
            assert!(
                error.to_string().contains("usage: story link-pr"),
                "{error}"
            );
        }

        #[test]
        fn link_pr_with_only_an_id_is_a_usage_error() {
            assert!(parse(&["link-pr", "SH-1"]).is_err());
        }

        #[test]
        fn link_pr_with_an_unknown_flag_is_a_usage_error() {
            assert!(parse(&["link-pr", "SH-1", URL, "--bogus"]).is_err());
        }

        #[test]
        fn link_pr_with_too_many_arguments_is_a_usage_error() {
            assert!(parse(&["link-pr", "SH-1", URL, "--no-close-on-merge", "extra"]).is_err());
        }

        #[test]
        fn unlink_pr_parses_id_and_url() {
            assert_eq!(
                parse(&["unlink-pr", "SH-1", URL]).expect("parses"),
                Invocation::UnlinkPr {
                    id: "SH-1".to_string(),
                    url: URL.to_string(),
                }
            );
        }

        #[test]
        fn unlink_pr_with_wrong_argument_count_is_a_usage_error() {
            assert!(parse(&["unlink-pr"]).is_err());
            assert!(parse(&["unlink-pr", "SH-1"]).is_err());
            assert!(parse(&["unlink-pr", "SH-1", URL, "extra"]).is_err());
        }

        #[test]
        fn pr_check_with_no_id_checks_every_story() {
            assert_eq!(
                parse(&["pr-check"]).expect("parses"),
                Invocation::PrCheck { id: None }
            );
        }

        #[test]
        fn pr_check_with_an_id_checks_one_story() {
            assert_eq!(
                parse(&["pr-check", "SH-1"]).expect("parses"),
                Invocation::PrCheck {
                    id: Some("SH-1".to_string()),
                }
            );
        }

        #[test]
        fn pr_check_with_too_many_arguments_is_a_usage_error() {
            assert!(parse(&["pr-check", "SH-1", "extra"]).is_err());
        }
    }
}

#[cfg(test)]
mod landing_release_tests {
    use super::*;
    #[test]
    fn sh842_landing_release_cli_requires_exact_id_and_reason() {
        // Exercise the public flag gate as well as the landing parser. Calling
        // parse_verifier directly hid an undeclared --reason from this detector.
        let parse = |s: &[&str]| {
            let args = s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            let (_, args) = split_global_flags(&args)?;
            parse_invocation(&args)
        };
        const INTENT: &str = "a2cb702b-12e8-46c4-831b-c78bf57e944b";
        assert!(matches!(
            parse(&["verifier", "landing", "show"]).unwrap(),
            Invocation::Verifier {
                action: VerifierAction::LandingShow
            }
        ));
        for json in [false, true] {
            let mut args = vec![
                "verifier",
                "landing",
                "release",
                INTENT,
                "--reason",
                "checked rejection",
            ];
            if json {
                args.push("--json");
            }
            assert!(
                matches!(parse(&args).unwrap(), Invocation::Verifier { action: VerifierAction::LandingRelease { intent_id, reason } } if intent_id == INTENT && reason == "checked rejection")
            );
        }
        for args in [
            vec!["verifier", "landing", "release"],
            vec!["verifier", "landing", "release", INTENT],
            vec!["verifier", "landing", "release", INTENT, "--reason"],
            vec!["verifier", "landing", "release", INTENT, "--reason", " "],
            vec![
                "verifier", "landing", "release", INTENT, "--reason", "reason", "--force",
            ],
            vec!["verifier", "landing", "show", "extra"],
            vec!["verifier", "landing", "show", "--reason", "reason"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn landing_reason_flag_is_scoped_and_other_flags_still_refuse() {
        for args in [
            vec!["verifier", "status", "--reason", "reason"],
            vec!["verifier", "start", "--reason", "reason"],
            vec!["verifier", "stop", "--reason", "reason"],
            vec!["verifier", "drain", "--reason", "reason"],
            vec!["verifier", "ack", "incident-1", "--reason", "reason"],
            vec!["verifier", "repair", "show", "--reason", "reason"],
            vec!["verifier", "landing", "show", "--force"],
        ] {
            let args = args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            let error = parse_invocation(&args).unwrap_err().to_string();
            assert!(error.contains("unknown flag"), "{args:?}: {error}");
        }
    }
}
