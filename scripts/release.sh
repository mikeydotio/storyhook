#!/usr/bin/env bash
#
# Cut a storyhook release.
#
# Three verbs, because CUTTING a version and SHIPPING it are separate
# decisions:
#
#   ./scripts/release.sh --bump minor    # cut v-next, build it everywhere, install it here
#   ./scripts/release.sh --publish       # ship the draft you have been dogfooding
#   ./scripts/release.sh --local-only    # just rebuild and reinstall what is checked out
#
# --bump bumps VERSION on a release branch, opens and merges a PR, builds and
# verifies all four release archives on this machine, tags the release commit,
# and uploads exactly one **draft** release. The tag itself triggers nothing.
# The script verifies GitHub's digest for every uploaded asset before it
# installs the new version locally, so what you dogfood is what would ship.
#
# Nothing is public at that point, and that is the design. `install.sh` and
# `story update` both resolve `/releases/latest`, which GitHub defines as the
# newest release that is neither a draft nor a prerelease. So the local and
# committed version may legitimately run AHEAD of the published one, for as
# long as you want, without anyone downloading it by accident.
#
# --publish is the second decision: it refuses unless every platform asset is
# attached, then flips the draft. That check is the one v2.1.0 never got — its
# build died on one target and the release never happened at all (SH-259).
#
# --local-only rebuilds and reinstalls the checked-out tree without touching
# VERSION, tagging, pushing, or talking to GitHub.
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
#     break and a published one is not. When the gate does run, it is the FULL
#     battery -- `make test-full`, browser suite included (SH-394) -- because
#     this script is the one place that battery is meant to run at all; the
#     ordinary merge gate, `make test`, deliberately does not.
#   - Move a tag that already exists.
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Parameters
# ---------------------------------------------------------------------------

REPO="mikeydotio/storyhook"
MARKETPLACE="storyhook"          # `.claude-plugin/marketplace.json`'s `name`
PLUGIN="story"                   # `plugins/story/.claude-plugin/plugin.json`'s `name`
PLUGIN_DIR="plugins/story"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-targets.sh
source "$SCRIPT_DIR/release-targets.sh"
ARTIFACTS=("${RELEASE_ARTIFACTS[@]}")

local_only=0
bump=""
do_publish=0
publish=""
skip_gate=0
assume_yes=0
dry_run=0
skip_plugin=0
skip_daemon=0
no_install=0

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

# Every release matching a tag, including drafts. Listing rather than using
# `gh release view <tag>` is load-bearing: GitHub can contain two same-tag
# drafts (SH-540), and the tag endpoint chooses one instead of exposing the
# ambiguity that must stop publishing.
releases_for_tag() {
  gh api --paginate "repos/$REPO/releases?per_page=100" \
    --jq ".[] | select(.tag_name == \"$1\") | [.id, .draft] | @tsv"
}

release_assets() {
  gh api --paginate "repos/$REPO/releases/$1/assets?per_page=100" \
    --jq '.[] | [.name, .digest] | @tsv'
}

confirm() {
  [ "$assume_yes" = 1 ] && return 0
  [ "$dry_run" = 1 ] && return 0
  printf '\n%s%s%s [y/N] ' "$bold" "$1" "$reset"
  read -r reply </dev/tty || die "no tty to confirm on; re-run with --yes if you mean it"
  case "$reply" in [yY]*) return 0 ;; *) die "aborted" ;; esac
}

# Build the working tree, put it on PATH, refresh the plugin, and relaunch the
# daemon onto it. Shared by --local-only and by --bump, so the version being
# dogfooded is installed by exactly the same steps whichever way you got here.
install_locally() {
  step "Building and installing the binary"
  # The daemon is stopped BEFORE the binary is replaced: a running daemon holds
  # the old executable, answers reads from its own page cache, and would keep
  # serving the build just replaced. `make install` uses `install(1)` rather
  # than `cp`, which is what keeps this from SIGKILLing a running process on
  # macOS.
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
  # the new one -- a false statement of exactly the kind this project's notice
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
    # `marketplace add` errors if the name is already registered, so update an
    # existing registration rather than re-adding it.
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
      # Confirm the PROCESS, not just that a command exited 0 -- this project
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
}

usage() {
  sed -n '2,38p' "$0" | sed 's/^#\{1,2\} \{0,1\}//'
  cat <<'EOF'

Usage:
  scripts/release.sh --bump <major|minor|patch> [options]   # cut, build, install
  scripts/release.sh --publish [vX.Y.Z] [options]           # ship the draft
  scripts/release.sh --local-only [options]                 # reinstall only

Options:
  --bump LEVEL      major | minor | patch. Cuts the version, assembles all
                    targets locally, uploads a DRAFT, then installs it.
  --publish [VER]   Publish an already-assembled draft (default: VERSION).
                    Refuses unless all four platform assets are attached.
  --local-only      Rebuild and reinstall the checked-out tree. Nothing is
                    tagged, pushed or published.
  --no-install      With --bump: cut the version but do not install it here.
  --skip-gate       Skip `make test-full`. Only permitted with --local-only.
  --skip-plugin     Local mode: leave the Claude Code plugin alone.
  --skip-daemon     Local mode: do not stop or start the daemon.
  --dry-run         Print every command instead of running it.
  --yes             Do not prompt for confirmation.
  -h, --help        This text.

Environment:
  STORYHOOK_RELEASE_ALLOW_WORKTREE=1   Permit running from a linked worktree.
  INSTALL_DIR                          Passed through to `make install`.

Public release requirements:
  rustup stable with both Darwin targets, cross, and a running Docker engine.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --local-only) local_only=1; shift ;;
    --bump) bump="${2:-}"; [ -n "$bump" ] || die "--bump needs a level"; shift 2 ;;
    --bump=*) bump="${1#*=}"; shift ;;
    # Optional value: `--publish` alone means whatever VERSION says. A bare
    # `--publish --yes` must not swallow the next FLAG as its argument, so a
    # value is only consumed when it does not begin with a dash.
    --publish)
      do_publish=1
      case "${2:-}" in
        ""|-*) shift ;;
        *) publish="$2"; shift 2 ;;
      esac
      ;;
    --publish=*) do_publish=1; publish="${1#*=}"; shift ;;
    --no-install) no_install=1; shift ;;
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

# Three verbs, and exactly one of them per invocation.
#
#   --bump      cut a version: PR, merge, local build, tag, and DRAFT upload
#   --publish   ship a draft that already exists
#   --local-only  rebuild and reinstall what is checked out
#
# --bump and --publish are deliberately separate because that separation is
# the whole point: local assembly proves all four platforms and attaches every
# asset, and only then is shipping a decision worth making.
if [ "$do_publish" = 1 ] && { [ "$local_only" = 1 ] || [ -n "$bump" ]; }; then
  die "--publish is its own step. Run it after --bump, once you have decided to ship."
fi

if [ "$local_only" = 0 ] && [ "$do_publish" = 0 ] && [ -z "$bump" ]; then
  die "nothing to do. Use --bump <level> to cut a version, --publish to ship a draft, or --local-only to reinstall."
fi

if [ "$skip_gate" = 1 ] && [ "$local_only" = 0 ] && [ "$do_publish" = 0 ]; then
  die "--skip-gate is only allowed with --local-only. A version that will be shipped runs the full battery (make test-full)."
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

# --------------------------------------------------------------------------
# PUBLISH — flip an already-assembled draft. Its own step, and it ends here.
# --------------------------------------------------------------------------

if [ "$do_publish" = 1 ]; then
  command -v gh >/dev/null || die "gh is not on PATH, and publishing needs it"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated. Run \`gh auth login\`."

  target="${publish:-$current_version}"
  release_version_is_valid "$target" \
    || die "--publish version must be vMAJOR.MINOR.PATCH, got \`$target\`"
  step "Publishing $target"

  releases="$(releases_for_tag "$target")" \
    || die "could not list GitHub releases for $target"
  release_count="$(printf '%s\n' "$releases" | sed '/^$/d' | wc -l | tr -d ' ')"
  [ "$release_count" -gt 0 ] \
    || die "no release $target exists. Run \`--bump\` to assemble its draft locally."
  [ "$release_count" -eq 1 ] \
    || die "$release_count releases exist for $target. Refusing to choose between ambiguous same-tag releases."
  IFS=$'\t' read -r release_id is_draft <<EOF
$releases
EOF

  case "$is_draft" in
    false)
      info "$target is already published."
      note "nothing to do; /releases/latest already resolves it unless a newer one exists"
      exit 0 ;;
  esac
  [ "$is_draft" = "true" ] || die "release $target returned an unknown draft state: $is_draft"

  # A draft is only worth shipping if it is COMPLETE. This is the check that
  # v2.1.0 never got: its build died on one target, so the release it would
  # have published was missing an asset that `install.sh` resolves by name.
  step "Verifying every platform asset is attached before shipping"
  assets="$(release_assets "$release_id")" \
    || die "could not list assets for the $target draft"
  invalid=0
  for artifact in "${ARTIFACTS[@]}"; do
    digests="$(printf '%s\n' "$assets" | awk -F '\t' -v name="$artifact" '$1 == name { print $2 }')"
    matches="$(printf '%s\n' "$digests" | sed '/^$/d' | wc -l | tr -d ' ')"
    if [ "$matches" -ne 1 ]; then
      printf '    %sinvalid  %s (%s matching assets)%s\n' "$red" "$artifact" "$matches" "$reset"
      invalid=$((invalid + 1))
    elif ! printf '%s\n' "$digests" | grep -Eq '^sha256:[0-9a-f]{64}$'; then
      printf '    %sinvalid  %s (missing SHA-256 metadata)%s\n' "$red" "$artifact" "$reset"
      invalid=$((invalid + 1))
    else
      info "ok       $artifact $digests"
    fi
  done
  [ "$invalid" -eq 0 ] \
    || die "$invalid required asset(s) are missing, duplicated, or lack a GitHub SHA-256 digest. Refusing to publish."

  confirm "Publish $target? This is the moment it becomes visible to install.sh and \`story update\`."
  run gh release edit "$target" --repo "$REPO" --draft=false

  if [ "$dry_run" = 0 ]; then
    latest="$(gh release view --repo "$REPO" --json tagName --jq .tagName 2>/dev/null)"
    info "latest release is now ${latest:-unknown}"
    [ "$latest" = "$target" ] \
      || warn "/releases/latest resolves $latest, not $target — a newer release already exists"
  fi

  step "Published $target"
  info "https://github.com/$REPO/releases/tag/$target"
  exit 0
fi

if [ "$local_only" = 0 ]; then
  command -v gh >/dev/null || die "gh is not on PATH, and a public release needs it"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated. Run \`gh auth login\`."

  branch="$(git rev-parse --abbrev-ref HEAD)"
  [ "$branch" = "main" ] || die "public releases are cut from main; you are on \`$branch\`"

  git fetch --quiet origin main
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
    || die "local main and origin/main disagree. Pull (or push your merges) first."

  # This check is intentionally before semver creates a branch or commit. The
  # actual binaries are built after the bumped tree lands on main.
  scripts/build-release-assets.sh --check
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
    # Every matched basename is a semver directory.
    # shellcheck disable=SC2012
    SEMVER_CLI="$(ls -d "$HOME"/.claude/plugins/cache/agentics/semver/*/bin/semver-cli 2>/dev/null \
      | sort -V | tail -1)"
  fi
fi

if [ -n "$bump" ]; then
  { [ -n "$SEMVER_CLI" ] && [ -x "$SEMVER_CLI" ]; } \
    || die "--bump needs semver-cli. Set SEMVER_CLI=/path/to/semver-cli, or install the semver plugin."
  note "semver-cli   $SEMVER_CLI"

  # Ask semver whether it will consent BEFORE the gate, the build and the
  # install — all of which are wasted if it refuses. It reports a refusal as
  # JSON on stdout and exits 0, so the answer has to be read out of the body
  # rather than taken from the exit status.
  #
  # SH-494: this runs under --dry-run too. `validate` is a pure read, and
  # skipping it made a dry run report a clean plan for a release that could not
  # actually start -- which is what a dry run exists to rule out. A rehearsal
  # whose verdict differs from the real run's is worse than no rehearsal.
  if true; then
    validation="$("$SEMVER_CLI" validate 2>/dev/null || true)"
    if printf '%s' "$validation" | grep -q '"status"[[:space:]]*:[[:space:]]*"FAIL"'; then
      printf '%s\n' "$validation" \
        | grep -B2 '"status"[[:space:]]*:[[:space:]]*"FAIL"' \
        | sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/    failing check: \1/p' >&2
      printf '%s\n' "$validation" \
        | sed -n 's/.*"detail"[[:space:]]*:[[:space:]]*"\(Tag [^"]*\)".*/    \1/p' >&2
      die "semver-cli validation fails, so \`bump run\` will refuse.
    A missing tag for the CURRENT version is the usual cause: this repository
    bumps VERSION through a PR and tags afterwards, so a release that never
    finished leaves VERSION ahead of any tag. Fix that first, or pass
    --skip-validation intent explicitly by bumping with semver yourself."
    fi
  fi
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

  existing_releases="$(releases_for_tag "$next_version")" \
    || die "could not check whether a release already exists for $next_version"
  [ -z "$existing_releases" ] \
    || die "a release already exists for $next_version. Refusing to create another same-tag draft."

  # `render-release-body.sh` requires a changelog section for the version.
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
  warn "skipping \`make test-full\`. This build is not gated."
else
  step "Gate — make test-full"
  note "the full battery, including the browser suite (SH-394) -- this also \
mints the push receipt .githooks/pre-push verifies"
  run make test-full
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
    # that no GitHub release corresponds to.
    run "$SEMVER_CLI" bump run "$bump" --skip-tag --non-interactive

    # Verify the bump actually landed, rather than trusting the exit status.
    # `semver-cli` reports a refusal as a JSON body on stdout and STILL EXITS
    # 0 — measured: a `validation_failed` refusal ("Tag vX.Y.Z not found")
    # printed `"error": "non_interactive_blocked"` and returned success, so
    # `set -e` sailed straight past it and this script went on to build,
    # install and relaunch an UNBUMPED tree while reporting a release. The
    # public path below has always checked VERSION for exactly this reason;
    # the local path did not, and that asymmetry was the bug.
    if [ "$dry_run" = 0 ]; then
      bumped="$(tr -d '[:space:]' < VERSION)"
      if [ "$bumped" = "$current_version" ]; then
        die "the bump did not happen — VERSION is still $current_version.
    semver-cli refused (it prints the reason above and still exits 0).
    Run \`semver-cli validate\` to see which check failed; a missing tag for
    the CURRENT version is the usual cause. Nothing was built or installed."
      fi
      current_version="$bumped"
      info "bumped to    $current_version"
    fi
  fi

  install_locally

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
info "tag          $next_version (pushed only after local assembly passes)"
info "assets       ${#ARTIFACTS[@]} platform tarballs, all required"
confirm "Cut and assemble a DRAFT for $next_version on GitHub?"

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

  # Now that the changelog section exists, the body must render before either
  # the local build or the tag can make the release observable.
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

Merging this lands the bump on \`main\`; \`scripts/release.sh\` then assembles and verifies every release asset locally before it pushes the tag."

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

artifact_dir="$repo_root/target/release-assets/$next_version"
step "Building and verifying all release assets locally"
if [ "$dry_run" = 1 ]; then
  run scripts/build-release-assets.sh --version "$next_version" --output-dir "$artifact_dir"
  run scripts/render-release-body.sh --version "$next_version" --repo "$REPO"
else
  scripts/build-release-assets.sh --version "$next_version" --output-dir "$artifact_dir"
  scripts/render-release-body.sh --version "$next_version" --repo "$REPO" \
    > "$artifact_dir/release-body.md"
  [ -s "$artifact_dir/release-body.md" ] || die "the rendered release body is empty"
fi

step "Tagging and pushing $next_version"
note "tag pushes trigger no GitHub Actions workflow"

# SH-494: the tag goes on the commit that MODIFIED VERSION, never on whatever
# HEAD happens to be. At this point HEAD is the merge commit `gh pr merge`
# produced, and the release commit is its parent -- so a bare `git tag -a
# "$next_version"` (which is what this line used to be) tags the merge, and
# `semver-cli`'s `tag_correct_commit` check then fails for the NEXT release
# rather than this one. v2.2.0 shipped that way and v2.3.0 could not begin.
#
# `release-tag-commit.sh` answers by running semver's own query rather than by
# carrying a SHA captured earlier, so the two cannot drift; it refuses rather
# than guessing, and a refusal here is fatal because tagging the wrong commit
# is the entire defect.
release_commit=""
if [ "$dry_run" = 0 ]; then
    release_commit="$(scripts/release-tag-commit.sh "$next_version")" \
        || die "cannot determine which commit to tag for $next_version (see above). \
Nothing has been tagged; the bump is already on main, so fix the cause and tag by hand."
    info "tagging      ${release_commit} ($(git log -1 --format=%s "$release_commit"))"
    if [ "$release_commit" = "$(git rev-parse HEAD)" ]; then
        note "the release commit IS HEAD here — no merge commit intervened"
    fi
else
    release_commit="<release-commit>"
fi
run git tag -a "$next_version" -m "$next_version" "$release_commit"
run git -c "url.https://github.com/.insteadOf=git@github.com:" push origin "$next_version"

asset_paths=()
for artifact in "${ARTIFACTS[@]}"; do
  asset_paths+=("$artifact_dir/$artifact")
done
step "Creating one verified draft"
run gh release create "$next_version" "${asset_paths[@]}" \
  --repo "$REPO" \
  --verify-tag \
  --draft \
  --generate-notes \
  --notes-file "$artifact_dir/release-body.md"

if [ "$dry_run" = 1 ]; then
  step "Dry run complete"
  exit 0
fi

# ---------------------------------------------------------------------------
# Verify exactly one draft and its uploaded bytes
# ---------------------------------------------------------------------------

releases="$(releases_for_tag "$next_version")" \
  || die "the draft was created but could not be read back"
release_count="$(printf '%s\n' "$releases" | sed '/^$/d' | wc -l | tr -d ' ')"
[ "$release_count" -eq 1 ] \
  || die "expected one release for $next_version after creation, found $release_count"
IFS=$'\t' read -r release_id is_draft <<EOF
$releases
EOF
[ "$is_draft" = "true" ] || die "$next_version was not created as a draft"

assets="$(release_assets "$release_id")" \
  || die "the draft was created but its assets could not be read back"
for artifact in "${ARTIFACTS[@]}"; do
  expected="$(awk -v name="$artifact" '$2 == name { print $1 }' "$artifact_dir/SHA256SUMS")"
  actual="$(printf '%s\n' "$assets" | awk -F '\t' -v name="$artifact" '$1 == name { print $2 }')"
  [ "$actual" = "sha256:$expected" ] \
    || die "$artifact digest mismatch: local sha256:$expected, GitHub ${actual:-missing}"
  info "verified $artifact $actual"
done

# Install the version just cut, so the thing being dogfooded is the thing that
# would ship. Skipped with --no-install for a version cut on one machine and
# tried on another.
if [ "$no_install" = 0 ]; then
  install_locally
fi

step "$next_version is cut, built on every platform, and NOT published"
info "draft   https://github.com/$REPO/releases/tag/$next_version"
info "main    in sync at $next_version"
note "install.sh and \`story update\` still resolve the previous release: a draft is"
note "invisible to /releases/latest, which is what lets your local version run ahead."
printf '\n    %sWhen you have dogfooded it and want to ship:%s\n' "$bold" "$reset"
printf '        ./scripts/release.sh --publish\n\n'
