# SH-557 — Project-owned AGENTS roadmap

- `templates::agents_md` now contains only reusable Storyhook instructions.
- Root `AGENTS.md` fences that generated payload with the existing Storyhook
  sentinels and keeps this repository's mini-roadmap outside the managed block.
- The drift contract compares only the managed bytes and rejects missing,
  duplicate, or reversed boundaries.
- Scaffold coverage rejects a repository roadmap or hard-coded `SH-` identity.
- Setup-helper coverage proves refresh preserves local roadmap bytes and
  refuses malformed ownership boundaries without rewriting the file.
- RED: the new root contract failed because the sentinels were absent.
- GREEN: 67 directly impacted Rust tests, targeted Clippy, formatting, Bash
  syntax, diff checks, the local build, and the focused setup-helper test pass.
- The centralized verifier owns the full suite, merge, completion, and cleanup.
- Remaining roadmap: audit the story lifecycle in SH-560, then resume
  authenticated attachment serving and upload in SH-315.
