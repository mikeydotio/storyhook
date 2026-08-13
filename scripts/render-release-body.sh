#!/usr/bin/env bash
#
# Renders a release body from CHANGELOG.md, measured from the newest PUBLISHED
# release rather than from the newest tag.
#
# GitHub's generated release notes count from the newest tag, published or not.
# `v2.1.0` was tagged and never published -- its release build failed on one
# target (SH-259) -- so the notes GitHub generates for the release after it
# describe `v2.1.0...v2.1.1`: six pull requests, while a user upgrading from the
# newest release they could actually install receives seven hundred and
# forty-three commits. Those notes are not wrong about the span they describe.
# They describe the wrong span, and nothing on the release page says so
# (SH-257's council, condition 2; SH-262).
#
# So the body is rendered here instead, from the changelog, against the release
# `/releases/latest` resolves -- the same one `install.sh` and `story update`
# fetch, which makes it the version a user actually upgrades FROM. Versions
# tagged in between are named and linked rather than pasted (condition 4), and a
# breaking change anywhere in that span is disclosed scoped to the reader it is
# true for rather than flatly (condition 3).
#
# Every failure exits non-zero with nothing on stdout. The alternative -- an
# empty or partial body -- publishes, and a release body cannot be un-published
# from the feeds that already carried it.
#
# Usage:
#   render-release-body.sh --version vX.Y.Z --repo owner/name [--since vA.B.C]
#                          [--changelog PATH]
#
#   --version    the version being released; must have a changelog section
#   --repo       owner/name, used to anchor links at the released tag
#   --since      the newest PUBLISHED release; omit for a first release
#   --changelog  defaults to this repository's own CHANGELOG.md
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

version=""
repo=""
since=""
changelog="${repo_root}/CHANGELOG.md"

die() {
    echo "render-release-body.sh: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:-}"; shift 2 ;;
        --repo) repo="${2:-}"; shift 2 ;;
        --since) since="${2:-}"; shift 2 ;;
        --changelog) changelog="${2:-}"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ -n "$version" ] || die "--version is required: it decides which changelog section becomes the body"
[ -n "$repo" ] || die "--repo is required: it decides where the changelog links point"
[ -r "$changelog" ] || die "CHANGELOG is unreadable at ${changelog}"

# Every version heading in the file, newest first. The changelog's own order is
# the ordering used throughout -- comparing version numbers would need a semver
# parser and would disagree with the file the humans read.
versions() {
    awk '
        /^## \[/ {
            line = $0
            sub(/^## \[/, "", line)
            sub(/\].*$/, "", line)
            print line
        }
    ' "$changelog"
}

# A version heading with its brackets removed: `v2.1.0 - 2026-08-12`.
heading_of() {
    awk -v want="$1" '
        /^## \[/ {
            line = $0
            sub(/^## \[/, "", line)
            sub(/\].*$/, "", line)
            if (line == want) {
                heading = $0
                sub(/^## /, "", heading)
                gsub(/[][]/, "", heading)
                print heading
                exit
            }
        }
    ' "$changelog"
}

# Everything under a version's heading, up to the next one. The heading itself
# is dropped -- the release page already carries the tag as its title -- and so
# is the blank line beneath it, so a body with no preamble opens on an entry.
section_of() {
    awk -v want="$1" '
        /^## \[/ {
            inside = 0
            line = $0
            sub(/^## \[/, "", line)
            sub(/\].*$/, "", line)
            if (line == want) { inside = 1; started = 0; next }
        }
        inside {
            if (!started) {
                if ($0 ~ /^[[:space:]]*$/) next
                started = 1
            }
            print
        }
    ' "$changelog"
}

# GitHub anchors a heading by lowercasing it, dropping every character that is
# not alphanumeric, a space, a hyphen or an underscore, and turning spaces into
# hyphens: `## [v2.1.0] - 2026-08-12` becomes `#v210---2026-08-12`.
anchor_of() {
    printf '%s' "$1" \
        | tr '[:upper:]' '[:lower:]' \
        | sed 's/[^a-z0-9 _-]//g' \
        | tr ' ' '-'
}

# Position in the file, counting from the top: newest is 1. `0` means the
# version has no section at all, which every caller treats as fatal.
position_of() {
    local wanted="$1" index=0 candidate
    while IFS= read -r candidate; do
        index=$((index + 1))
        if [ "$candidate" = "$wanted" ]; then
            printf '%s' "$index"
            return 0
        fi
    done < <(versions)
    printf '0'
}

release_position="$(position_of "$version")"
[ "$release_position" != "0" ] || die "no changelog section for ${version} in ${changelog}: the version was tagged but never written down"

release_section="$(section_of "$version")"
[ -n "$(printf '%s' "$release_section" | tr -d '[:space:]')" ] || die "the changelog section for ${version} is empty, so this release has nothing to say for itself"

# Versions tagged since the newest published release: everything between the two
# headings. They ship in this release, and generated notes never mention them.
skipped=()
if [ -n "$since" ]; then
    since_position="$(position_of "$since")"
    [ "$since_position" != "0" ] || die "no changelog section for ${since}, the newest published release, so the span this release covers cannot be established"
    if [ "$since_position" -le "$release_position" ]; then
        die "${since} is not older than ${version} in ${changelog}: a release cannot be measured from itself or from something newer"
    fi
    index=0
    while IFS= read -r candidate; do
        index=$((index + 1))
        if [ "$index" -gt "$release_position" ] && [ "$index" -lt "$since_position" ]; then
            skipped+=("$candidate")
        fi
    done < <(versions)
fi

# A breaking change anywhere in the span crosses the upgrade, even when the
# release being cut is a patch: the reader upgrading from `$since` meets every
# one of them at once.
#
# Read from a here-string rather than through a pipe: `grep -q` exits on its
# first match, the writer upstream of it dies of SIGPIPE, and under `pipefail`
# that becomes the pipeline's status -- so `section_of x | grep -q` reports "no
# match" precisely when there IS one. The disclosure went missing that way once
# already, and the test that caught it is
# `a_breaking_change_among_the_skipped_versions_is_disclosed`.
breaking=()
for candidate in "$version" ${skipped[@]+"${skipped[@]}"}; do
    if grep -q '^### Breaking' <<< "$(section_of "$candidate")"; then
        breaking+=("$candidate")
    fi
done

preamble=""
add_line() { preamble="${preamble}${1}"$'\n'; }

if [ ${#skipped[@]} -gt 0 ]; then
    add_line "> **Upgrading from \`${since}\`?** That is the newest release published before"
    add_line "> this one. The versions below were tagged since, and never published — so they"
    add_line "> ship here, and GitHub's generated notes further down do not cover them:"
    add_line ">"
    for candidate in "${skipped[@]}"; do
        heading="$(heading_of "$candidate")"
        anchor="$(anchor_of "$heading")"
        add_line "> - [${heading}](https://github.com/${repo}/blob/${version}/CHANGELOG.md#${anchor})"
    done
fi

if [ ${#breaking[@]} -gt 0 ]; then
    [ -n "$preamble" ] && add_line ">"
    if [ ${#breaking[@]} -eq 1 ] && [ "${breaking[0]}" = "$version" ]; then
        add_line "> **Breaking changes** are listed below."
    else
        add_line "> **Breaking changes** are listed under ${breaking[*]}."
    fi
fi

if [ -n "$preamble" ]; then
    printf '%s\n' "$preamble"
fi
printf '%s\n' "$release_section"
