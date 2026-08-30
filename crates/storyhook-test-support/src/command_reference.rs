//! Grammar support for checking documented `story ...` invocations.
//!
//! README examples and shipped help topics describe the same CLI grammar. This
//! module keeps their placeholder substitution and optional/alternative
//! expansion in one place, then drives the same parser pipeline as `main.rs`.

use storyhook::cli::{parse_invocation, split_global_flags};

/// The expanded result of one documented invocation.
pub enum DocumentedInvocation {
    /// The command is dispatched before `parse_invocation` in a real process.
    ParsedElsewhere,
    /// Concrete argv variants ready for [`parse_documented_argv`].
    Argvs(Vec<Vec<String>>),
}

/// Strips a trailing ` # comment` outside double quotes.
pub fn strip_trailing_comment(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut in_quotes = false;
    for i in 0..chars.len() {
        match chars[i] {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes && i > 0 && chars[i - 1] == ' ' => {
                return chars[..i].iter().collect::<String>().trim_end().to_string();
            }
            _ => {}
        }
    }
    line.to_string()
}

#[derive(Debug, Clone)]
enum SubTok {
    Word(String),
    Quoted(String),
}

#[derive(Debug, Clone)]
enum Piece {
    Sub(SubTok),
    Optional(Vec<Vec<SubTok>>),
    Choice(Vec<Vec<SubTok>>),
}

fn split_alternatives(inner: &str) -> Vec<Vec<SubTok>> {
    inner.split(" | ").map(simple_tokenize).collect()
}

fn simple_tokenize(s: &str) -> Vec<SubTok> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            tokens.push(SubTok::Quoted(chars[start..j].iter().collect()));
            i = j + 1;
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            tokens.push(SubTok::Word(chars[start..i].iter().collect()));
        }
    }
    tokens
}

fn tokenize_top(s: &str) -> Vec<Piece> {
    let chars: Vec<char> = s.chars().collect();
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        match chars[i] {
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                pieces.push(Piece::Sub(SubTok::Quoted(chars[start..j].iter().collect())));
                i = j + 1;
            }
            '[' => {
                let start = i + 1;
                let mut j = start;
                let mut depth = 1;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        j += 1;
                    }
                }
                let inner: String = chars[start..j].iter().collect();
                pieces.push(Piece::Optional(split_alternatives(&inner)));
                i = j + 1;
            }
            '(' => {
                let start = i + 1;
                let mut j = start;
                let mut depth = 1;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        j += 1;
                    }
                }
                let inner: String = chars[start..j].iter().collect();
                pieces.push(Piece::Choice(split_alternatives(&inner)));
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && chars[i] != '['
                    && chars[i] != '('
                    && chars[i] != '"'
                {
                    i += 1;
                }
                pieces.push(Piece::Sub(SubTok::Word(chars[start..i].iter().collect())));
            }
        }
    }
    pieces
}

fn placeholder(token: &str) -> Option<&'static str> {
    Some(match token {
        "<id>" | "<a>" | "<b>" | "<story-id>" | "<epic-id>" | "<expected>" | "<blocker>" => "SH-1",
        "<run-id>" => "run-1",
        "<n>" | "<N>" => "1",
        "<PORT>" => "3456",
        "<PREFIX>" => "SH",
        "<NEW-PREFIX>" => "ZZ",
        "<json>" => "{\"title\":\"x\"}",
        "<relationship-type>" | "<relation>" => "relates-to",
        "<name <email>>" => "Ada Lovelace <ada@example.com>",
        "<github-handle>" => "adalovelace",
        "<slug>" | "<state>" | "<state-slug>" => "todo",
        "<slug,slug,...>" => "todo,in-progress,done",
        "<topic>" => "storage",
        "<duration>" => "3d",
        "<date>" => "2026-01-01",
        "<key>" => "greeting",
        "<value>" => "hello",
        "<level>" => "high",
        "<levels>" => "high,medium",
        "<labels>" | "<name>" => "backend",
        "<labels-csv>" | "<csv>" => "backend,api",
        "<text>" => "example text",
        "<title>" => "Example title",
        "<reason>" => "example reason",
        "<comment>" => "example comment",
        "<member>" => "mikey",
        "<query>" => "auth",
        "<file>" => "export.json",
        "<path>" | "<PATH>" => "/tmp/example",
        "<url>" => "https://github.com/acme/widgets",
        "<NAME>" => "Example Project",
        "<glyph>" => "🐛",
        "<event_type>" => "pre-commit",
        "<request-id>" => "req-1",
        "<crash-id>" => "crash-1",
        "<target>" => "claude",
        _ => return None,
    })
}

fn substitute(token: &str) -> Result<String, String> {
    if !token.starts_with('<') {
        return Ok(token.to_string());
    }
    placeholder(token).map(str::to_string).ok_or_else(|| {
        format!(
            "unknown placeholder `{token}` — add it to command_reference::placeholder with a \
             value that is actually legal for its flag (an unvalidated default could make a \
             documented invocation pass while naming a value the CLI rejects)"
        )
    })
}

/// Commands intentionally answered before the ordinary invocation parser.
pub const PARSED_ELSEWHERE: &[(&str, &str)] = &[
    ("tui", "src/main.rs dispatches it ahead of parse_invocation"),
    ("mcp", "src/main.rs dispatches it ahead of parse_invocation"),
];

fn skip_leading_globals(pieces: &[Piece]) -> usize {
    let mut i = 0;
    while let Some(Piece::Sub(SubTok::Word(w))) = pieces.get(i) {
        match w.as_str() {
            "--project" | "--store-path" | "--deadline" => i += 2,
            "--quiet" | "--no-hooks" => i += 1,
            _ => break,
        }
    }
    i
}

fn expand_phrase(phrase: &[SubTok]) -> Result<Vec<Vec<String>>, String> {
    let mut variants: Vec<Vec<String>> = vec![Vec::new()];
    for tok in phrase {
        let alts: Vec<String> = match tok {
            SubTok::Quoted(q) => vec![substitute(q)?],
            // `...` is usage notation for repetition. One occurrence is
            // enough to prove the repeated flag and its value parse.
            SubTok::Word(w) if w == "..." => continue,
            SubTok::Word(w) => {
                if w == "|" {
                    return Err(
                        "a top-level ` | ` alternation was found outside any [ ] or ( ) \
                         group — split this into separate lines, one invocation per line"
                            .to_string(),
                    );
                }
                let mut out = Vec::new();
                for alt in w.split('|') {
                    out.push(substitute(alt)?);
                }
                out
            }
        };
        let mut next = Vec::new();
        for variant in &variants {
            for alt in &alts {
                let mut extended = variant.clone();
                extended.push(alt.clone());
                next.push(extended);
            }
        }
        variants = next;
    }
    Ok(variants)
}

/// Expands one documented invocation with its leading `story` already removed.
pub fn expand_documented_invocation(
    raw_without_story: &str,
) -> Result<DocumentedInvocation, String> {
    let pieces = tokenize_top(raw_without_story);

    let verb_at = skip_leading_globals(&pieces);
    if let Some(Piece::Sub(SubTok::Word(word))) = pieces.get(verb_at) {
        if word.starts_with('<') && word.ends_with('>') {
            return Ok(DocumentedInvocation::ParsedElsewhere);
        }
        if PARSED_ELSEWHERE.iter().any(|(verb, _)| verb == word) {
            return Ok(DocumentedInvocation::ParsedElsewhere);
        }
    }

    let mut required: Vec<Vec<Vec<String>>> = Vec::new();
    let mut optional: Vec<Vec<Vec<String>>> = Vec::new();

    for piece in &pieces {
        match piece {
            Piece::Sub(tok) => required.push(expand_phrase(std::slice::from_ref(tok))?),
            Piece::Choice(alts) => {
                let mut phrases = Vec::new();
                for alt in alts {
                    phrases.extend(expand_phrase(alt)?);
                }
                required.push(phrases);
            }
            Piece::Optional(alts) => {
                let mut phrases = Vec::new();
                for alt in alts {
                    phrases.extend(expand_phrase(alt)?);
                }
                optional.push(phrases);
            }
        }
    }

    let combos: usize = required.iter().map(Vec::len).product();
    if combos > 16 {
        return Err(format!(
            "{combos} required-alternative combinations is more than this line should ever \
             need — split it rather than let the cross product grow unbounded"
        ));
    }

    let mut base_forms: Vec<Vec<String>> = vec![Vec::new()];
    for position in &required {
        let mut next = Vec::new();
        for base in &base_forms {
            for phrase in position {
                let mut combined = base.clone();
                combined.extend(phrase.iter().cloned());
                next.push(combined);
            }
        }
        base_forms = next;
    }

    let mut argvs = Vec::new();
    for base in &base_forms {
        argvs.push(base.clone());
        for group in &optional {
            for phrase in group {
                let mut with_one = base.clone();
                with_one.extend(phrase.iter().cloned());
                argvs.push(with_one);
            }
        }
    }

    Ok(DocumentedInvocation::Argvs(argvs))
}

/// Runs the real CLI parser pipeline for one concrete documented argv.
pub fn parse_documented_argv(argv: &[String]) -> Result<(), String> {
    let (_flags, filtered) = split_global_flags(argv).map_err(|error| error.to_string())?;
    parse_invocation(&filtered)
        .map(|_| ())
        .map_err(|error| error.to_string())
}
