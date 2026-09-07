#!/usr/bin/env bash
# SH-574: one identity policy for effective identities and stored commit metadata.
# Requires Bash 3.2, Git and standard utilities; never writes repository state.
# See docs/spec/commit-identity.md for the trust boundary and recovery contract.
set -uo pipefail

gi_die() { printf 'git-identity: %s\n' "$*" >&2; exit 2; }
gi_note() { printf 'git-identity: %s\n' "$*" >&2; }

gi_root="$(git rev-parse --show-toplevel)" || gi_die 'cannot locate the worktree'
gi_scratch="$(mktemp -d /tmp/storyhook-identity.XXXXXX)" || gi_die 'cannot create scratch directory'
trap 'rm -r "$gi_scratch"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Baseline and history readers must not inherit the very overrides they detect.
# HOME/XDG name persistent policy; command-scope Git configuration is not policy.
gi_environment=("PATH=$PATH" "HOME=${HOME:?HOME is required}" "LC_ALL=C" "GIT_CONFIG_NOSYSTEM=1" "GIT_NO_REPLACE_OBJECTS=1" "GIT_GRAFT_FILE=/dev/null")
for gi_variable in XDG_CONFIG_HOME DEVELOPER_DIR TMPDIR; do
    if [ "${!gi_variable+x}" = x ]; then
        gi_environment+=("$gi_variable=${!gi_variable}")
    fi
done
gi_git() { env -i "${gi_environment[@]}" git -C "$gi_root" -c advice.graftFileDeprecated=false "$@" </dev/null; }

gi_global() {
    local status
    gi_git config --global --includes --get "$1"
    status=$?
    case "$status" in
    0 | 1) return 0 ;;
    *) gi_die "cannot read global identity setting $1 (status $status)" ;;
    esac
}

gi_labels=() gi_names=() gi_emails=() gi_roles=() gi_reasons=()
gi_loaded=0

gi_policy() {
    [ "$gi_loaded" = 0 ] || return 0
    local user_name user_email scope status record key value label field i found
    user_name="$(gi_global user.name)" || exit 2
    user_email="$(gi_global user.email)" || exit 2
    gi_author_name="$(gi_global author.name)" || exit 2
    gi_author_email="$(gi_global author.email)" || exit 2
    gi_committer_name="$(gi_global committer.name)" || exit 2
    gi_committer_email="$(gi_global committer.email)" || exit 2
    gi_author_name="${gi_author_name:-$user_name}"
    gi_author_email="${gi_author_email:-$user_email}"
    gi_committer_name="${gi_committer_name:-$user_name}"
    gi_committer_email="${gi_committer_email:-$user_email}"
    for scope in global local; do
        gi_git config "--$scope" --includes --null --get-regexp '^storyhookidentity\.' >"$gi_scratch/policy"
        status=$?
        case "$status" in
        0 | 1) ;;
        *) gi_die "cannot read $scope identity approvals (status $status)" ;;
        esac
        # Git's NUL record delimiter preserves spaces and shell metacharacters.
        while IFS= read -r -d '' record; do
            key="${record%%$'\n'*}"
            value="${record#*$'\n'}"
            field="${key##*.}"
            label="${key#storyhookidentity.}"
            case "$label" in *.*) ;; *) gi_die "identity approval needs a named subsection: $key" ;; esac
            label="${label%.*}"
            [ -n "$label" ] || gi_die "identity approval needs a named subsection: $key"
            found=0
            for ((i=0; i<${#gi_labels[@]}; i++)); do
                if [ "${gi_labels[$i]}" = "$label" ]; then found=1; break; fi
            done
            if [ "$found" = 0 ]; then
                gi_labels[i]="$label" gi_names[i]="" gi_emails[i]="" gi_roles[i]="" gi_reasons[i]=""
            fi
            case "$field" in
            name) gi_names[i]="$value" ;;
            email) gi_emails[i]="$value" ;;
            role) gi_roles[i]="$value" ;;
            reason) gi_reasons[i]="$value" ;;
            *) gi_die "unknown identity approval field $key" ;;
            esac
        done <"$gi_scratch/policy"
    done
    for ((i=0; i<${#gi_labels[@]}; i++)); do
        for value in "${gi_names[$i]}" "${gi_emails[$i]}" "${gi_reasons[$i]}" "${gi_labels[$i]}"; do
            gi_text "$value" || gi_die "identity approval '${gi_labels[$i]}' requires nonempty, single-line name, email, role and reason"
        done
        case "${gi_roles[$i]}" in
        author | committer | both) ;;
        *) gi_die "invalid identity role '${gi_roles[$i]}' in approval '${gi_labels[$i]}'" ;;
        esac
    done
    gi_loaded=1
}

gi_text() {
    [ -n "$1" ] || return 1
    case "$1" in
    *[$'\001'-$'\037'$'\177']*) return 1 ;;
    *[![:space:]]*) return 0 ;;
    *) return 1 ;;
    esac
}

gi_check() {
    local role="$1" name="$2" email="$3" context="$4" expected_name expected_email i
    case "$role" in
    author) expected_name="$gi_author_name"; expected_email="$gi_author_email" ;;
    committer) expected_name="$gi_committer_name"; expected_email="$gi_committer_email" ;;
    esac
    if gi_text "$name" && gi_text "$email"; then
        if [ "$name" = "$expected_name" ] && [ "$email" = "$expected_email" ]; then return 0; fi
        for ((i=0; i<${#gi_labels[@]}; i++)); do
            if { [ "${gi_roles[$i]}" = "$role" ] || [ "${gi_roles[$i]}" = both ]; } &&
                [ "${gi_names[$i]}" = "$name" ] && [ "${gi_emails[$i]}" = "$email" ]; then
                gi_note "$context: approved $role identity $name <$email> via '${gi_labels[$i]}': ${gi_reasons[$i]}"
                return 0
            fi
        done
    fi
    gi_note "$context: $role identity requires review: $name <$email>"
    if [ -n "$expected_name" ] && [ -n "$expected_email" ]; then
        gi_note "expected global $role identity: $expected_name <$expected_email>, or an explicitly approved alternative"
    else
        gi_note "no complete global $role identity; configure global user/role name and email or a reasoned storyhookIdentity alternative"
    fi
    gi_note 'inspect git config --show-origin --show-scope and GIT_AUTHOR_*/GIT_COMMITTER_*; see docs/spec/commit-identity.md'
    return 1
}

gi_current() {
    gi_policy
    local role ident name email status=0
    for role in AUTHOR COMMITTER; do
        # Deliberately NOT gi_git: this is the identity the invoking Git will use.
        ident="$(git var "GIT_${role}_IDENT" </dev/null)" || gi_die "cannot resolve effective $role identity"
        name="${ident% <*}"
        email="${ident##*<}"
        email="${email%%>*}"
        case "$role" in
        AUTHOR) gi_check author "$name" "$email" 'before commit' || status=1 ;;
        COMMITTER) gi_check committer "$name" "$email" 'before commit' || status=1 ;;
        esac
    done
    return "$status"
}

gi_scan() {
    local mode="$1" sha an ae cn ce mismatch status=0
    shift
    local shallow
    shallow="$(gi_git rev-parse --is-shallow-repository)" || gi_die 'cannot determine history completeness'
    [ "$shallow" = false ] || gi_die 'identity scan requires complete history; fetch --unshallow and retry'
    # -z plus tformat gives five NUL-terminated fields per commit, including the last.
    gi_git log --no-use-mailmap -z --format='%H%x00%an%x00%ae%x00%cn%x00%ce' "$@" -- >"$gi_scratch/commits" \
        || gi_die 'cannot enumerate raw commit identities for the requested range'
    [ -s "$gi_scratch/commits" ] || return 0
    gi_policy
    while IFS= read -r -d '' sha; do
        if ! { IFS= read -r -d '' an && IFS= read -r -d '' ae &&
            IFS= read -r -d '' cn && IFS= read -r -d '' ce; }; then
            gi_die "incomplete commit identity record at $sha"
        fi
        mismatch=0
        gi_check author "$an" "$ae" "$sha" || mismatch=1
        gi_check committer "$cn" "$ce" "$sha" || mismatch=1
        if [ "$mode" = audit ]; then
            printf 'author\t%s <%s>\ncommitter\t%s <%s>\n' "$an" "$ae" "$cn" "$ce" >>"$gi_scratch/inventory"
            if [ "$mismatch" = 1 ]; then
                printf '%s\t%s <%s>\t%s <%s>\n' "$sha" "$an" "$ae" "$cn" "$ce" >>"$gi_scratch/findings"
            fi
        fi
        [ "$mismatch" = 0 ] || status=1
    done <"$gi_scratch/commits"
    return "$status"
}

gi_push() {
    local remote="$1" local_ref local_sha remote_ref remote_sha extra tip kind old status=0 scan_status refs
    local exclusions=()
    while read -r local_ref local_sha remote_ref remote_sha extra; do
        [ -n "$local_ref" ] || continue
        [ -n "$remote_sha" ] && [ -z "$extra" ] || gi_die 'malformed pre-push identity record'
        case "$remote_ref" in refs/*) ;; *) gi_die "invalid destination ref $remote_ref" ;; esac
        case "$local_sha" in
        *[!0]*) ;;
        *) continue ;;
        esac
        tip="$(gi_git rev-parse --verify "${local_sha}^{}")" || gi_die "cannot resolve identity tip $local_sha"
        kind="$(gi_git cat-file -t "$tip")" || gi_die "cannot inspect identity tip $tip"
        [ "$kind" = commit ] || continue
        # A nonempty revision array also works under macOS Bash 3.2 nounset.
        exclusions=("$tip")
        case "$remote_sha" in
        *[!0]*)
            old="$(gi_git rev-parse --verify "${remote_sha}^{}")" || gi_die "cannot resolve advertised identity baseline $remote_sha; fetch the destination and retry"
            kind="$(gi_git cat-file -t "$old")" || gi_die "cannot inspect identity baseline $old"
            if [ "$kind" = commit ]; then exclusions+=("^$old"); fi
            ;;
        *)
            # A URL/path push has no reliable tracking namespace. No guessed baseline.
            refs="$(gi_git for-each-ref --format='%(objectname)' "refs/remotes/$remote/")" || gi_die "cannot read destination tracking refs for $remote"
            while IFS= read -r old; do
                [ -z "$old" ] || exclusions+=("^$old")
            done <<<"$refs"
            ;;
        esac
        gi_scan push "${exclusions[@]}"
        scan_status=$?
        case "$scan_status" in
        0) ;;
        1) status=1 ;;
        *) return "$scan_status" ;;
        esac
    done
    return "$status"
}

case "${1:-}" in
current)
    [ "$#" = 1 ] || gi_die 'usage: git-identity.sh current'
    gi_current
    ;;
push)
    [ "$#" = 2 ] || gi_die 'usage: git-identity.sh push <remote>'
    gi_push "$2"
    ;;
audit)
    [ "$#" -le 2 ] || gi_die 'usage: git-identity.sh audit [revision-range]'
    : >"$gi_scratch/inventory"
    : >"$gi_scratch/findings"
    if [ "$#" = 2 ]; then
        case "$2" in -*) gi_die 'audit range must not be an option' ;; esac
        gi_scan audit "$2"
    else
        gi_scan audit --all
    fi
    gi_status=$?
    [ "$gi_status" -le 1 ] || exit "$gi_status"
    printf 'COUNT\tROLE\tIDENTITY\n'
    sort "$gi_scratch/inventory" | uniq -c | sed -E 's/^ *([0-9]+) /\1\t/' || gi_die 'cannot summarize identity inventory'
    printf '\nREQUIRES REVIEW (differences are not proof of corruption)\nCOMMIT\tAUTHOR\tCOMMITTER\n'
    cat "$gi_scratch/findings" || gi_die 'cannot read audit findings'
    exit "$gi_status"
    ;;
*) gi_die 'usage: git-identity.sh current|push <remote>|audit [revision-range]' ;;
esac
