# Storyhook: Domain Research & Strategic Recommendations

**Date:** 2026-03-22
**Scope:** Market research, competitive analysis, and feature recommendations for Storyhook — an AI-optimized project management CLI for solo engineers and small teams.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Market Context & Opportunity](#2-market-context--opportunity)
3. [Competitive Landscape](#3-competitive-landscape)
4. [Evaluation of Current Storyhook Implementation](#4-evaluation-of-current-storyhook-implementation)
5. [Recommended Fixes (Current Implementation)](#5-recommended-fixes-current-implementation)
6. [Strategic Feature Roadmap](#6-strategic-feature-roadmap)
7. [Anti-Recommendations (What NOT to Build)](#7-anti-recommendations-what-not-to-build)
8. [Positioning & Community Strategy](#8-positioning--community-strategy)

---

## 1. Executive Summary

Storyhook occupies a genuine gap in the developer tooling market. Research validates three core claims:

1. **The market needs an AI-agent-native PM tool.** Existing tools (Jira, Linear, GitHub Issues) are designed for human browsers, not AI queries. The few AI-native competitors (Beads, Claude Task Master, CCPM) are early-stage and leave significant whitespace.
2. **CLI-first, git-native is the right architecture.** Developer community consensus overwhelmingly favors tools that live where code lives. The rise of agentic CLI tools (Claude Code, Codex CLI, Gemini CLI) makes terminal-native project management a natural integration point.
3. **Solo developers and small teams are underserved.** 65% of PM tool dissatisfaction stems from over-complexity. Developers want tools that "disappear" — minimal ceremony, maximum signal.

Storyhook's current implementation (v0.1.0) demonstrates solid architectural foundations: event-sourced storage, structured JSON output, relationship modeling, and concurrency safety. The primary gaps are **discoverability** (no way for AI agents to ask "what should I do next?"), **integration** (no MCP server), and **workflow automation** (no spec decomposition or status derivation).

The recommendations below are ordered by expected impact on adoption and differentiation.

---

## 2. Market Context & Opportunity

### 2.1 The AI-Assisted Development Shift

A March 2026 survey of 906 engineers found Claude Code is the most-used AI coding tool at 46% share, followed by Cursor (19%) and Copilot (9%). Anthropic's 2026 Agentic Coding Trends Report documents real-world results: TELUS achieved 30% faster delivery with 500K hours saved; Augment Code compressed 4-8 month projects to 2 weeks.

The dominant workflow has become **spec-driven development**: write a spec/PRD, decompose into tasks, feed one task at a time to AI agents. GitHub released Spec Kit as an open-source toolkit for this pattern. Addy Osmani coined "waterfall in 15 minutes" to describe how AI enables exhaustive upfront planning at near-zero cost.

**Key insight:** AI agents need structured, queryable project state — not visual boards. The tool that best serves both the human planning phase and the AI execution phase wins.

### 2.2 What Developers Actually Want from PM Tools

From HackerNews, Reddit, and developer surveys, the consistent themes are:

| Priority | Feature | Why It Matters |
|----------|---------|----------------|
| 1 | **Speed** | Linear's success is attributed almost entirely to being fast. Jira's decline correlates with performance. |
| 2 | **Lives with code** | Tools integrated into GitHub/terminal win over standalone apps. |
| 3 | **Keyboard-first** | Command palettes, vim-like nav, zero-mouse workflows. |
| 4 | **Opinionated defaults** | "Just works" beats infinite configurability. |
| 5 | **Data portability** | Plain text, open formats. Fear of vendor lock-in is real. |
| 6 | **Minimal ceremony** | Solo devs resist anything that feels like corporate process. |

Only 35% of project managers express satisfaction with current systems. 75% cite concerns about reliability, usability, and integration.

### 2.3 The "50 First Dates" Problem

Every new AI agent session starts fresh. Steve Yegge (Beads creator) identifies this as the central challenge: agents have amnesia across sessions. The PM tool becomes the memory layer — it must:

- Persist project state across sessions
- Surface actionable context without consuming the entire context window
- Track what was done, what's blocked, and what's ready
- Support session handoff (generating prompts for successor agents)

### 2.4 Format Performance for AI Consumption

Controlled testing across models found:

| Format | Accuracy | Token Efficiency |
|--------|----------|-----------------|
| YAML | Best (62% on GPT-5 Nano) | Moderate |
| Markdown | Good (54%) | Best (34-38% fewer tokens than JSON) |
| JSON | Moderate (50%) | Baseline |
| XML | Worst (44%) | 80% more tokens than Markdown |

**Implication:** Storyhook's JSONL storage is correct for machine writes, but human/AI-readable output should favor Markdown for reports and YAML for structured data.

---

## 3. Competitive Landscape

### 3.1 Direct Competitors (AI-Native CLI PM)

| Tool | Storage | Key Innovation | Key Gap |
|------|---------|----------------|---------|
| **Beads** (Yegge, 18.7K stars) | JSONL in `.beads/`, git-native | `bd ready` surfaces unblocked tasks; hash IDs prevent merge conflicts; semantic memory decay for closed issues | Node.js (not a single binary); no relationship types beyond 4; no custom states |
| **Claude Task Master** (Toledano) | JSON tasks file | PRD decomposition; complexity scoring; TDD autopilot | NPM dependency; no git-native storage; single-tool focus (Claude) |
| **CCPM** (Automaze) | GitHub Issues + worktrees | Spec-driven workflow; parallel execution in isolated worktrees | Requires GitHub; heavyweight; not portable |
| **ai-todo** (fxstein) | Plain Markdown | Zero-config; works with every AI tool | No structured data; no dependencies; no query interface |

### 3.2 Adjacent Competitors (Developer PM with AI Features)

| Tool | Type | AI Story | Key Gap for AI Agents |
|------|------|----------|----------------------|
| **Linear** | SaaS | AI labeling, duplicate detection, MCP server | SaaS-only; AI features are augmentation, not core; no CLI-first |
| **Plane** | Open-source SaaS | MCP server; self-hostable | Heavy infra; web-first |
| **GitHub Issues/Projects** | SaaS | Copilot integration | Limited PM features; no local-first |
| **Taskwarrior** | CLI | Urgency algorithm (`task next`) | No AI integration; painful sync; no team features |

### 3.3 Storyhook's Competitive Position

**Unique advantages Storyhook holds or could hold:**
- **Rust single binary**: Faster install, no runtime dependencies (vs. Beads/Node.js, Claude Task Master/NPM)
- **Event-sourced storage**: Full audit trail with JSONL open + SQLite archive (no competitor does this)
- **Rich relationship model**: 16 relationship types with auto-inverses, derived ancestry, cycle detection (far beyond any competitor)
- **Concurrency safety**: File locking with timeout (Beads uses SQLite WAL; most others have no story)
- **Custom states with superstates**: Flexible but constrained (competitors either have fixed states or unconstrained)

**Critical gaps vs. competitors:**
- No "next task" / "ready task" command (Beads' killer feature)
- No MCP server (Linear, Plane, Beads all have or are building one)
- No spec/PRD decomposition workflow
- No priority/urgency system
- No labels/tags
- No session handoff support

---

## 4. Evaluation of Current Storyhook Implementation

### 4.1 Architecture Assessment

| Aspect | Rating | Notes |
|--------|--------|-------|
| **Language choice (Rust)** | Excellent | Single binary, fast, safe. Right choice for a CLI tool. |
| **Module structure** | Good | Clean separation (cli/app/domain/storage/output). Could benefit from a `query` layer. |
| **Storage model** | Excellent | Event-sourcing with JSONL is brilliant for AI audit trails. SQLite archive is pragmatic. |
| **Data model** | Very Good | Relationship system is sophisticated. Missing: priority, labels, effort. |
| **CLI design** | Good | Concise positional syntax. Missing: `next`, `summary`, `import`. |
| **JSON output** | Very Good | Stable schema with `--json`. Missing: bulk operations, project-level summaries. |
| **Error handling** | Good | Typed errors with exit codes. Good for scripting. |
| **Test coverage** | Adequate | 17 tests covering happy paths. Needs: edge cases, concurrent access, large-scale. |
| **Documentation** | Good | README covers basics. Needs: AI integration guide, MCP docs, AGENTS.md template. |

### 4.2 AI-Readiness Assessment

| Capability | Status | Impact |
|------------|--------|--------|
| Structured JSON output | Present | High — essential for agent parsing |
| Deterministic exit codes | Present | High — enables scripting |
| Atomic operations | Present | High — prevents corruption |
| Queryable task state | **Missing** | **Critical** — agents need "what's next?" |
| MCP server | **Missing** | **Critical** — primary integration mechanism for 2026 |
| Bulk operations | **Missing** | High — agents often create/update multiple items |
| Context-efficient summary | **Missing** | High — agents need project overview without loading everything |
| Session handoff | **Missing** | Medium — supports multi-session agent workflows |
| Spec decomposition | **Missing** | Medium — automates the planning-to-tasks pipeline |

### 4.3 Specific Implementation Issues

**1. No priority system**
Stories have states and relationships but no priority. Without priority, there's no way to answer "what's most important?" — a question both humans and AI agents ask constantly.

**2. No labels or tags**
No way to categorize stories by type (bug, feature, chore), component, or any user-defined dimension. This limits filtering and makes project-level reporting impossible.

**3. Relationship richness is underutilized**
The 16-relationship-type system is more sophisticated than any competitor, but there's no command that leverages it to compute actionable insights (e.g., critical path, blocked chains, ready tasks).

**4. `story list` is limited**
Only filters by `--state`, `--assignee`, and `--flagged`. Cannot filter by: relationship type, creation date range, last-updated, keyword search, or custom criteria. AI agents need richer query capabilities.

**5. No project-level overview**
No command produces a summary of project health: story counts by state, blocked items, stale items, velocity, etc. Agents need this for planning.

**6. No bulk/batch operations**
Creating 10 stories from a decomposed spec requires 10 sequential commands. Agents should be able to pipe a structured input (JSON/YAML) into a bulk create.

**7. Hardcoded ID prefix**
The `SH-` prefix is hardcoded. Projects should be able to configure their prefix (e.g., `API-`, `WEB-`).

**8. No way to reopen archived stories**
Once a story transitions to CLOSED and is archived to SQLite, it cannot be reopened. This is a reasonable design decision but should be documented explicitly and potentially reconsidered.

---

## 5. Recommended Fixes (Current Implementation)

These are improvements to the existing v0.1.0 codebase, ordered by priority.

### 5.1 Critical (Should fix before next feature work)

**F1: Add `story next` command**
Surface the highest-priority unblocked story. Algorithm: filter to OPEN stories with no `awaiting` status and no unresolved `follows`/`starts-after`/`child-of` dependencies on OPEN stories. Among those, sort by: explicit priority (once added), then relationship depth (leaf nodes first), then creation order.

This is the single most impactful feature for AI agent usability. Beads' `bd ready` is its killer feature precisely because it answers the question every agent asks at session start.

```
$ story next
$ story next --json
$ story next --count 5    # top 5 ready stories
```

**F2: Add priority field**
Add a `priority` field to stories: `critical`, `high`, `medium`, `low`, `none` (default). Simple enum, stored as a `StoryPrioritySet` event. Used by `story next` for sorting and by `story list` for filtering.

```
$ story SH-1 priority high
$ story list --priority critical,high
```

**F3: Add `story summary` command**
Produce a compact project overview optimized for AI context windows. Output should include: total story counts by state, blocked stories, flagged stories, recent activity, and a list of ready-to-work items. Both human-readable (Markdown) and `--json` formats.

```
$ story summary
$ story summary --json
```

### 5.2 High Priority

**F4: Add labels/tags**
Allow arbitrary string labels on stories. Stored as `StoryLabelsChanged` events. Filterable in `story list`.

```
$ story SH-1 label bug,backend
$ story SH-1 label --remove bug
$ story list --label backend
```

**F5: Add bulk create from structured input**
Accept JSON or YAML on stdin to create multiple stories at once, optionally with relationships between them. Returns created IDs. Essential for spec decomposition workflows.

```
$ cat stories.json | story import
$ story import stories.yaml
```

**F6: Configurable project prefix**
Allow projects to set a custom ID prefix in `project.toml`. Default remains `SH-`.

```toml
# .storyhook/project.toml
prefix = "API"
```

**F7: Add `story search` command**
Full-text search across story titles, comments, and labels. Both open and archived stories.

```
$ story search "authentication"
$ story search "auth" --state done --json
```

### 5.3 Medium Priority

**F8: Richer `story list` filtering**
Add filters for: `--label`, `--priority`, `--created-after`, `--updated-after`, `--blocked` (has unresolved dependencies), `--ready` (no blockers, OPEN state). Composable with AND logic.

**F9: Story templates**
Allow defining reusable story templates in `.storyhook/templates/`. Useful for recurring task types (bug reports, feature stories, etc.) and for AI agents that need to create stories with consistent structure.

**F10: Reopen archived stories**
Add `story <id> reopen` to move a story from the SQLite archive back to an open JSONL file, resetting its state to a specified OPEN state.

**F11: Export/import for migration**
Full project export to a single JSON/YAML file and import from same. Enables migration between machines and from other tools.

---

## 6. Strategic Feature Roadmap

These are larger features that define Storyhook's future trajectory, organized into tiers.

### Tier 1: AI Integration Layer (Highest Impact)

**R1: MCP Server**
Build an MCP server that exposes Storyhook operations as tools. This is the single most important strategic feature — it's how AI tools (Claude Code, Cursor, Codex) will natively integrate with Storyhook without shelling out to the CLI.

MCP tool surface should include:
- `storyhook_list_stories` (with filters)
- `storyhook_get_story` (by ID)
- `storyhook_create_story` (with optional relationships, labels, priority)
- `storyhook_update_story` (state, priority, labels, assignee, awaiting)
- `storyhook_add_comment`
- `storyhook_get_summary` (project overview)
- `storyhook_get_next` (ready tasks)
- `storyhook_search` (full-text)
- `storyhook_get_dependency_graph` (for planning)
- `storyhook_bulk_create` (from structured input)

Implementation options:
- **Option A:** Rust binary with `--mcp` flag that runs as stdio MCP server (simplest, no new dependencies)
- **Option B:** Separate lightweight server process (more flexible, supports SSE/HTTP transports)

Recommendation: Start with Option A (stdio), expand to Option B later. The Plane MCP server supports all three transports and is a good reference.

**R2: Context File Generation**
Auto-generate a `.storyhook/CONTEXT.md` or contribute to `AGENTS.md` with a project state summary. This file gets picked up by AI tools at session start, solving the "50 First Dates" problem.

```
$ story context > AGENTS.md  # or append to existing
$ story context --format yaml
```

The generated context should include:
- Project name and current milestone
- Active story count and state distribution
- Ready-to-work items (top 5)
- Currently blocked items with reasons
- Recent completions (last 7 days)

**R3: Session Handoff**
Generate a structured handoff document when an AI agent session ends. Captures: what was worked on, what changed, what's left, what's blocked. The next session can ingest this to resume with full context.

```
$ story handoff --since "2 hours ago"
$ story handoff --session-id abc123
```

### Tier 2: Workflow Intelligence (High Impact)

**R4: Dependency Graph Analysis**
Leverage the existing relationship system to compute:
- **Critical path**: The longest chain of dependent stories from start to finish
- **Blocked chains**: Stories that are transitively blocked and the root blocker
- **Impact analysis**: "If SH-5 is delayed, what else is affected?"
- **Parallelism opportunities**: Independent story clusters that can be worked simultaneously

```
$ story graph
$ story graph --critical-path
$ story graph --blocked-by SH-5
$ story graph --parallel-groups --json
```

**R5: Spec Decomposition**
Accept a specification document (Markdown) and decompose it into a story tree with relationships. This can be a hybrid approach: Storyhook provides the structure and import mechanism; the AI agent does the actual decomposition.

```
$ story decompose spec.md                    # AI-assisted
$ story decompose spec.md --dry-run          # preview without creating
$ cat decomposed.yaml | story import         # manual pipeline
```

**R6: Smart Status Derivation**
Automatically derive story status from external signals:
- Git activity: detect commits referencing story IDs, auto-transition to "in-progress"
- CI/CD: detect merged PRs, suggest transition to "done"
- Staleness: flag stories with no activity for N days

```
$ story sync-git                             # scan recent commits
$ story list --stale 14d                     # no activity in 14 days
```

### Tier 3: Team & Ecosystem (Medium Impact)

**R7: GitHub Integration**
Bidirectional sync between Storyhook stories and GitHub Issues. Stories can be linked to PRs, and PR merges can trigger state transitions.

```
$ story SH-1 link gh:org/repo#42
$ story sync-github
```

**R8: Minimal Web Viewer**
A read-only web view (single static HTML file generated from project data) for sharing with non-CLI users. Not a full web app — just a generated report.

```
$ story report --html > report.html
$ story serve --port 8080                    # local dev server
```

**R9: Multi-Project Support**
Support multiple `.storyhook/` directories in a monorepo or cross-project dependency tracking.

**R10: Hooks System**
Pre/post hooks for story events (state change, creation, comment). Enables custom automation (Slack notifications, CI triggers, auto-assignment rules).

```toml
# .storyhook/hooks.toml
[on_state_change]
command = "notify-team.sh"

[on_create]
command = "auto-label.sh"
```

---

## 7. Anti-Recommendations (What NOT to Build)

Based on research into what makes PM tools fail, these are features to explicitly avoid:

### 7.1 Do Not Build

| Feature | Why Not |
|---------|---------|
| **Time tracking** | Solo devs and small teams don't use it. It becomes surveillance in team contexts. Only 12% of surveyed solo devs wanted it. |
| **Gantt charts** | Universally cited as unused by small teams. Shape Up explicitly rejects them. |
| **Story points / velocity** | Research shows these get weaponized as performance metrics. Appetite-based planning (Shape Up) is the modern alternative. |
| **Complex custom workflows** | The "Jira trap" — infinite configurability creates admin burden. Storyhook's superstate model (OPEN/CLOSED with custom substates) is the right level of flexibility. |
| **Built-in chat/messaging** | Stay in your lane. Integration with existing tools (Slack, Discord) via hooks is better than building chat. |
| **User authentication / RBAC** | Storyhook is local-first. File system permissions and git access are the auth layer. Adding auth adds complexity without value for the target audience. |
| **AI inside the tool** | Don't embed LLM calls in Storyhook itself. The tool should be AI-*readable* and AI-*writable*, not AI-*powered*. Let the AI agent orchestrate; Storyhook is the data layer. |
| **Mobile app** | Research shows CLI PM tools fail when they try to build mobile. Focus on generating static reports/views that work on mobile browsers instead. |

### 7.2 Design Principles to Maintain

1. **Two-second rule**: Any common operation should complete in under 2 seconds of typing. Don't add flags or ceremony that slow down the fast path.
2. **Opinionated defaults, minimal configuration**: The current default states (`todo`, `done`) are correct. Don't require configuration before first use.
3. **Data portability**: All data must remain human-readable and exportable. Never create a format that requires Storyhook to read.
4. **Single binary, zero infrastructure**: No database servers, no web servers required. The SQLite archive is already the right boundary — don't add Redis, Postgres, or any external service dependency.
5. **Composability over features**: A small tool that pipes well is more valuable than a large tool that does everything. `story list --json | jq '.stories[] | select(.state == "todo")'` should always work.

---

## 8. Positioning & Community Strategy

### 8.1 Positioning Statement

> **Storyhook is the project tracker that AI agents actually use.**
> Git-native. CLI-first. One binary. Zero ceremony.
> Built for developers who ship with AI — not for managers who track developers.

### 8.2 Key Differentiators to Emphasize

1. **Rust single binary** — vs. Beads (Node.js), Claude Task Master (NPM), CCPM (Python). Install is `cargo install storyhook` or a single binary download. No runtime, no dependencies.
2. **Event-sourced audit trail** — No other CLI PM tool offers append-only event history with both human-readable JSONL and efficient SQLite archive. This is a unique technical differentiator.
3. **Rich relationship model** — 16 types with auto-inverses, derived ancestry, cycle detection. Competitors have 4 types at most. This enables dependency graph analysis that no competitor can match.
4. **Superstate architecture** — Custom states mapped to OPEN/CLOSED superstates gives flexibility without the "Jira trap" of unconstrained workflows.

### 8.3 Community Adoption Strategy

**Phase 1: Developer credibility**
- Publish a blog post: "Why We Built Another Task Tracker (and Why It's Different)"
- Submit to HackerNews with a focus on the AI-agent-native angle
- Create an `AGENTS.md` template that shows Storyhook integration with Claude Code
- Publish a video/demo of an AI agent using Storyhook's MCP server to manage a project end-to-end

**Phase 2: Ecosystem integration**
- MCP server listed on the official MCP server registry
- Claude Code skill/hook that auto-syncs with Storyhook
- Cursor rules file that teaches Cursor to use Storyhook
- GitHub Action that syncs Storyhook stories with GitHub Issues

**Phase 3: Community growth**
- Accept contributions for: import/export plugins, additional relationship types, report formats
- Maintain a "Built with Storyhook" showcase of projects using it
- Partner with AI coding tool creators (Beads and Storyhook could be complementary rather than competing, given different architectural approaches)

### 8.4 Success Metrics

| Metric | 3 Months | 6 Months | 12 Months |
|--------|----------|----------|-----------|
| GitHub stars | 500 | 2,000 | 5,000 |
| MCP server installs | — | 500 | 2,000 |
| Weekly active CLI users | 50 | 200 | 1,000 |
| Cargo.io downloads | 200 | 1,000 | 5,000 |

---

## Appendix A: Research Sources

### PM Best Practices & Developer Sentiment
- Solo Developer Project Management Systems 2025 — apatero.com
- Shape Up by Basecamp — basecamp.com/shapeup
- Project Management Software Can't Save You — HN #37749608
- A Critique of Project Management Software — HN #28388564
- PM Tools for Personal Projects — HN #44181517
- Project Management Statistics by Team Size — electroiq.com

### AI-First Development Workflows
- Addy Osmani: My LLM Coding Workflow Going Into 2026 — addyosmani.com
- Claude Code Task Management Guide — claudefa.st
- Beads: Git-Friendly Issue Tracker for AI Agents — betterstack.com
- Teresa Torres: Claude Code + Obsidian Task Management — chatprd.ai
- Claude Task Master — github.com/eyaltoledano/claude-task-master
- Spec-Driven Development with AI — github.blog
- Beyond the Vibes: Coding Assistants and Agents — blog.tedivm.com

### MCP & Integration
- MCP First Anniversary Spec Release — blog.modelcontextprotocol.io
- 2026 MCP Roadmap — blog.modelcontextprotocol.io
- Plane MCP Server — developers.plane.so, github.com/makeplane/plane-mcp-server
- ATLAS MCP Server — github.com/cyanheads/atlas-mcp-server
- Linear MCP expansion — linear.app

### Data Formats for AI
- Best Nested Data Format for LLMs — improvingagents.com
- Markform structured Markdown — github.com/jlevy/markform
- Markdown for Agents — developers.cloudflare.com

### Competitive Tools
- Beads — github.com/steveyegge/beads (18.7K stars)
- CCPM — github.com/automazeio/ccpm
- ai-todo — github.com/fxstein/ai-todo
- Plane — plane.so
- Linear — linear.app
- Dimension — dimension.dev
- Taskwarrior — taskwarrior.org
- dstask — github.com/naggie/dstask

### AI Agent Context & Failure Modes
- How Long Contexts Fail — dbreunig.com
- Mike Mason: AI Coding Agents Jan 2026 — mikemason.ca
- Anthropic 2026 Agentic Coding Trends Report — resources.anthropic.com
- 8 Trends Shaping Software Engineering 2026 — tessl.io
- AI Config Files Guide — deployhq.com
