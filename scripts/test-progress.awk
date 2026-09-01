# The libtest line grammar shared by scripts/test-delta.sh and the SH-524
# gate progress journal.
#
# Reads cargo test's own default text output (never `--format json`) from
# stdin and emits `<binary>\t<test>\t<PASS|FAIL>`, one line per test function
# that actually ran.
#
# cargo prints one "     Running <path> (<binary-path>)" header per test
# binary, followed by that binary's own "test <name> ... ok|FAILED|ignored"
# lines. Tracking "current binary" as a state machine over the stream is what
# lets two binaries share a test's local name (e.g. two files each with their
# own `it_works`) without colliding.
#
# Extracted from test-delta.sh (SH-429) rather than duplicated, so the two
# readers of this grammar cannot drift into disagreeing about what a libtest
# line looks like (CLAUDE.md: SH-136, SH-198, SH-260/276 already paid for
# exactly that shape once).

/^     Running / {
    line = $0
    sub(/^     Running /, "", line)
    split(line, parts, / \(/)
    src = parts[1]
    n = split(src, segs, "/")
    file = segs[n]
    sub(/\.rs$/, "", file)
    current = file
    next
}
/^test .* \.\.\. (ok|FAILED)$/ {
    name = $0
    sub(/^test /, "", name)
    sub(/ \.\.\. (ok|FAILED)$/, "", name)
    outcome = ($0 ~ / \.\.\. ok$/) ? "PASS" : "FAIL"
    bin = (current == "") ? "(unknown)" : current
    print bin "\t" name "\t" outcome
}
