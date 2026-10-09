# Throughput measurement adapter — work in progress

SH-872's cohort controller now has an execution bridge and a per-leg path in
`leg.sh`. These are source prerequisites, not a supported campaign entry point.
No before/after measurements or production performance claims exist yet.

`run_observation` requires a complete identity observer and a supervised gate
launcher. It persists a slot before launch, compares observed identity afterward,
and requires complete top-level detector progress plus exact exit/settlement
evidence. Exceptions retain the pending slot and append a diagnostic. They do not
create an exit observation, free an unresolved owner, or permit a replacement.

`OwnedGate` calls the current verifier owner with its existing host reservation,
session custody and execution-result channel. It checks the command against the
owner-bound manifest and mirrors progress. Deadline or health refusal signals
only its direct supervisor, which retains descendant cleanup ownership. A cleanup
observation timeout preserves the owner and all evidence. The outer campaign
still needs to supply the complete identity and health observers.

The owner-bound throughput mode has its own result namespace. Cold and warm
legs execute even when ordinary receipts exist. Reuse requires the immediately
preceding successful warm slot and each leg's matching command and inherited
environment digest. Unknown environment inputs participate in that digest.
Only attempt telemetry paths, the gate deadline and the shell's `_` value are
excluded. Unexpected environment drift therefore refuses reuse. Measured legs
never read or write ordinary gate-leg receipts; existing measurement receipt
boundaries suppress production gate and tree certification.

Remaining work before any campaign starts:

- Complete and validate source, toolchain, external configuration, environment,
  worker-limit and target identity capture. The supplied observer is a required
  integration contract, not proof those inputs have already been captured.
- Prepare the throughput manifest and coordinate revision/window lifetimes,
  fresh cold targets, exact-owned settled target turnover and competing work.
- Validate the real owner, gate, shell-leg and receipt boundaries together in
  the coordinated quiet host window, including cancellation and failed sensors.
- Collect the approved matched observations. No source test substitutes for
  measurement acceptance, and no draft PR authorizes production activation.
