#!/usr/bin/env bash
#
# Cut a storyhook release.
#
# Two modes, and the difference is whether anything leaves this machine:
#
#   ./scripts/release.sh --bump minor      # public: PR, merge, tag, GitHub release
#   ./scripts/release.sh --local-only      # dogfood: build, install, relaunch. Nothing pushed.
#
# PUBLIC mode is the full path a user's `install.sh` one-liner and `story
# update` both resolve: bump VERSION on a release branch, open a PR (main is
# protected org-wide, so nothing reaches it any other way), merge it with a
# merge commit, tag `main`, push the tag. Pushing a `v*` tag is what triggers
# `.github/workflows/release.yml`, which builds four targets and publishes the
# release. This script then WAITS for that workflow and verifies all four
# assets actually landed before it reports success — a release missing one
# platform's asset is a 404 for that platform's installer on a release that
# looks complete (SH-259).
#
# LOCAL-ONLY mode builds what is checked out right now, installs the binary,
# refreshes the Claude Code plugin from this working copy, and relaunches the
# daemon so it is actually running the build you just made. It does not touch
# VERSION, does not tag, does not push, and never talks to GitHub.
#
# WHAT THIS SCRIPT WILL NOT DO, deliberately:
#
#   - Run from a linked worktree. Version bumps and releases happen from the
#     main checkout only; `semver` and `deployit` already hard-refuse there and
#     this refuses for the same reason. Override with
#     STORYHOOK_RELEASE_ALLOW_WORKTREE=1 if you genuinely mean it.
#   - Push to `main` directly, or force-push anything, ever.
#   - Squash or rebase-merge. The org allows merge commits only.
#   - Skip the gate in public mode. `--skip-gate` is accepted ONLY with
#     --local-only, because an unreleased build you are dogfooding is yours to
#     break and a published one is not.
#   - Move a tag that already exists.
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Parameters
# ---------------------------------------------------------------------------

REPO="mikeydotio/storyhook"
MARKETPLACE="storyhook"          # `.claude-plugin/marketplace.json`'s `name`
PLUGIN="story"                   # `plugin/claude-code/.claude-plugin/plugin.json`'s `name`
PLUGIN_DIR="plugin/claude-code"

# The four artifacts `release.yml`'s matrix builds. Named here so the
# post-publish check can insist on all of them rather than trusting that a
# green workflow means a complete release.
ARTIFACTS=(
  "story-x86_64-unknown-linux-gnu.tar.gz"
  "story-aarch64-unknown-linux-gnu.tar.gz"
  "story-x86_64-apple-darwin.tar.gz"
  "story-aarch64-apple-darwin.tar.gz"
)

local_only=0
bump=""
skip_gate=0
assume_yes=0
dry_run=0
skip_plugin=0
skip_daemon=0

# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

if [ -t 1 ]; then
  bold=$'\033[1m'; red=$'\033[31m'; green=$'\033[32m'; yellow=$'\033[33m'; dim=$'\033[2m'; reset=$'\033[0m'
else
  bold=""; red=""; green=""; yellow=""; dim=""; reset=""
fi

step() { printf '\n%s==>%s %s%s%s\n' "$green" "$reset" "$bold" "$*" "$reset"; }
info() { printf '    %s\n' "$*"; }
note() { printf '    %s%s%s\n' "$dim" "$*" "$reset"; }
warn() { printf '%swarning:%s %s\n' "$yellow" "$reset" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$red" "$reset" "$*" >&2; exit 1; }

run() {
  if [ "$dry_run" = 1 ]; then
    printf '    %s[dry-run]%s %s\n' "$dim" "$reset" "$*"
    return 0
  fi
  "$@"
}

confirm() {
  [ "$assume_yes" = 1 ] && return 0
  [ "$dry_run" = 1 ] && return 0
  printf '\n%s%s%s [y/N] ' "$bold" "$1" "$reset"
  read -r reply </dev/tty || die "no tty to confirm on; re-run with --yes if you mean it"
  case "$reply" in [yY]*) return 0 ;; *) die "aborted" ;; esac
}

usage() {
  sed -n '2,38p' "$0" | sed 's/^#\{1,2\} \{0,1\}//'
  cat <<'EOF'

Usage:
  scripts/release.sh --bump <major|minor|patch> [options]
  scripts/release.sh --local-only [options]

Options:
  --local-only      Build, install, refresh the plugin and relaunch the daemon.
                    Nothing is tagged, pushed or published.
  --bump LEVEL      major | minor | patch. Required for a public release.
                    Optional (and unusual) with --local-only.
  --skip-gate       Skip `make test`. Only permitted with --local-only.
  --skip-plugin     Local mode: leave the Claude Code plugin alone.
  --skip-daemon     Local mode: do not stop or start the daemon.
  --dry-run         Print every command instead of running it.
  --yes             Do not prompt for confirmation.
  -h, --help        This text.

Environment:
  STORYHOOK_RELEASE_ALLOW_WORKTREE=1   Permit running from a linked worktree.
  INSTALL_DIR                          Passed through to `make install`.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --local-only) local_only=1; shift ;;
    --bump) bump="${2:-}"; [ -n "$bump" ] || die "--bump needs a level"; shift 2 ;;
    --bump=*) bump="${1#*=}"; shift ;;
    --skip-gate) skip_gate=1; shift ;;
    --skip-plugin) skip_plugin=1; shift ;;
    --skip-daemon) skip_daemon=1; shift ;;
    --dry-run) dry_run=1; shift ;;
    --yes|-y) assume_yes=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument \`$1\`. Try --help." ;;
  esac
done

# An argument that lands nowhere is refused rather than dropped (SH-357's
# doctrine, applied to this script's own flags).
if [ -n "$bump" ]; then
  case "$bump" in
    major|minor|patch) ;;
    *) die "--bump takes major, minor or patch, not \`$bump\`" ;;
  esac
fi

if [ "$local_only" = 0 ] && [ -z "$bump" ]; then
  die "a public release needs --bump <major|minor|patch>. For a dogfood build use --local-only."
fi

if [ "$skip_gate" = 1 ] && [ "$local_only" = 0 ]; then
  die "--skip-gate is only allowed with --local-only. A published release runs the gate."
fi

# ---------------------------------------------------------------------------
# Preflight — everything cheap that can refuse, before anything irreversible
# ---------------------------------------------------------------------------

step "Preflight"

command -v git >/dev/null || die "git is not on PATH"
command -v cargo >/dev/null || die "cargo is not on PATH"
command -v make >/dev/null || die "make is not on PATH"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || die "not inside a git repository"
cd "$repo_root"

# A linked worktree's `--git-dir` points inside the main checkout's
# `.git/worktrees/`, while `--git-common-dir` names the shared one; they differ
# in a worktree and agree in the main checkout. Releases and version bumps
# happen in the main checkout only.
if [ "$(git rev-parse --git-dir)" != "$(git rev-parse --git-common-dir)" ]; then
  if [ "${STORYHOOK_RELEASE_ALLOW_WORKTREE:-0}" != "1" ]; then
    die "this is a linked worktree. Release from the main checkout, or set STORYHOOK_RELEASE_ALLOW_WORKTREE=1."
  fi
  warn "running from a linked worktree because STORYHOOK_RELEASE_ALLOW_WORKTREE=1"
fi

[ -f VERSION ] || die "no VERSION file at $repo_root"
current_version="$(tr -d '[:space:]' < VERSION)"
info "repository   $repo_root"
info "version      $current_version"
info "branch       $(git rev-parse --abbrev-ref HEAD)"
info "mode         $([ "$local_only" = 1 ] && echo 'local-only (nothing published)' || echo 'PUBLIC release')"

if [ -n "$(git status --porcelain)" ]; then
  git status --short | sed 's/^/    /'
  die "working tree is dirty. Commit or stash first — a release must describe a tree that exists."
fi

# The two manifests carry the plugin's version string twice, and nothing in the
# build makes them agree. `claude plugin validate` checks exactly that, so use
# it rather than re-implementing the comparison here (SH-136: this project has
# paid three times for hand-copied constants that drifted).
if command -v claude >/dev/null 2>&1; then
  if [ "$dry_run" = 0 ]; then
    claude plugin validate "$PLUGIN_DIR" >/dev/null 2>&1 \
      || die "\`claude plugin validate $PLUGIN_DIR\` failed. Fix the manifest before releasing."
    note "plugin manifests validate"
  fi
else
  warn "the \`claude\` CLI is not on PATH; plugin manifest validation skipped"
  [ "$local_only" = 1 ] && [ "$skip_plugin" = 0 ] \
    && die "--local-only installs the plugin and needs the \`claude\` CLI. Use --skip-plugin to omit it."
fi

if [ "$local_only" = 0 ]; then
  command -v gh >/dev/null || die "gh is not on PATH, and a public release needs it"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated. Run \`gh auth login\`."

  branch="$(git rev-parse --abbrev-ref HEAD)"
  [ "$branch" = "main" ] || die "public releases are cut from main; you are on \`$branch\`"

  git fetch --quiet origin main
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
    || die "local main and origin/main disagree. Pull (or push your merges) first."
fi

# ---------------------------------------------------------------------------
# Work out the version being released
# ---------------------------------------------------------------------------

next_version="$current_version"

# The bump is owned by `semver-cli`, which ships inside the Claude Code plugin
# cache rather than on PATH. Resolve it explicitly instead of assuming a bare
# `semver` exists: on this machine it does not, and a script that discovered
# that only after cutting a release branch would strand one.
SEMVER_CLI="${SEMVER_CLI:-}"
if [ -z "$SEMVER_CLI" ]; then
  if command -v semver-cli >/dev/null 2>&1; then
    SEMVER_CLI="$(command -v semver-cli)"
  elif command -v semver >/dev/null 2>&1; then
    SEMVER_CLI="$(command -v semver)"
  else
    # Newest cached plugin version wins; `sort -V` so 3.7.4 beats 2.39.1.
    SEMVER_CLI="$(ls -d "$HOME"/.claude/plugins/cache/agentics/semver/*/bin/semver-cli 2>/dev/null \
      | sort -V | tail -1)"
  fi
fi

if [ -n "$bump" ]; then
  { [ -n "$SEMVER_CLI" ] && [ -x "$SEMVER_CLI" ]; } \
    || die "--bump needs semver-cli. Set SEMVER_CLI=/path/to/semver-cli, or install the semver plugin."
  note "semver-cli   $SEMVER_CLI"
fi

if [ "$local_only" = 0 ]; then
  # Compute what the bump will produce, so the changelog and tag checks below
  # can refuse BEFORE anything is written. `semver-cli` has no "what would the
  # next version be" query -- `recommend` answers a different question (which
  # LEVEL to bump) -- so the arithmetic happens here, against the prefix
  # `semver-cli current` reports rather than a hardcoded "v".
  prefix="$("$SEMVER_CLI" current 2>/dev/null \
    | sed -n 's/.*"version_prefix"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$prefix" ] || prefix="v"

  base="${current_version#"$prefix"}"
  case "$base" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) die "VERSION reads \`$current_version\`, which is not <prefix>MAJOR.MINOR.PATCH. Refusing to guess." ;;
  esac
  IFS='.' read -r major minor patch <<EOF
$base
EOF
  case "$bump" in
    major) major=$((major + 1)); minor=0; patch=0 ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    patch) patch=$((patch + 1)) ;;
  esac
  next_version="${prefix}${major}.${minor}.${patch}"

  info "releasing    $current_version -> $next_version"

  # A tag that already exists is never moved. Refuse early and loudly.
  if git rev-parse -q --verify "refs/tags/$next_version" >/dev/null; then
    die "tag $next_version already exists locally. Tags are never moved here."
  fi
  if git ls-remote --exit-code --tags origin "refs/tags/$next_version" >/dev/null 2>&1; then
    die "tag $next_version already exists on origin. Tags are never moved here."
  fi

  # `render-release-body.sh` requires a changelog section for the version, and
  # the workflow runs it BEFORE publishing — a body that cannot be rendered on
  # a tag push leaves a permanently dangling tag. Check it here, where failing
  # is free.
  if [ -f scripts/render-release-body.sh ] && [ "$dry_run" = 0 ]; then
    if ! scripts/render-release-body.sh --version "$next_version" --repo "$REPO" >/dev/null 2>&1; then
      warn "no changelog section for $next_version yet."
      note "\`semver bump\` writes it, so this is expected before the bump and fatal after."
    fi
  fi
fi

# ---------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------

if [ "$skip_gate" = 1 ]; then
  warn "skipping \`make test\`. This build is not gated."
else
  step "Gate — make test"
  note "this also mints the push receipt .githooks/pre-push verifies"
  run make test
fi

# ---------------------------------------------------------------------------
# LOCAL-ONLY
# ---------------------------------------------------------------------------

if [ "$local_only" = 1 ]; then
  if [ -n "$bump" ]; then
    step "Bumping version (local only — this WILL modify VERSION and CHANGELOG.md)"
    warn "you asked for --bump with --local-only; the bump is committed locally and never pushed"
    confirm "Bump $current_version by $bump without publishing?"
    # --skip-tag: a tag is a claim about a published release. Minting one for a
    # build that never leaves this machine would put a tag in the repository
    # that no GitHub release corresponds to, which is the dangling-tag state
    # release.yml's own notes step exists to avoid.
    run "$SEMVER_CLI" bump run "$bump" --skip-tag --non-interactive
    current_version="$(tr -d '[:space:]' < VERSION)"
  fi

  step "Building and installing the binary"
  # The daemon is stopped BEFORE the binary is replaced: a running daemon holds
  # the old executable, answers reads from its own page cache, and would keep
  # serving the build you just replaced. `make install` uses `install(1)`
  # rather than `cp`, which is what keeps this from SIGKILLing a running
  # process on macOS.
  if [ "$skip_daemon" = 0 ]; then
    if story daemon status >/dev/null 2>&1; then
      info "stopping the running daemon first"
      run story daemon stop || warn "\`story daemon stop\` reported a problem; continuing"
    else
      note "no daemon currently running"
    fi
  fi

  run make install

  # Read back what is now on PATH. Under --dry-run nothing was installed, so
  # reporting the version found here would name the OLD build as though it were
  # the new one — a false statement of exactly the kind this project's notice
  # doctrine forbids elsewhere. Say what was not done instead.
  if [ "$dry_run" = 1 ]; then
    installed_version=""
    note "nothing installed (dry run); the running binary is still $(story --version 2>/dev/null | awk '{print $2}')"
  else
    installed_version="$( { command -v story >/dev/null && story --version; } 2>/dev/null | awk '{print $2}')"
    info "installed    story ${installed_version:-unknown}"
  fi

  if [ "$skip_plugin" = 0 ]; then
    step "Refreshing the Claude Code plugin from this working copy"
    # `marketplace add` is idempotent-ish but errors if the name is already
    # registered, so update an existing registration rather than re-adding it.
    if claude plugin marketplace list 2>/dev/null | grep -q "$MARKETPLACE"; then
      info "marketplace \`$MARKETPLACE\` already registered; updating from source"
      run claude plugin marketplace update "$MARKETPLACE"
    else
      info "registering marketplace \`$MARKETPLACE\` from $repo_root"
      run claude plugin marketplace add "$repo_root"
    fi

    if claude plugin list 2>/dev/null | grep -q "$PLUGIN"; then
      run claude plugin update "${PLUGIN}@${MARKETPLACE}" || run claude plugin update "$PLUGIN"
    else
      run claude plugin install "${PLUGIN}@${MARKETPLACE}"
    fi
    note "restart Claude Code for the plugin change to take effect"
  fi

  if [ "$skip_daemon" = 0 ]; then
    step "Relaunching the daemon"
    run story daemon start
    if [ "$dry_run" = 0 ]; then
      # Confirm the PROCESS, not just that a command exited 0 — this project
      # has been bitten by inferring a running process from rendered output
      # (SH-226).
      sleep 1
      story daemon status || die "the daemon did not come back up"
      running="$(story daemon status 2>/dev/null | head -1 | awk '{print $3}')"
      info "daemon reports version ${running:-unknown}"
      if [ -n "$installed_version" ] && [ -n "$running" ] && [ "$running" != "$installed_version" ]; then
        warn "daemon reports $running but the installed binary is $installed_version — version skew"
      fi
    fi
  fi

  step "Local build installed"
  info "Nothing was tagged, pushed or published."
  exit 0
fi

# ---------------------------------------------------------------------------
# PUBLIC RELEASE
# ---------------------------------------------------------------------------

release_branch="release/$next_version"

step "Public release: $current_version -> $next_version"
info "branch       $release_branch"
info "tag          $next_version (pushed to origin, which triggers release.yml)"
info "assets       ${#ARTIFACTS[@]} platform tarballs, all required"
confirm "Cut and PUBLISH $next_version to GitHub?"

step "Bumping the version on a release branch"
# Never on main directly: the org's `protect-main` ruleset forbids it, and the
# bump has to arrive through a PR like everything else.
run git switch -c "$release_branch"
# --skip-tag is load-bearing, not tidiness. `semver-cli bump run` tags by
# default, and it would tag THIS branch's commit -- but the tag has to name the
# merge commit that actually lands on `main`, or `install.sh` resolves a
# release built from a commit that is not on the released branch. The tag is
# created further down, after the merge.
run "$SEMVER_CLI" bump run "$bump" --skip-tag --non-interactive

if [ "$dry_run" = 0 ]; then
  actual="$(tr -d '[:space:]' < VERSION)"
  [ "$actual" = "$next_version" ] \
    || die "semver produced $actual but this script planned $next_version. Refusing to continue."

  # Now that the changelog section exists, the body must render — the workflow
  # will run exactly this and a failure there strands a tag.
  scripts/render-release-body.sh --version "$next_version" --repo "$REPO" >/dev/null \
    || die "the release body will not render for $next_version. Fix CHANGELOG.md before tagging."
  note "release body renders"
fi

# `semver bump` commits its own change; only commit if it left anything.
if [ "$dry_run" = 0 ] && [ -n "$(git status --porcelain)" ]; then
  run git add VERSION CHANGELOG.md
  run git commit -m "chore: release $next_version"
fi

step "Opening the pull request"
# HTTPS with the gh credential helper: SSH auth here goes through 1Password's
# agent, which wants an interactive approval.
run git -c "url.https://github.com/.insteadOf=git@github.com:" push origin "$release_branch"
run gh pr create \
  --base main \
  --head "$release_branch" \
  --title "chore: release $next_version" \
  --body "Version bump and changelog for \`$next_version\`.

Merging this lands the bump on \`main\`; the tag is pushed afterwards by \`scripts/release.sh\`, which is what triggers the release workflow."

step "Merging (merge commit — the only method this org allows)"
run gh pr merge --merge --delete-branch

step "Returning to main"
run git switch main
run git pull --ff-only

if [ "$dry_run" = 0 ]; then
  landed="$(tr -d '[:space:]' < VERSION)"
  [ "$landed" = "$next_version" ] \
    || die "main says $landed after the merge, expected $next_version. Not tagging."
fi

step "Tagging and pushing $next_version"
note "pushing the tag is what starts .github/workflows/release.yml"
run git tag -a "$next_version" -m "$next_version"
run git -c "url.https://github.com/.insteadOf=git@github.com:" push origin "$next_version"

# ---------------------------------------------------------------------------
# Verify the release actually published — completely
# ---------------------------------------------------------------------------

if [ "$dry_run" = 1 ]; then
  step "Dry run complete"
  exit 0
fi

step "Waiting for the release workflow"
info "watching the run triggered by $next_version"
sleep 10
if ! gh run watch --exit-status "$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"; then
  die "the release workflow failed. The tag $next_version exists and was NOT published — see \`gh run list\`."
fi

step "Verifying every platform asset landed"
missing=0
for artifact in "${ARTIFACTS[@]}"; do
  if gh release view "$next_version" --repo "$REPO" --json assets \
      --jq '.assets[].name' 2>/dev/null | grep -qx "$artifact"; then
    info "ok       $artifact"
  else
    printf '    %smissing  %s%s\n' "$red" "$artifact" "$reset"
    missing=$((missing + 1))
  fi
done

[ "$missing" -eq 0 ] \
  || die "$missing asset(s) missing from $next_version. That release is a 404 for those platforms — do not announce it."

step "Released $next_version"
info "https://github.com/$REPO/releases/tag/$next_version"
note "the install one-liner and \`story update\` both resolve /releases/latest, so this is now live"
