# SH-756: Story complexity and dispatch policy

Stories have low, medium, or high complexity. Missing values use medium and
remain unassessed. An explicit choice sets the assessment flag. Complexity
does not change queue order or priority. Events remain the source of truth.

The canonical rubric is `story help complexity-rubric`. Low describes local,
known work; medium describes bounded work across components; high describes
uncertain work or interacting invariants. Choose the highest applicable level.

| Complexity | Codex model | Claude model | Effort |
|---|---|---|---|
| low | gpt-6-astra | fable | medium |
| medium | gpt-6-astra | fable | high |
| high | gpt-6-astra | fable | xhigh |

Resolve model and effort independently: dispatch flag, environment, project
policy, installation policy, built-in value. Automatic models are limited to
Sol/Astra and Opus/Fable. Explicit choices retain the full provider catalog.
Store policies in SQLite, outside browser-token preferences. CLI and web use
the same service. Reset removes an override and restores inheritance.

Custom launch commands keep control of their model and effort. Automatic
mapping is skipped and reported. Explicit incompatible selectors remain errors.
Guarded continuation retains its captured choices; other launches use current
policy. Engine lanes resolve each executable child's complexity, never an epic's.

Expose complexity through CLI, JSON, decomposition, plugins, web, and TUI.
Preserve it through replay, migration, export/import, archive, and transfer.
Web settings edit installation and project policies and show inherited values.
Dispatch previews and results identify effective values and their sources.

Test the new and directly affected paths. The central verifier owns the full
suite and submission. No release or version change belongs to this story.
