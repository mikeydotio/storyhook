# Verifier compiler diagnostics

SH-685 replaces error-prefix matching over the combined verification log.
Passing tests deliberately print errors, including daemon disconnects; those
strings cannot establish which process failed.

## Contract

`verify-pr.sh` creates an empty `<attempt-log>.compiler.jsonl` for each gate
attempt and passes its absolute path as `STORYHOOK_COMPILER_DIAGNOSTICS`.
`cargo_diagnostics.py` is explicitly invoked by the build and Clippy recipes,
and by both execution and discovery paths in `run-tests.sh`. With no artifact
configured, the adapter executes the original command unchanged.

The adapter observes build-only Cargo stdout with `--message-format=json`.
It validates and appends `compiler-message` records, renders their diagnostic
text into the ordinary log, and stops interpreting records at that invocation's
`build-finished`. Stderr is ordinary log context. Unknown output is preserved.
The artifact is a regular file; append locking keeps concurrent records whole.
The verifier summarizes error-level records, deduplicates diagnostic headlines,
and retains its existing count and display limits. Missing or malformed evidence
produces an explicit collection diagnostic rather than an empty successful read.

For `cargo test`, the adapter first runs the same compilation selection with
`--no-run`, without harness arguments. On success it executes the original
Cargo command with the original stdout and stderr descriptors. This retains
Cargo's selection, harness flags, exit status, and the shared-file ordering
used to associate `Running` records with test cases. It does not implement a
second test runner. Clippy's arguments after `--`, including `-D warnings`,
remain attached to its build command.

The artifact environment is removed before either build or test execution.
Nested test commands therefore cannot append to the enclosing verification's
artifact. The collector reads finite stdout file-size snapshots and returns
when the owned Cargo process exits, even if a descendant still holds stdout.
The existing supervisor owns process-group cancellation; the collector drains
final output without forwarding a duplicate signal. Collection failures are
explicit and nonzero; the compiler's ordinary nonzero status is preserved when
collection succeeds.

## Deliberate coverage limits

Cargo resolver errors, failing build scripts, rustdoc compilation errors, and
compilation unexpectedly repeated during test execution need not produce a
collected compiler record. Stable Cargo cannot prepare doctests with `--no-run`,
so doctests use the original command and combined log. These failures retain
their nonzero status and full log; they are never classified from an error
prefix. An empty artifact means no structured compiler error was collected,
not that compilation necessarily succeeded. A successful build-only process
without `build-finished` fails collection explicitly.

This phase separation is the unanimous council decision recorded on SH-685;
use `story show SH-685` for its context, alternatives, and limitations.
[Cargo documents stdout JSON and the build-finished boundary](https://doc.rust-lang.org/cargo/reference/external-tools.html).

## Regression evidence

`tests/merge_gate.rs` drives production verification with intentional test errors
and real rustc errors, including bounded summaries and fresh attempt artifacts.
`tests/cargo_diagnostics.rs` runs the Python contracts with real Cargo packages:
fragmented records, stderr impostors, nested commands, rapid test binaries,
selection, Clippy flags, signals, malformed evidence, and detached writers.
`tests/gate_lock.rs` covers the enabled collector through test discovery,
execution, progress publication, and lock release.
