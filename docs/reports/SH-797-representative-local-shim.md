# SH-797 representative local Git-shim observation

One bounded paired run completed on 2026-10-09, 18:00:15–18:00:30 UTC.
Collector source: `88a622d843928a797b9f792b1a1e3c47170738e0`.
Measured source: `2c6d57dc76bf7efdc57a765dc9a0bf205e348a7d` (the retained
measurement snapshot, not a claim about the latest dev tree).

All 240 timed operations completed, with 40 alternating direct/shim pairs in
each case. All 24 warmup operations and 11 preparation/postflight commands
completed separately. All 275 command receipts prove empty owned sessions before
leader reap. Collection took 14.896 seconds within the 300-second budget;
wrapper elapsed time was 14.976 seconds. No failed or replaced attempt in this
run. Earlier incomplete runs remain preserved separately.

| Local case | Pairs | Median added shim time | Minimum | Maximum |
|---|---:|---:|---:|---:|
| `rev-parse --git-dir` | 40 | 31.207 ms | 24.029 ms | 39.425 ms |
| mapped explicit `ls-remote` | 40 | 30.153 ms | 15.113 ms | 52.817 ms |
| mapped origin `ls-remote` | 40 | 49.312 ms | 21.591 ms | 61.417 ms |

Differences are paired shim minus direct wall duration, measured from Popen entry
to the non-reaping exact-child exit observation. Observer scheduling can delay
that endpoint. Output hashes matched within every pair. The fixture uses an
empty local bare repository; it does not measure network or real repository size.

Recorded load1 ranged from 12.107 to 25.719 on ten logical CPUs. CPU idle
fractions across the short observation intervals ranged from 7.018% to 69.697%.
These intervals include custody observation/cleanup and do not establish sustained
CPU saturation. Native memory pressure was normal throughout and swap used was
zero. Python was 3.14.8. Raw observations retain the actual Git/Python paths and
hashes, pinned shim bytes, pair order, durations, exposure and cleanup evidence.

The private raw result has SHA-256
`d221769b755f2029e66c098ef6864c2e934a091bd754b95e711b78649bf34810`.
It is retained in the task evidence ledger; private process/path metadata is not
published in this report. No compilation, provider, daemon, tmux or network
operation ran in the experiment. No exclusive Apple build lane was required.

This is descriptive local shim overhead under observed variable load. The
historical/current production dispatch pair remains unmeasured, and attribution
of the historical 1.6–1.9× slowdown is inconclusive. This result alone does not
complete SH-797, SH-801 or SH-872 and does not certify a release.
