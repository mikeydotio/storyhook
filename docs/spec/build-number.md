# Build numbers (SH-732)

`VERSION` identifies a release. `BUILD` identifies a version change or a
build-for-use invocation within a checkout's history. It starts at zero and
contains a canonical unsigned 64-bit decimal integer followed by a newline.
Numbered builds reserve the next
number before compilation; failure consumes the reservation.

The 3.0.1 release starts production numbering at 100. `BUILD` participates in
the cache fingerprints for Clippy and every test or build leg that consumes the
compiled identity. Changing only the number must invalidate those results;
source-formatting results remain reusable (SH-733).

`make release-build`, `make install`, and standalone release asset assembly
allocate numbers. One install allocates once. All platform artifacts in one
release assembly share a number. Debug builds, tests, checks, and dry runs only
read the counter. A source archive must contain its assigned BUILD value.

A Python wrapper holds a separate advisory lock through compilation and artifact
collection. BUILD replacement is atomic and durable. The command inherits the
lock descriptor so a killed wrapper cannot release ownership while compilation
continues. Concurrent operations in one checkout serialize. Independent clones
do not coordinate; these numbers are not globally unique identifiers.

Local installs leave BUILD modified. Each semver bump reserves the next number
from the current BUILD and stages it in the same version commit (SH-749).
The pre-bump `sync-build-number.sh` hook uses the same locked allocator and holds
its lock through staging. It compares OLD_VERSION with NEW_VERSION independently
of the bump level. A same-version operation does not allocate or stage BUILD.
Missing hook context and invalid,
missing, unwritable, or exhausted counters abort the operation. A failed staging
or later hook consumes its reservation; a retry must not reuse that number.

For example, if an installation left BUILD at 103, a version change allocates
104 even if HEAD still records 102. Release preparation does not allocate a
second time: it requires the version commit to contain an advanced BUILD, then
passes that reservation to all platform builders. Assembly checks the
reservation before using it. A later local installation reserves another number.
No build wrapper makes Git commits. Branch switches, historical checkouts, and
ordinary Cargo builds do not allocate; VERSION changes go through semver.

The installed semver 3.9.4 CLI invokes pre-bump hooks for patch, minor, and major
bumps, but skips them for explicit `set` and `init` operations. SH-749 records
this upstream limitation. Those operations need the same pre-hook lifecycle
before they can satisfy the automatic build-number contract; direct hook tests
alone do not establish that the CLI calls them.

The compiled display version is `3.0.0 (N)`. CLI output keeps bare semver as its
second whitespace field and retains the optional Git-content stamp. Semver used
by compatibility checks, release lookup, and plugin paths remains unchanged.
Structured identities carry optional numeric metadata; older records have an
unknown number, never the current process's number. Schema and protocol numbers
are unrelated to this counter.

Tests exercise the production wrapper and build script in isolated directories,
including failed builds, concurrent allocation, lock inheritance, source
archives, and stale reservations. The installed CLI and daemon are never used as
test artifacts. The centralized verifier owns full-suite validation.
