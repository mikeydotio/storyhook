# SH-826: a fixture lost Cargo's mutable binary

The central gate for PR 882 rejected tree
`af001d82d2a4611cd1df62af6e382ca2238f49e0`. The pinned-configuration test
received `infrastructure-failure` instead of `project-fault` because
`github-access.sh` could not execute `target/debug/story`. The configuration
check had not failed; its prerequisite executable was absent.

The merge-gate fixture had five direct references to the compile-time Cargo
artifact path. Those references bypassed `story_binary()`, the existing
process-owned hard-link lease. A raw Cargo path can disappear or change after
the test starts. The log proves absence, but does not identify the operation
that removed the artifact.

All integration-fixture launch, `STORY_BIN`, generated-script, and installation
copy sources now use the lease. The sibling sweep found 23 direct references
in 11 files. No production command or gate configuration changed.
The submission-receipt shell fixture now gets its own Cargo-shaped directory
with a binary copied from the lease. It no longer derives Cargo's target
directory from an executable path that can be inside a lease directory.

The new fixture-isolation guard failed on the raw references before repair.
The existing binary snapshot regression now removes its own source artifact
after replacing it and proves that the lease still runs the original build.
It never removes the real Cargo artifact, which other tests can be using.
Together these checks pin both the lease mechanism and its use by fixtures.
