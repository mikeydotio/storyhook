# Representative workload measurement

Protocol v2, 2026-10-09. The user explicitly amended idle-only requirements in
SH-797 and SH-801 to study resilience on their everyday variable-load Mac.
SH-872's collector-only idle gate is also removed. Normal memory pressure,
known process ownership, immutable input identity, storage and hard deadlines
remain admission requirements. A load average is not a CPU utilization meter.

Record raw load averages, logical CPUs, native CPU tick boundaries, swap and
memory pressure. For gate campaigns retain timestamped process census and sampled
owned CPU/RSS. Tick deltas estimate mean CPU use between observations; short
intervals with no ticks are unknown, never reported as idle. Five-second sampling
can miss spikes; process percentages use different averaging windows. No claim
of sustained saturation follows from a single high load average.

Our broad suites and releases remain serialized and need actual lane coordination.
External natural work may continue and is retained as exposure. No external
process is stopped, and a load spike neither resets a deadline nor proves gate
progress. Unsafe/unknown observations hold or stop the owned attempt with its
failure and cleanup evidence intact. No sample replacement or retry-to-green.

## Bounded local Git-shim experiment

The explicitly authorized small experiment is a single sequential local run:

- Three cases: `rev-parse --git-dir`, mapped explicit `ls-remote`, mapped origin
  `ls-remote`. Both arms use equivalent private repositories and a local bare
  endpoint. Logical HTTPS mapping resolves locally; Git permits only file transport.
- Four warmups per arm/case: 24 operations, excluded from timing statistics.
- Forty alternating direct/shim pairs per case: 240 timed operations. Pair order
  alternates; stdout hashes must match within every pair. Retain every start,
  failure, duration, input identity, exposure and exact-session cleanup receipt.
- One 300-second monotonic budget covering preparation, warmups and samples;
  each command additionally has its original ten-second ceiling. On exhaustion,
  admit no new work. The existing two-second TERM plus two-second KILL cleanup
  allowance may extend past the collection budget; report that separately.
- No compilation, provider, daemon, tmux, or network call. One collector lock,
  no parallel tests, no automatic retry. Preserve private scratch and all evidence.
- Time Popen entry to exact non-reaping exit observation. Native custody cleanup
  and output reading are outside the timed interval. The observer thread can be
  delayed by scheduling. Exposure is read before/after; this interval also includes
  cleanup/observation work, so it is a covariate, not exact command CPU attribution.
- Pin and recheck measured source, Git/Python binaries and generated/source shim
  bytes. The native custody bootstrap remains byte-pinned to the reviewed source.

`scripts/git_shim_measurement.py` runs only with `--run-shim`; `--samples` must
remain 40. Use a fresh private output beneath the existing task measurement root.
The pinned measured snapshot for this experiment predates the collector changes;
report its exact commit, never relabel it as current dev. This experiment only
estimates local shim overhead. Historical/current production dispatch remains
unmeasured and SH-797 stays open regardless of a successful local run.

## Gate controls and inference

SH-801 retains ten interleaved normal/utility pairs, a separate warmup, one exact
tree and day, warm build/no verdict reuse, real overlapping list/hook probes,
21 total gate slots, 60-minute gate ceiling and 22.25-hour campaign ceiling.
SH-872 retains three C/W/R blocks per revision, two revisions (18 slots), a
20-hour continuous reservation, 10-hour windows, 75-minute C/W and 10-minute R
ceilings. Each retains 40-minute preparation, source/tool/environment controls,
900-second breach reporting, detector coverage and exact cleanup. These are
containment ceilings, not expected runtimes or permission to extend a reservation.

Complete sample collection is separate from statistical inference. Report raw
paired differences/ratios and their ranges, order, failures, missing observations
and exposure imbalance. Do not infer speedup from unmatched historical logs or
normalise away load post hoc. Small or inconsistent effects, unmatched exposure,
insufficient pairs or failed samples remain inconclusive. Any accepted optimization
also requires regression or mutation evidence that detection is preserved.

The proposed 15-attempt/20-hour exploratory paired gate design is pending review;
it does not replace the executable 18-slot policy. No expensive campaign begins
from this source task. Fresh binary/input capture, design review and coordinated
start are required. No automation/provider setting or release order changes.
