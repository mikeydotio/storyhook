//! No skill or reference file restates a fact only `bin/story.sh` (or the
//! real `story` CLI) is allowed to own (SH-308).
//!
//! Six of the plugin's nine standalone skills used to drive the `story` CLI
//! directly from prose, each hand-copying a detail the CLI itself could
//! change out from under it — `skills/story-handoff/SKILL.md` said the
//! handoff window defaulted to `2h` a full release after the CLI's real
//! default (`DEFAULT_HANDOFF_HOURS`) had been `24h` everywhere else, because
//! commit `04ac259`'s fix touched neither the skill nor a script that didn't
//! exist yet. SH-308 rewrote every skill that had one of these facts to stop
//! stating it — `--since`/`--stale` are now passed through to `bin/story.sh`
//! only when the caller gives one, never restated as a number in prose — so
//! this scan can assert the class is gone with **zero exceptions**, the
//! same "derive from tracked files, no allowlist" convention
//! `tests/dead_public_surface.rs` and `tests/harness_path_entries.rs` use:
//! a hand-maintained list of "the files that are still allowed to do this"
//! is exactly the kind of thing that lets the class re-accumulate unnoticed.
//!
//! Two related but separately-caused symptoms of the same root problem,
//! checked together because both are cheap, both are string scans, and both
//! disappeared for the same reason (SH-308's router split):
//!
//! 1. **A hand-copied numeric default.** A tracked skill or reference file
//!    stating `` `Nd` ``/`` `Nh` ``/`` `Nw` ``/`` `Nm` `` beside the word
//!    "default" is a value with two owners the moment the CLI's own default
//!    changes. (`STORY_STALE_THRESHOLD`'s own `3d` used to be one of
//!    these too, in `skills/story-context/SKILL.md` and
//!    `skills/story-triage/SKILL.md` — both now point at the real value in
//!    the script's own output instead of restating it.)
//! 2. **The double-nested JSON access pattern.** `.story.story.<field>` and
//!    `.stories[].story.<field>` are the raw shape every `story` command's
//!    JSON output carries, which is exactly what routing through
//!    `bin/story.sh` exists to hide — `story.sh` always returns a flat
//!    `{ok, display, …}` envelope instead. A skill that still has to
//!    explain this nesting is a skill that is still parsing the CLI's raw
//!    JSON by hand. `references/cli-reference.md` and
//!    `references/workflow-patterns.md` were the last two files teaching
//!    this pattern; both were orphaned (nothing loaded either — confirmed
//!    by grepping every skill and hook) and were deleted as part of SH-308
//!    rather than kept as permanent exceptions to this scan. That was a
//!    council decision; its trail lived in the worktree that made the call
//!    and is gone (SH-363), and it belonged to no story, so the sentence
//!    above is the whole of the record — delete an orphan, never exempt it.

use std::path::Path;

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every tracked skill/reference markdown file's full text, keyed by its
/// path relative to the repository root. `.council/` is untracked
/// (`.gitignore`) and never reaches `git ls-files`, so a council's own
/// working notes can never trip this scan.
fn tracked_skill_and_reference_docs(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--",
            "plugins/story/skills/*/SKILL.md",
            "plugins/story/references/*.md",
        ])
        .output()
        .expect("listing this repository's tracked skill/reference docs");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let files: Vec<(String, String)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect();

    // A sanity floor, not an arbitrary number: this plugin ships 10 skills
    // (the router plus 9 standalone) and 3 references today. A pathspec
    // that silently matched nothing would make every assertion below pass
    // by proving nothing at all, exactly the failure mode
    // `dead_public_surface.rs`'s own header warns about.
    assert!(
        files.len() >= 10,
        "expected at least 10 tracked skill/reference docs under plugins/story/, found {} \
         -- the `git ls-files` pathspec is probably broken, not the tree",
        files.len()
    );
    files
}

/// True iff `token` is exactly digits followed by exactly one ASCII letter
/// (`3d`, `2h`, `7w`, `30m`) -- the shape every duration flag in this CLI
/// accepts (`story help handoff`'s own examples: `4h`, `1d`).
fn looks_like_a_duration(token: &str) -> bool {
    let Some((digits, unit)) = token.split_at_checked(token.len().saturating_sub(1)) else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && unit.len() == 1
        && unit.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
}

/// Every backtick-quoted token in `text`, e.g. `` `3d` `` -> `"3d"``.
fn backtick_quoted_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        tokens.push(&rest[..close]);
        rest = &rest[close + 1..];
    }
    tokens
}

#[test]
fn no_skill_or_reference_restates_a_numeric_cli_default() {
    let docs = tracked_skill_and_reference_docs(repo_root());
    let mut offenders = Vec::new();

    for (file, text) in &docs {
        for (idx, line) in text.lines().enumerate() {
            if !line.to_ascii_lowercase().contains("default") {
                continue;
            }
            for token in backtick_quoted_tokens(line) {
                if looks_like_a_duration(token) {
                    offenders.push(format!("{file}:{}: `{token}`", idx + 1));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a skill or reference doc restates a numeric CLI default -- pass the flag through to \
         `bin/story.sh` only when the caller gives one instead, the same fix SH-308 applied \
         everywhere else (the 04ac259 bug this class produced: `skills/story-handoff/\
         SKILL.md` said `2h` a full release after the CLI's real default became `24h`):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn no_skill_or_reference_teaches_the_double_nested_json_shape() {
    let docs = tracked_skill_and_reference_docs(repo_root());
    let mut offenders = Vec::new();

    for (file, text) in &docs {
        for (idx, line) in text.lines().enumerate() {
            if line.contains(".story.story.") || line.contains(".stories[].story.") {
                offenders.push(format!("{file}:{}", idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a skill or reference doc still teaches the raw `.story.story.<field>` / \
         `.stories[].story.<field>` JSON shape -- route the underlying `story` call through \
         `bin/story.sh` instead, which always returns a flat {{ok, display, …}} envelope so no \
         skill has to explain the CLI's own nesting:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn shared_skills_and_references_are_provider_neutral() {
    let docs = tracked_skill_and_reference_docs(repo_root());
    let forbidden = ["CLAUDE_PLUGIN_ROOT", "ARGUMENTS", "AskUserQuestion"];
    let mut offenders = Vec::new();

    for (file, text) in &docs {
        for (idx, line) in text.lines().enumerate() {
            for token in forbidden {
                if line.contains(token) {
                    offenders.push(format!("{file}:{}: {token}", idx + 1));
                }
            }
            if line.contains("`/story") || line.contains(" /story ") {
                offenders.push(format!("{file}:{}: user-facing slash invocation", idx + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "shared skill material contains a provider-only variable, tool, or invocation; put a \
         genuinely provider-specific exception under plugins/story/adapters instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn every_skill_uses_common_frontmatter() {
    let skills_root = repo_root().join("plugins/story/skills");
    let mut checked = 0;

    for entry in std::fs::read_dir(&skills_root).expect("listing packaged skills") {
        let entry = entry.expect("reading a packaged skill entry");
        let path = entry.path().join("SKILL.md");
        if !path.is_file() {
            continue;
        }
        checked += 1;
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let mut parts = text.splitn(3, "---");
        assert_eq!(
            parts.next(),
            Some(""),
            "{} must start with YAML",
            path.display()
        );
        let frontmatter = parts
            .next()
            .unwrap_or_else(|| panic!("{} has no YAML frontmatter", path.display()));
        let keys: Vec<&str> = frontmatter
            .lines()
            .filter_map(|line| line.split_once(':').map(|(key, _)| key.trim()))
            .collect();
        assert_eq!(
            keys,
            ["name", "description"],
            "{} must use the Claude/Codex common-denominator frontmatter",
            path.display()
        );
    }

    assert_eq!(checked, 9, "the shared package must expose all nine skills");
}
