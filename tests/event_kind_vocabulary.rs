//! No fixture invents its own spelling for `events.kind` (SH-364).
//!
//! `seed_a_v14_project` in `tests/store_migrations.rs` seeded `'story_created'`
//! and `'story_priority_set'` — snake_case spellings no storyhook writer has
//! ever produced. `domain::event_kind` emits PascalCase, `write.rs` inserts its
//! return value verbatim, and `is_known_event_kind` is case-sensitive on
//! purpose. Migration 15's backfill joins on `seq = head_seq` and never reads
//! `kind`, so the invented vocabulary sat there harmlessly for as long as
//! nothing consulted it.
//!
//! SH-359 then added a migration whose predicate is *entirely* `kind`. A seat
//! on its council read this fixture as the nearest precedent — correctly, it
//! was — and proposed the snake_case spelling for the migration itself. A test
//! that seeds a spelling and a migration that matches the same spelling agree
//! with each other, pass, and match **zero rows** in every real store: every
//! story silently backfilling to "never assessed". Review caught it; the suite
//! could not have.
//!
//! This is SH-263's shape one layer over — *the gate was right; the fixture
//! lied to it* — and it is confined to this one column by construction. Every
//! other vocabulary column the store has is fenced by the schema itself:
//! `superstate` and `priority` carry `CHECK (… IN (…))` constraints, and
//! `priority_rank` is tied to `priority` by a third. `events.kind` is the one
//! column that deliberately **cannot** have that CHECK — SH-54 requires a store
//! written by a *newer* storyhook to stay readable by this one, so an
//! unrecognised kind is retained rather than rejected. This file is the check
//! that column would otherwise get for free.
//!
//! Derived over `git ls-files` rather than a hand-maintained list of sites, in
//! the style `dead_public_surface.rs` and `release_targets.rs` established: a
//! list is exactly what let this go unnoticed once already. It covers
//! production SQL as well as fixtures — a schema migration that hand-writes a
//! kind (migration 9 does, and correctly) is the same hazard with a longer
//! blast radius.
//!
//! **Known blind spots, and why each fails safe (silence, never a false
//! alarm):**
//! - A kind supplied by a bound parameter, a column reference or any other
//!   non-literal expression is not judged. That is the shape this file exists
//!   to encourage, and one test depends on it: `store_rebuild.rs` writes a kind
//!   this binary genuinely cannot decode, and asserts separately that it stays
//!   undecodable.
//! - A statement assembled at runtime from fragments is invisible; only text
//!   that reads as SQL in a tracked file is scanned.
//! - An `UPDATE` that computes a kind rather than assigning a literal to it is
//!   not judged, for the same reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use storyhook::domain::{EVENT_KINDS, is_known_event_kind};

/// One `events.kind` value written as a SQL string literal, and where.
#[derive(Debug)]
struct KindLiteral {
    value: String,
    file: String,
    line: usize,
}

/// The repository root, which is this package's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every tracked file's text, keyed by its path relative to the repository
/// root, skipping the ones that are not UTF-8 (images, fixtures, archives).
fn tracked_sources(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("listing this repository's tracked files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).ok()?.to_string();
            let text = std::fs::read_to_string(root.join(&relative)).ok()?;
            Some((relative, text))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Turning a source file into something scannable
// ---------------------------------------------------------------------------

/// `text` with Rust string escapes resolved and whole-line comments dropped,
/// plus the source line each surviving byte came from.
///
/// Three transformations, each of which a naive scan gets wrong:
///
/// - A Rust line continuation (`\` at end of line) swallows the newline and
///   the next line's indentation, so `"INSERT INTO events \` … `(project_id`
///   is one statement in the database and two lines in the file. It becomes a
///   single space here.
/// - `\"` becomes `"`, so an embedded JSON payload reads as the database sees
///   it and its commas stay inside the SQL string literal that contains them.
/// - A line whose first non-space characters are `//` or `--` is dropped
///   entirely. Without this the scan would parse its own prose — this file
///   names the statement it looks for several times above.
fn scannable(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut lines: Vec<usize> = Vec::with_capacity(text.len());
    let mut line = 1usize;

    for source_line in text.split_inclusive('\n') {
        let trimmed = source_line.trim_start();
        if !(trimmed.starts_with("//") || trimmed.starts_with("--")) {
            let mut chars = source_line.chars().peekable();
            while let Some(character) = chars.next() {
                if character == '\\' {
                    match chars.peek() {
                        Some('\n') => {
                            chars.next();
                            while matches!(chars.peek(), Some(' ' | '\t')) {
                                chars.next();
                            }
                            push(&mut out, &mut lines, ' ', line);
                            continue;
                        }
                        Some('"') | Some('\\') | Some('\'') => {
                            let escaped = chars.next().expect("peeked");
                            push(&mut out, &mut lines, escaped, line);
                            continue;
                        }
                        _ => {}
                    }
                }
                push(&mut out, &mut lines, character, line);
            }
        } else {
            // Keep the line break so a comment cannot glue two statements
            // together, but drop the comment's own text.
            push(&mut out, &mut lines, '\n', line);
        }
        line += 1;
    }

    (out, lines)
}

/// Appends `character`, recording `line` for each of its bytes so an offset
/// into the scannable text can be reported as a source line.
fn push(out: &mut String, lines: &mut Vec<usize>, character: char, line: usize) {
    out.push(character);
    lines.resize(out.len(), line);
}

/// The next SQL word at or after `from`, lowercased, with the offset just past
/// it — or `None` if only punctuation and whitespace remain.
fn next_word(sql: &str, from: usize) -> Option<(String, usize)> {
    let bytes = sql.as_bytes();
    let mut index = from;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') {
        index += 1;
    }
    (index > start).then(|| (sql[start..index].to_ascii_lowercase(), index))
}

/// The offset just past the next word at or after `from`, if that word is
/// `want` — the building block for matching a keyword sequence without caring
/// how much whitespace separates it.
fn expect_word(sql: &str, from: usize, want: &str) -> Option<usize> {
    let (word, after) = next_word(sql, from)?;
    (word == want).then_some(after)
}

/// The offset of the first non-whitespace byte at or after `from`.
fn skip_whitespace(sql: &str, from: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut index = from;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

/// Walks a comma-separated value list starting at `from`, returning each
/// element with its absolute offset, plus the offset where the list ended.
///
/// Commas inside parentheses or inside a SQL string literal do not separate
/// elements — the first is how a subquery or `json_object(…)` stays one
/// element, the second is how an embedded JSON payload does. The list ends at
/// the parenthesis that closes it (a `VALUES (…)` tuple), or at a `;` or `"`
/// (an `INSERT … SELECT`, whose list runs to the end of the statement, which in
/// a Rust source file is the end of the string literal holding it).
fn value_list(sql: &str, from: usize) -> (Vec<(usize, String)>, usize) {
    let bytes = sql.as_bytes();
    let mut elements = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut element_start = from;
    let mut index = from;

    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == b'\'' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' => quoted = true,
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            b';' | b'"' if depth == 0 => break,
            b',' if depth == 0 => {
                elements.push(element_at(sql, element_start, index));
                element_start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    elements.push(element_at(sql, element_start, index));
    (elements, index)
}

/// One list element, trimmed, carrying the offset of its first real character.
fn element_at(sql: &str, start: usize, end: usize) -> (usize, String) {
    let raw = &sql[start..end];
    let lead = raw.len() - raw.trim_start().len();
    (start + lead, raw.trim().to_string())
}

/// Every SQL string literal inside `expression`, with its offset relative to
/// the start of the expression.
fn literals_in(expression: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let bytes = expression.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'\'' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while end < bytes.len() && bytes[end] != b'\'' {
            end += 1;
        }
        if end >= bytes.len() {
            break;
        }
        found.push((start, expression[start..end].to_string()));
        index = end + 1;
    }
    found
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

/// What one file's scan found: the kind literals, and how many statements
/// wrote the `kind` column at all.
struct Scan {
    literals: Vec<KindLiteral>,
    statements: usize,
}

/// Every `events.kind` literal in `text`, and the number of statements that
/// address that column.
///
/// The statement count is what makes a broken parser noisy rather than silent:
/// a scan that has stopped recognising statements reports no literals, which
/// on its own is indistinguishable from a clean tree.
fn scan(file: &str, text: &str) -> Scan {
    let (sql, lines) = scannable(text);
    let lower = sql.to_ascii_lowercase();
    let mut scan = Scan {
        literals: Vec::new(),
        statements: 0,
    };

    for (offset, _) in lower.match_indices("insert") {
        if !starts_word(&lower, offset) {
            continue;
        }
        scan_insert(
            file,
            &sql,
            &lower,
            &lines,
            offset + "insert".len(),
            &mut scan,
        );
    }
    for (offset, _) in lower.match_indices("update") {
        if !starts_word(&lower, offset) {
            continue;
        }
        scan_update(file, &sql, &lines, offset + "update".len(), &mut scan);
    }
    scan
}

/// Whether `offset` begins a word rather than ending one — so `reinsert` is
/// not read as `insert`.
fn starts_word(sql: &str, offset: usize) -> bool {
    offset == 0 || {
        let previous = sql.as_bytes()[offset - 1];
        !(previous.is_ascii_alphanumeric() || previous == b'_')
    }
}

/// Reads an `INSERT INTO events (…) VALUES (…)` or `INSERT INTO events (…)
/// SELECT …` beginning just past the `INSERT` keyword at `from`.
fn scan_insert(file: &str, sql: &str, lower: &str, lines: &[usize], from: usize, scan: &mut Scan) {
    let Some(after_into) = expect_word(sql, from, "into") else {
        return;
    };
    let Some(after_table) = expect_word(sql, after_into, "events") else {
        return;
    };
    let Some(kind_index) = kind_column_index(sql, after_table) else {
        return;
    };
    let list_end = sql[after_table..]
        .find(')')
        .map(|relative| after_table + relative + 1)
        .expect("a column list that parsed must have a closing parenthesis");
    scan.statements += 1;

    let body = skip_whitespace(sql, list_end);
    if lower[body..].starts_with("values") {
        let mut cursor = body + "values".len();
        while let Some(relative) = sql[cursor..].find('(') {
            let open = cursor + relative;
            if sql[cursor..open]
                .chars()
                .any(|character| !character.is_whitespace() && character != ',')
            {
                break;
            }
            let (elements, end) = value_list(sql, open + 1);
            collect(file, lines, &elements, kind_index, scan);
            cursor = end + 1;
        }
    } else if lower[body..].starts_with("select") {
        let (elements, _) = value_list(sql, body + "select".len());
        collect(file, lines, &elements, kind_index, scan);
    }
}

/// The position of `kind` in the parenthesised column list at `from`, or
/// `None` if there is no list there or it does not name that column.
///
/// A `None` here is how the scan skips the many other things the word
/// sequence `insert into events` can appear in — this file's own search
/// needle among them.
fn kind_column_index(sql: &str, from: usize) -> Option<usize> {
    let open = skip_whitespace(sql, from);
    if sql.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let close = open + sql[open..].find(')')?;
    sql[open + 1..close]
        .split(',')
        .position(|column| column.trim().eq_ignore_ascii_case("kind"))
}

/// Reads an `UPDATE events SET kind = '…'` beginning just past the `UPDATE`
/// keyword at `from`.
///
/// Only a literal assigned directly to `kind` is taken, and only the leading
/// one: an assignment's right-hand side runs to the next top-level comma,
/// which for the last assignment swallows the `WHERE` clause, and a timestamp
/// or a slug in there is not a candidate kind.
fn scan_update(file: &str, sql: &str, lines: &[usize], from: usize, scan: &mut Scan) {
    let Some(after_table) = expect_word(sql, from, "events") else {
        return;
    };
    let Some(after_set) = expect_word(sql, after_table, "set") else {
        return;
    };
    let (assignments, _) = value_list(sql, after_set);
    for (offset, assignment) in assignments {
        let Some((target, expression)) = assignment.split_once('=') else {
            continue;
        };
        if !target.trim().eq_ignore_ascii_case("kind") {
            continue;
        }
        scan.statements += 1;
        let lead = expression.len() - expression.trim_start().len();
        let value = expression.trim_start();
        if !value.starts_with('\'') {
            continue;
        }
        let Some((relative, literal)) = literals_in(value).into_iter().next() else {
            continue;
        };
        let at = offset + target.len() + 1 + lead + relative;
        scan.literals.push(KindLiteral {
            value: literal,
            file: file.to_string(),
            line: lines[at],
        });
    }
}

/// Records every literal in the element that lands in the `kind` column.
///
/// Every literal rather than only a bare one, so a `CASE`, a `COALESCE` or any
/// other expression choosing between spellings is judged on all of them — the
/// element's boundaries are exact here, so nothing but a candidate kind can be
/// inside it.
fn collect(
    file: &str,
    lines: &[usize],
    elements: &[(usize, String)],
    kind_index: usize,
    scan: &mut Scan,
) {
    let Some((offset, expression)) = elements.get(kind_index) else {
        return;
    };
    for (relative, literal) in literals_in(expression) {
        scan.literals.push(KindLiteral {
            value: literal,
            file: file.to_string(),
            line: lines[offset + relative],
        });
    }
}

/// Every kind literal in the tree, and the total number of statements that
/// write the column.
fn scan_repository() -> (Vec<KindLiteral>, usize) {
    let root = repo_root();
    let mut literals = Vec::new();
    let mut statements = 0usize;
    for (file, text) in tracked_sources(&root) {
        let lower = text.to_ascii_lowercase();
        if !(lower.contains("insert") || lower.contains("update")) {
            continue;
        }
        let found = scan(&file, &text);
        literals.extend(found.literals);
        statements += found.statements;
    }
    (literals, statements)
}

// ---------------------------------------------------------------------------
// The invariants
// ---------------------------------------------------------------------------

#[test]
fn every_event_kind_written_as_a_literal_is_one_this_binary_emits() {
    let (literals, _) = scan_repository();

    let strangers: Vec<&KindLiteral> = literals
        .iter()
        .filter(|literal| !is_known_event_kind(&literal.value))
        .collect();

    assert!(
        strangers.is_empty(),
        "these `events.kind` literals are spellings no storyhook writer emits, \
         so anything reading `kind` agrees with the fixture and disagrees with \
         every real store (SH-364):\n{}\n\nDerive the string from \
         `storyhook::domain::event_kind(&StoryEvent::…)` instead of typing it. \
         The kinds this binary writes are: {}",
        strangers
            .iter()
            .map(|literal| format!(
                "  {}:{} wrote '{}'",
                literal.file, literal.line, literal.value
            ))
            .collect::<Vec<_>>()
            .join("\n"),
        EVENT_KINDS.join(", ")
    );
}

#[test]
fn the_scan_still_reads_the_statements_it_is_meant_to_read() {
    let (literals, statements) = scan_repository();

    // A positive control rather than a count: migration 9 hand-writes
    // `'StoryTypeSet'` into the `kind` column of an `INSERT … SELECT`, and a
    // migration is immutable once shipped. A parser that has stopped
    // recognising statements reports an empty tree, which reads exactly like a
    // clean one — this is what tells the two apart.
    let control = literals
        .iter()
        .find(|literal| literal.file.ends_with("0009_type_emoji_and_drop_task.sql"));
    assert!(
        control.is_some_and(|literal| literal.value == "StoryTypeSet"),
        "migration 9 writes 'StoryTypeSet' into `events.kind`; the scan must \
         find it, or it is not reading statements any more and its silence \
         means nothing. Found instead: {control:?}"
    );

    assert!(
        statements >= 6,
        "only {statements} statements write `events.kind` — the scan has \
         stopped matching, or the tree shrank dramatically. This is a floor \
         against a silently broken parser, not a census."
    );
}
