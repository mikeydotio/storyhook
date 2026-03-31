# GitHub Issues Two-Way Sync

## Vision
Storyhook stories and GitHub Issues stay in sync bidirectionally, giving AI agents a fast local CLI while maintaining visibility for humans and external tools via GitHub Issues. Full fidelity: every field, comment, label, and lifecycle event propagates both directions.

## Problem Statement
Storyhook is local-only. Teams, stakeholders, and integrations that rely on GitHub Issues can't see storyhook-managed work. Conversely, issues created on GitHub don't appear in storyhook for agents to work with. There's no bridge between the two systems.

## Target Users
- Mikey (creator) and AI agents working in repos that use both storyhook and GitHub Issues
- Teams where some members use GitHub Issues directly and others use storyhook via agents

## Key Requirements
- [ ] Two-way sync: local story changes push to GitHub Issues; GitHub Issue changes pull to local stories
- [ ] All stories sync (no opt-in/opt-out per story)
- [ ] Full field mapping: title, state (open/closed), labels, assignee, comments, body, linked PRs, reactions, milestone
- [ ] Storyhook-specific fields (priority, relationships, awaiting) encoded in a visible fenced code block in the issue body
- [ ] Conflict detection via event-based local diffing + GitHub `updated_at` / timeline API for remote changes
- [ ] Manual conflict resolution with interactive CLI prompts; option to open TUI 3-way compare view per conflict
- [ ] Configurable sync mode: `off` | `manual` | `auto` (auto syncs on any story-modifying command)
- [ ] `story github-sync [<id>]` — full project sync or single-story sync
- [ ] `story commit-sync` — renamed from `sync-git` (local git commit scanning, unchanged behavior)
- [ ] Authentication via `STORYHOOK_GITHUB_TOKEN` env var (PAT)
- [ ] HTTP client: `reqwest` with manual GitHub REST/timeline API calls
- [ ] Compile-time feature flag: `github-sync` (default on in releases, opt-out for minimal builds)
- [ ] Sync metadata stored in separate file (`.storyhook/github-sync.toml` or similar), not in event logs
- [ ] Same-repo only: sync targets the GitHub repo detected from git remote
- [ ] Setup: `story init --github` flag OR auto-detect on first `story github-sync` if token is set
- [ ] Initial sync offers interactive choices: import all open issues, selective import, manual linking, or full import + title-match with rollback logging
- [ ] Full lifecycle sync: close/reopen propagates both directions
- [ ] Dry-run support for visibility before committing changes

## Assumptions (Examined)
| Assumption | Challenged? | Status |
|-----------|------------|--------|
| Users have PAT available | Asked — PAT via env var only | Validated |
| One repo per storyhook project is sufficient | Asked — same repo only | Validated |
| All stories should sync | Asked directly | Validated |
| Event sourcing tracks local changes for diffing | Asked — events since last sync = local diff | Validated |
| GitHub timeline API provides adequate remote change detection | Asked — user chose this over full-fetch | Validated |
| Fenced code block is acceptable in issue body | Asked — user prefers visible over hidden | Validated |
| reqwest is acceptable despite being a large dependency | Asked — user chose it over octocrab or gh CLI | Validated |
| Feature flag keeps the dependency optional | Asked — user wants compile-time opt-out | Validated |

## Constraints
- Must work behind `github-sync` cargo feature flag
- PAT-only auth (no OAuth flow, no gh CLI dependency)
- Same-repo only (no cross-repo sync)
- Must rename existing `sync-git` to `commit-sync` (breaking change)
- Sync metadata must NOT go in event logs (separate file)
- Conflict resolution must be interactive (no silent overwrites)
- `story commit-sync` and `story github-sync` must have distinct tab-completion prefixes

## What "Done" Looks Like
1. `story github-sync` pulls remote GitHub Issues into storyhook and pushes local stories to GitHub Issues
2. Editing a story locally and running `story github-sync` updates the corresponding GitHub Issue
3. Editing a GitHub Issue on the web and running `story github-sync` updates the corresponding story
4. When both sides changed the same field, the user is prompted to resolve the conflict interactively
5. `story init --github` sets up sync configuration
6. `sync=auto` causes any story-modifying command to trigger a sync for that story
7. The storyhook code block in GitHub Issue bodies is clean and parseable
8. Closing a story locally closes the GitHub Issue, and vice versa

## Open Questions
- Exact format of the `storyhook` fenced code block (YAML? TOML? custom?)
- How to handle GitHub-side deletions (issue deleted or transferred to another repo)
- Rate limiting strategy for large projects with many stories
- Whether milestone mapping should be 1:1 or more flexible
- How reactions should be represented in storyhook (if at all)
- Whether linked PRs should become storyhook relationships or comments
- How the TUI 3-way compare view should present conflicts

## Prior Art
- GitHub CLI (`gh`) — manages issues from terminal but no local state
- Linear sync tools — bidirectional sync between Linear and GitHub
- Jira-GitHub integrations — typically one-way or webhook-based
- `git-bug` — distributed bug tracker embedded in git (no GitHub sync)
