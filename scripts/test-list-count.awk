# Counts the libtest cases selected by `--list` without executing them.
#
# Pretty and terse libtest discovery both print exactly one `<name>: test` or
# `<name>: benchmark` line per selected case. Counting those records instead
# of Rust source recognizes cfg-gated, macro-generated and doctest cases using
# the same harness that will execute them.

/: (test|benchmark)$/ {
    total++
}

END {
    print total + 0
}
