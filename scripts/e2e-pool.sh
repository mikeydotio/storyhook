#!/usr/bin/env bash
#
# The browser leg's slice planner and bounded pool (SH-792). Sourced by
# `scripts/run-e2e.sh`; `tests/e2e_pool.rs` drives every function here under
# macOS's /bin/bash 3.2 and the caller's own `set -euo pipefail`.
#
# WHY SLICES. The leg used to run its Playwright projects one after another,
# `workers: 1` each -- about 41 minutes, 36 of them the 606 desktop tests run
# once per engine. `workers: 1` is a council decision (SH-627) and stays: one
# Playwright process, one daemon, one seed (SH-335). What changes is how much
# of the selection one such process owns. A slice is one project plus a set of
# WHOLE spec files, run through `run_one_project`'s own fixture, so slices share
# nothing but the machine and can run at the same time. Whole files, because
# `fullyParallel: false` keeps a file's tests in one worker in order, and some
# files carry state from one test to the next.
#
# WHY A TEST-LIST, NOT `--shard`. Playwright's `--shard=i/n` cuts the file
# sequence into contiguous runs by test count, so a shard is off by up to its
# largest file and can be empty. A `--test-list` of `[project] › file` lines
# (Playwright 1.63, `loadTestList`) is an exact partition, and it intersects
# with the caller's own filters rather than replacing them.
#
# WHY THIS POOL. It is `plugins/story/tests/run-tests.sh`'s proven bash-3.2
# shape (no `wait -n`): each job renames its exit status into place and the
# parent polls for it. Hardened for a caller running under errexit, which that
# runner is not: every `kill` and `wait` is guarded, and a job that dies before
# it can write its status is read as a failure instead of being waited on
# forever.

# Prints `project<TAB>file<TAB>count` for every spec file the Playwright
# `--list` listing on stdin selects, in listing order.
#
# A listing line is `  [project] › file:line:col › title…` (Playwright's
# ListModeReporter). Other lines -- the header, the `Total:` line -- are not
# tests. The caller compares the counts' sum with the `Total:` line, so a
# drift in this shape is refused rather than silently dropping tests.
e2e_pool_file_counts() {
  LC_ALL=C awk -v arrow=' › ' '
    index($0, "  [") == 1 {
      rest = substr($0, 4)
      close_at = index(rest, "]" arrow)
      if (close_at == 0) next
      project = substr(rest, 1, close_at - 1)
      rest = substr(rest, close_at + 1 + length(arrow))
      cut = index(rest, arrow)
      if (cut == 0) next
      file = substr(rest, 1, cut - 1)
      sub(/:[0-9]+:[0-9]+$/, "", file)
      key = project "\t" file
      if (!(key in count)) order[++n] = key
      count[key]++
    }
    END { for (i = 1; i <= n; i++) print order[i] "\t" count[order[i]] }
  '
}

# e2e_pool_plan BUDGET LIST_DIR PROJECT...
#
# Splits the `project<TAB>file<TAB>count[<TAB>seconds]` lines on stdin (from
# `e2e_pool_file_counts`) into slices, writes each slice's Playwright
# test-list to `LIST_DIR/<slice>.list`, and prints one
# `slice<TAB>project<TAB>test-list<TAB>count` row per slice, largest first --
# the order the pool should admit them in.
#
# PROJECT... are the projects this run selected, in config order; that order
# breaks ties. A project with no tests gets no slice: the caller reports it
# skipped. Every project with tests gets at least one slice, and never more
# slices than it has files. The rest of BUDGET slices go, one at a time, to
# whichever project's slices are currently largest, and each project's files
# are packed longest first into its lightest slice. A slice is named after its
# project, or `<project>.<i>of<n>` when the project is split.
#
# Refuses (status 2, nothing printed) a BUDGET that is not a positive integer,
# a missing LIST_DIR, a project name that cannot be a file or gate-progress
# name, and a count line for a project the run did not select.
e2e_pool_plan() {
  if [ "$#" -lt 2 ]; then
    echo "e2e_pool_plan: usage: e2e_pool_plan BUDGET LIST_DIR PROJECT..." >&2
    return 2
  fi
  local budget="$1" lists="$2" project
  shift 2
  case "$budget" in
    '' | *[!0-9]* | 0*)
      echo "e2e_pool_plan: the slice budget must be a positive integer, got '$budget'" >&2
      return 2
      ;;
  esac
  if [ ! -d "$lists" ]; then
    echo "e2e_pool_plan: test-list directory $lists does not exist" >&2
    return 2
  fi
  # A slice name is a file name, a Playwright output directory and a
  # gate-progress path segment; `/` would nest the last.
  for project in "$@"; do
    case "$project" in
      '' | *[!A-Za-z0-9._-]*)
        echo "e2e_pool_plan: project name '$project' cannot name a slice (only [A-Za-z0-9._-])" >&2
        return 2
        ;;
    esac
  done

  LC_ALL=C awk -F '\t' -v budget="$budget" -v lists="$lists" -v projects="$*" -v arrow=' › ' '
    BEGIN {
      np = split(projects, plist, " ")
      for (i = 1; i <= np; i++) rank[plist[i]] = i
    }
    bad { next }
    (NF != 3 && NF != 4) || $3 !~ /^[0-9]+$/ || $3 + 0 <= 0 ||
    (NF == 4 && ($4 !~ /^[0-9]+([.][0-9]+)?$/ || $4 + 0 <= 0 || $4 + 0 > 1e12)) {
      printf "e2e_pool_plan: malformed count line: %s\n", $0 > "/dev/stderr"
      bad = 1
      next
    }
    !($1 in rank) {
      printf "e2e_pool_plan: the listing names project %s, which this run did not select\n", $1 > "/dev/stderr"
      bad = 1
      next
    }
    {
      p = $1
      nf[p]++
      file[p, nf[p]] = $2
      cnt[p, nf[p]] = $3 + 0
      weight[p, nf[p]] = (NF == 4 ? $4 + 0 : $3 + 0)
      total[p] += weight[p, nf[p]]
    }
    END {
      if (bad) exit 2

      na = 0
      for (i = 1; i <= np; i++) {
        if (plist[i] in nf) {
          act[++na] = plist[i]
          n[plist[i]] = 1
        }
      }
      if (na == 0) exit 0

      # Spend what is left of the budget where the largest slice is.
      spare = (budget > na ? budget : na) - na
      while (spare > 0) {
        best = ""
        bestload = -1
        for (i = 1; i <= na; i++) {
          p = act[i]
          if (n[p] >= nf[p]) continue
          if (total[p] / n[p] > bestload) {
            best = p
            bestload = total[p] / n[p]
          }
        }
        if (best == "") break
        n[best]++
        spare--
      }

      rows = 0
      for (i = 1; i <= na; i++) {
        p = act[i]
        # Files by measured seconds (count without history), stable on ties.
        for (j = 1; j <= nf[p]; j++) ord[j] = j
        for (j = 2; j <= nf[p]; j++) {
          k = ord[j]
          m = j - 1
          while (m >= 1 && weight[p, ord[m]] < weight[p, k]) {
            ord[m + 1] = ord[m]
            m--
          }
          ord[m + 1] = k
        }
        for (b = 1; b <= n[p]; b++) { load[b] = 0; counts[b] = 0 }
        for (j = 1; j <= nf[p]; j++) {
          f = ord[j]
          best = 1
          for (b = 2; b <= n[p]; b++) if (load[b] < load[best]) best = b
          bin[p, f] = best
          load[best] += weight[p, f]
          counts[best] += cnt[p, f]
        }
        for (b = 1; b <= n[p]; b++) {
          name = (n[p] == 1 ? p : p "." b "of" n[p])
          path = lists "/" name ".list"
          printf "" > path
          manifest = path ".tsv"
          printf "" > manifest
          for (f = 1; f <= nf[p]; f++) if (bin[p, f] == b) print "[" p "]" arrow file[p, f] > path
          for (f = 1; f <= nf[p]; f++) if (bin[p, f] == b) print p "\t" file[p, f] "\t" cnt[p, f] > manifest
          close(path)
          close(manifest)
          rows++
          rname[rows] = name
          rproj[rows] = p
          rpath[rows] = path
          rcount[rows] = counts[b]
          rweight[rows] = load[b]
        }
      }

      # Admission order: largest first; config order, then slice number, on
      # ties (a stable sort over rows already in that order).
      for (r = 1; r <= rows; r++) idx[r] = r
      for (r = 2; r <= rows; r++) {
        k = idx[r]
        m = r - 1
        while (m >= 1 && rweight[idx[m]] < rweight[k]) {
          idx[m + 1] = idx[m]
          m--
        }
        idx[m + 1] = k
      }
      for (r = 1; r <= rows; r++) {
        k = idx[r]
        printf "%s\t%s\t%s\t%d\n", rname[k], rproj[k], rpath[k], rcount[k]
      }
    }
  '
}

# e2e_pool_run JOBS GRACE RUNNER NAME...
#
# Runs `RUNNER NAME` for every NAME, in the order given, at most JOBS at once.
# Call it from the script's main shell, not a subshell: on a signal it
# re-raises the signal at `$$`.
#
# Each run's stdout and stderr go to a private log, never to the terminal
# while it runs: concurrent Playwright list reporters would interleave line by
# line, and the verifier's failure summary shows only the log's tail. Live
# one-line `started`/`finished` notes go to stderr; each log is replayed whole
# on stdout, in the order given, as soon as it and every run before it have
# finished; a summary table closes the output.
#
# Every run happens even after one fails. Returns 0 when every run exited 0
# and 1 otherwise -- normalized, because `scripts/gate-legs.sh` reads any
# status of 125 or more as a cancelled gate, and a browser OOM-killed inside
# one slice is a red leg, not a cancellation.
#
# On TERM, INT or HUP it admits nothing more and stops the running slices in
# the order their cleanup needs: every non-shell process under a slice first
# (Playwright, browsers, CLI calls -- the writers), then the shells, whose
# EXIT traps delete what those writers were writing, deepest first. Each step
# gets GRACE seconds; whatever is left is killed. Then it restores the
# caller's TERM/INT/HUP traps, re-raises the signal, and never returns. The
# caller's EXIT trap is left alone, so it still runs.
e2e_pool_run() {
  local jobs="${1:-}" grace="${2:-}" runner="${3:-}"
  case "$jobs" in
    '' | *[!0-9]* | 0*)
      echo "e2e_pool_run: jobs must be a positive integer, got '$jobs'" >&2
      return 2
      ;;
  esac
  case "$grace" in
    '' | *[!0-9]*)
      echo "e2e_pool_run: grace must be a whole number of seconds, got '$grace'" >&2
      return 2
      ;;
  esac
  if [ -z "$runner" ] || ! type "$runner" >/dev/null 2>&1; then
    echo "e2e_pool_run: runner '$runner' is not a command or function" >&2
    return 2
  fi
  shift 3

  # Globals, not locals: the signal handler reads them.
  _e2e_pool_grace="$grace"
  _e2e_pool_runner="$runner"
  _e2e_pool_names=("$@")
  _e2e_pool_total="$#"
  _e2e_pool_pids=()
  _e2e_pool_started=()
  _e2e_pool_seconds=()
  _e2e_pool_verdict=()
  _e2e_pool_stopping=0
  _e2e_pool_work="$(mktemp -d /private/tmp/story-e2e-pool.XXXXXX)" || {
    echo "e2e_pool_run: could not create a work directory under /private/tmp" >&2
    return 1
  }
  _e2e_pool_saved_traps="$(trap -p TERM INT HUP)"
  trap '_e2e_pool_on_signal TERM' TERM
  trap '_e2e_pool_on_signal INT' INT
  trap '_e2e_pool_on_signal HUP' HUP

  local began="$SECONDS" next=0 flushed=0 running=0 i
  while [ "$flushed" -lt "$_e2e_pool_total" ]; do
    while [ "$_e2e_pool_stopping" = 0 ] && [ "$next" -lt "$_e2e_pool_total" ] && [ "$running" -lt "$jobs" ]; do
      _e2e_pool_launch "$next"
      running=$((running + 1))
      next=$((next + 1))
    done

    i="$flushed"
    while [ "$i" -lt "$next" ]; do
      if [ -z "${_e2e_pool_verdict[$i]:-}" ] && _e2e_pool_reap "$i"; then
        running=$((running - 1))
      fi
      i=$((i + 1))
    done

    while [ "$flushed" -lt "$next" ] && [ -n "${_e2e_pool_verdict[$flushed]:-}" ]; do
      echo "e2e-pool: ==== ${_e2e_pool_names[$flushed]}: ${_e2e_pool_verdict[$flushed]} in ${_e2e_pool_seconds[$flushed]}s ===="
      cat "$_e2e_pool_work/$flushed.log" 2>/dev/null || true
      flushed=$((flushed + 1))
    done

    # A caller trap that swallowed the re-raised signal lands here: report
    # what ran and admit nothing more.
    if [ "$_e2e_pool_stopping" != 0 ] && [ "$flushed" -ge "$next" ]; then
      break
    fi
    if [ "$flushed" -lt "$_e2e_pool_total" ]; then
      sleep 0.2
    fi
  done

  local failed=0
  echo "e2e-pool: summary -- jobs=$jobs slices=$_e2e_pool_total wall=$((SECONDS - began))s"
  i=0
  while [ "$i" -lt "$_e2e_pool_total" ]; do
    if [ "${_e2e_pool_verdict[$i]:-}" != passed ]; then
      failed=1
    fi
    printf '  %-36s %6ss  %s\n' "${_e2e_pool_names[$i]}" "${_e2e_pool_seconds[$i]:--}" "${_e2e_pool_verdict[$i]:-not run}"
    i=$((i + 1))
  done

  rm -rf "$_e2e_pool_work"
  _e2e_pool_restore_traps
  return "$failed"
}

# Starts slice $1 in the background. Its exit status reaches the parent
# through a file renamed into place, so the poll never reads a half-written
# one. `|| rc=$?` keeps errexit off inside the runner, as a bare call from a
# caller's `||` would.
_e2e_pool_launch() {
  local i="$1"
  _e2e_pool_started[$i]="$SECONDS"
  (
    rc=0
    "$_e2e_pool_runner" "${_e2e_pool_names[$i]}" >"$_e2e_pool_work/$i.log" 2>&1 </dev/null || rc=$?
    printf '%s\n' "$rc" >"$_e2e_pool_work/$i.rc.tmp"
    mv "$_e2e_pool_work/$i.rc.tmp" "$_e2e_pool_work/$i.rc"
  ) &
  _e2e_pool_pids[$i]=$!
  echo "e2e-pool: started ${_e2e_pool_names[$i]}" >&2
}

# Succeeds, recording the verdict, once slice $1 has finished.
_e2e_pool_reap() {
  local i="$1" rc_file="$_e2e_pool_work/$1.rc"
  if [ ! -e "$rc_file" ]; then
    # bash reaps its background children as they exit, so `kill -0` fails
    # for a finished wrapper; the status file may have landed in between.
    kill -0 "${_e2e_pool_pids[$i]}" 2>/dev/null && return 1
    if [ ! -e "$rc_file" ]; then
      _e2e_pool_settle "$i" ""
      return 0
    fi
  fi
  _e2e_pool_settle "$i" "$(cat "$rc_file")"
}

_e2e_pool_settle() {
  local i="$1" status="$2"
  wait "${_e2e_pool_pids[$i]}" 2>/dev/null || true
  _e2e_pool_seconds[$i]=$((SECONDS - _e2e_pool_started[$i]))
  case "$status" in
    0) _e2e_pool_verdict[$i]=passed ;;
    '') _e2e_pool_verdict[$i]="FAILED (no verdict: the slice's wrapper died before writing one)" ;;
    *) _e2e_pool_verdict[$i]="FAILED (exit $status)" ;;
  esac
  echo "e2e-pool: finished ${_e2e_pool_names[$i]}: ${_e2e_pool_verdict[$i]} in ${_e2e_pool_seconds[$i]}s" >&2
}

_e2e_pool_on_signal() {
  local signal="$1"
  # A second signal must not cut the teardown short.
  trap '' TERM INT HUP
  _e2e_pool_stopping=1
  echo "e2e-pool: $signal received; stopping the running slices" >&2
  _e2e_pool_teardown
  rm -rf "$_e2e_pool_work" 2>/dev/null || true
  _e2e_pool_restore_traps
  kill -s "$signal" "$$" 2>/dev/null || true
}

_e2e_pool_restore_traps() {
  trap - TERM INT HUP
  eval "$_e2e_pool_saved_traps"
}

# Stops every running slice: writers, then shells, then whatever is left.
_e2e_pool_teardown() {
  local i=0 wrappers="" tree="" writers="" shells="" pid
  while [ "$i" -lt "$_e2e_pool_total" ]; do
    if [ -n "${_e2e_pool_pids[$i]:-}" ] && [ -z "${_e2e_pool_verdict[$i]:-}" ]; then
      wrappers="$wrappers ${_e2e_pool_pids[$i]}"
      tree="$tree $(_e2e_pool_descendants "${_e2e_pool_pids[$i]}")"
    fi
    i=$((i + 1))
  done
  # Post-order, so `shells` lists the deepest first.
  for pid in $tree; do
    if _e2e_pool_is_shell "$pid"; then
      shells="$shells $pid"
    else
      writers="$writers $pid"
    fi
  done

  _e2e_pool_signal_all TERM $writers
  _e2e_pool_await $writers
  _e2e_pool_signal_all TERM $shells $wrappers
  _e2e_pool_await $shells $wrappers
  _e2e_pool_signal_all KILL $tree $wrappers
  for pid in $wrappers; do
    wait "$pid" 2>/dev/null || true
  done
}

# Prints every descendant of $1, children before their parents.
_e2e_pool_descendants() {
  local child
  for child in $(pgrep -P "$1" 2>/dev/null || true); do
    _e2e_pool_descendants "$child"
    echo "$child"
  done
}

_e2e_pool_is_shell() {
  local comm
  comm="$(ps -o comm= -p "$1" 2>/dev/null)" || return 1
  comm="${comm##*/}"
  while [ "${comm% }" != "$comm" ]; do
    comm="${comm% }"
  done
  case "$comm" in
    bash | -bash | sh | -sh | zsh | dash | ksh) return 0 ;;
  esac
  return 1
}

_e2e_pool_signal_all() {
  local signal="$1" pid
  shift
  for pid in "$@"; do
    kill -s "$signal" "$pid" 2>/dev/null || true
  done
}

# Waits until every pid is gone, sharing one GRACE-second budget.
_e2e_pool_await() {
  local ticks=$((_e2e_pool_grace * 10)) pid
  for pid in "$@"; do
    while [ "$ticks" -gt 0 ] && _e2e_pool_alive "$pid"; do
      sleep 0.1
      ticks=$((ticks - 1))
    done
  done
}

# A zombie still answers `kill -0`; it is not running.
_e2e_pool_alive() {
  local stat
  kill -0 "$1" 2>/dev/null || return 1
  stat="$(ps -o stat= -p "$1" 2>/dev/null)" || return 1
  case "$stat" in
    *Z*) return 1 ;;
  esac
  return 0
}
