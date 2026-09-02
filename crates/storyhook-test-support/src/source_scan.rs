//! Reading a rule out of Rust source rather than out of prose about it.

/// `text` with Rust comments removed, so a scan is answered by code rather than
/// by the writing around it.
///
/// **Load-bearing rather than tidy, and this crate has three separate proofs of
/// it.** `daemon_containment`'s own doc comment spells `env_clear()` while
/// explaining who needs it, and `src/env/git_env.rs` discusses that call at
/// length in four more; a scan over raw text reads every one as a call site and
/// reports a file that has none. `tests/dashboard_focus_coverage.rs` paid for
/// the same thing one language over, where two real CSS selectors were
/// swallowed into the prose of the comment above them. And
/// `tests/fixture_isolation.rs` was flagged by *itself* the first time it ran
/// against a tree that had it committed: its module doc names the very call it
/// forbids, in order to explain why.
///
/// That last one carries a second lesson worth keeping: a scan derived over
/// `git ls-files` **cannot see its own file until that file is tracked**, so it
/// passes while it is being written and fails on the commit that adds it. There
/// is no way around that; there is only knowing it.
///
/// Nesting is handled, because Rust's block comments nest. A string literal
/// containing `//` is not — no scan in this repository needs that, and a
/// stripper that had to lex string literals would be a parser rather than a
/// filter.
pub fn without_rust_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_line_comment = false;
    let mut block_depth = 0usize;
    while let Some(c) = chars.next() {
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
                out.push(c);
            }
            continue;
        }
        if block_depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_depth -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_depth += 1;
            } else if c == '\n' {
                out.push(c);
            }
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    in_line_comment = true;
                    continue;
                }
                Some('*') => {
                    chars.next();
                    block_depth = 1;
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}
