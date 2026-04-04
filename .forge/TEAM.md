# Agent Team Roster

## Project Type
CLI tool (Rust, event-sourced)

## Active Agents
### Always Active
- domain-researcher
- software-architect
- senior-engineer
- qa-engineer
- project-manager
- devils-advocate
- technical-writer
- generator
- evaluator
- reviewer
- validator
- triager

### Conditionally Activated
- ux-designer: NO — This is a CLI tool. CLI UX decisions are well-established from the codebase patterns and research. No GUI/web interface involved.
- security-researcher: NO — This feature adds story classification metadata. No auth, no external APIs, no user input that could be exploited. Types are validated against a config file. Attack surface is negligible.
- accessibility-engineer: NO — CLI tool with no visual UI changes beyond text output. The TUI already exists and this feature adds text badges/progress bars following existing patterns. No WCAG considerations.

## Rationale

This is a **feature addition to an existing Rust CLI tool** that follows well-established internal patterns. The implementation is primarily pattern-following (states.toml → types.toml, StoryPrioritySet → StoryTypeSet, story phase → story epic). The core team (architect, engineer, QA, PM, devil's advocate, writer) is sufficient.

Security is not a concern because:
- Types are validated against a local TOML config file
- No network operations involved in type management
- No privilege escalation or auth changes
- Event log is append-only (existing security model)

UX design is not needed because:
- CLI conventions are well-established (`--type` flag, subcommand sugar)
- The `story phase` pattern provides the exact UX template
- Progress bar display is a solved problem in terminal tools
