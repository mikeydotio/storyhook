"""The admission inventory: every managed runner entry and its policy workloads (SH-869).

An entry names policy workloads, never resource numbers. A root reserves
`overhead + units * unit` from the measured host policy; a nested pool
divides its inherited share by `unit`. `tests/runner_admission_inventory.rs`
requires a production caller for every entry here.
"""

from dataclasses import dataclass

from .policy import CLASSES


@dataclass(frozen=True)
class Entry:
    """One managed runner entry point.

    `work` is the admission class of a root; `unit` names the policy workload
    of one concurrent unit; `overhead`, when present, names the workload of
    the entry's own orchestration, reserved once; `pool` marks an entry whose
    runner honours a unit count.
    """

    id: str
    work: str
    unit: str
    overhead: str | None = None
    pool: bool = False

    def __post_init__(self):
        if self.work not in CLASSES:
            raise ValueError(f"entry {self.id}: unknown work class {self.work}")


ENTRIES = {entry.id: entry for entry in (
    Entry("causal-rust", "repair", "causal-rust"),
    Entry("cargo-managed", "build", "cargo-managed"),
    Entry("rustc", "build", "rustc"),
    Entry("cargo-test-binary", "test", "rust-test-binary"),
    Entry("rust-pool", "test", "rust-test-thread", "rust-pool", pool=True),
    Entry("plugin-pool", "test", "plugin-script", "plugin-pool", pool=True),
    Entry("plugin-script", "test", "plugin-script"),
    Entry("browser-pool", "test", "browser-slice", "browser-pool", pool=True),
    Entry("verifier-python-workers", "test", "verifier-python-worker", pool=True),
    Entry("verifier-gate", "test", "verifier-gate"),
    Entry("release", "release", "release"),
    Entry("release-observer", "release", "release-observer"),
)}
