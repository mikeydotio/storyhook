# proptest regressions

`store_properties.txt` appears here the first time a property test in
`tests/store_properties.rs` finds a counterexample. It records the shrunk input
so that every later run replays it before generating anything new.

**Commit it.** A generated counterexample is a real defect that a real input
sequence produced; leaving it uncommitted means the next contributor's run
starts from a different seed and the bug goes back into hiding. Delete a line
only when the case it pins has become impossible to express — not when it
starts passing.

The path is set explicitly in `store_properties.rs` rather than left to
proptest's default, which searches for a `lib.rs` or `main.rs` beside the test
file, finds neither in an integration-test crate, and silently falls back to a
different filename.
