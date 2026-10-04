//! Conservative source convention for browser-harness process ownership (SH-805).

/// Direct call anchors understood by the exact command-ownership inventory.
pub const CALLS: [&str; 2] = ["execFileSync(", "execFile("];

/// Return every subprocess call offset, or a path/line diagnostic for an escape.
pub fn calls(path: &str, source: &str) -> Result<Vec<usize>, String> {
    let error = |at: usize, why: &str| {
        format!(
            "{path}:{}: {why} (SH-805)",
            source[..at].bytes().filter(|b| *b == b'\n').count() + 1
        )
    };
    let mut declarations = Vec::new();
    let mut imported = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if let Some(names) = declaration(line) {
            for name in names {
                if imported.contains(&name) {
                    return Err(error(offset, "duplicate subprocess binding"));
                }
                imported.push(name);
            }
            declarations.push(offset..offset + line.len());
        }
        offset += line.len();
    }
    let in_declaration = |at| declarations.iter().any(|range| range.contains(&at));
    for (at, _) in source.match_indices("child_process") {
        if !in_declaration(at) {
            return Err(error(
                at,
                "use a single-line named import or const destructuring of execFile/execFileSync from node:child_process; aliases and other APIs are forbidden",
            ));
        }
    }
    let mut found = Vec::new();
    let bytes = source.as_bytes();
    for name in ["execFile", "execFileSync"] {
        for (at, _) in source.match_indices(name) {
            let end = at + name.len();
            if (at > 0 && identifier_byte(bytes[at - 1]))
                || (end < bytes.len() && identifier_byte(bytes[end]))
                || in_declaration(at)
            {
                continue;
            }
            let before = source[..at].trim_end();
            let previous_word = before
                .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
                .next();
            if !imported.iter().any(|binding| binding == name)
                || bytes.get(end) != Some(&b'(')
                || before.ends_with(['.', '/', '#'])
                || matches!(previous_word, Some("new" | "function"))
            {
                return Err(error(
                    at,
                    "subprocess bindings may only appear in their declaration or a bare direct call with no space before '('",
                ));
            }
            found.push(at);
        }
    }
    found.sort_unstable();
    Ok(found)
}

/// Supported declarations are intentionally narrower than JavaScript syntax.
fn declaration(line: &str) -> Option<Vec<String>> {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let (rest, esm) = if let Some(rest) = compact.strip_prefix("import{") {
        (rest, true)
    } else {
        (compact.strip_prefix("const{")?, false)
    };
    let (names, tail) = rest.split_once('}')?;
    let allowed_tail = if esm {
        ["from\"node:child_process\";", "from'node:child_process';"]
    } else {
        [
            "=require(\"node:child_process\");",
            "=require('node:child_process');",
        ]
    };
    if !allowed_tail.contains(&tail) {
        return None;
    }
    let names: Vec<_> = names
        .trim_end_matches(',')
        .split(',')
        .map(str::to_owned)
        .collect();
    names
        .iter()
        .all(|name| matches!(name.as_str(), "execFile" | "execFileSync"))
        .then_some(names)
}

/// Identifier boundaries avoid mistaking a longer helper name for a binding.
fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 128
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_declarations_preserve_raw_call_offsets() {
        for declaration in [
            "import { execFile, execFileSync } from \"node:child_process\";",
            "const { execFileSync, execFile } = require('node:child_process');",
        ] {
            let source = format!(
                "{declaration}\nexecFile(storyBinary(), [], callback);\nexecFileSync(storyBinary(), []);"
            );
            assert_eq!(
                calls("e2e/helper.ts", &source).unwrap(),
                [
                    source.find("execFile(").unwrap(),
                    source.find("execFileSync(").unwrap()
                ]
            );
        }
        assert!(
            calls("e2e/helper.ts", "const match = /x/.exec(text);")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unsupported_module_access_never_disappears_from_the_audit() {
        for source in [
            "import * as cp from 'node:child_process'; cp.spawn('story');",
            "import cp from 'node:child_process'; cp.execFile('story');",
            "import { execFile as run } from 'node:child_process'; run('story');",
            "import { execFile } from 'child_process';",
            "const cp = require('node:child_process'); cp.spawn('story');",
            "const {execFile: run} = require('node:child_process');",
            "require('node:child_process').execFile('story');",
            "const cp = await import('node:child_process');",
            "export { execFile } from 'node:child_process';",
            "export * from 'node:child_process';",
            "import { execFile, execFile } from 'node:child_process';",
            "/* commentary\n */ import * as cp from 'node:child_process'; cp.spawn('story');",
        ] {
            assert!(
                calls("e2e/bypass.ts", source).is_err(),
                "accepted: {source}"
            );
        }
        for name in ["spawn", "spawnSync", "exec", "execSync", "fork"] {
            for source in [
                format!("import {{ {name} }} from 'node:child_process';"),
                format!("const {{ {name} }} = require('node:child_process');"),
            ] {
                assert!(
                    calls("e2e/bypass.ts", &source).is_err(),
                    "accepted: {source}"
                );
            }
        }
    }

    #[test]
    fn imported_functions_cannot_be_aliased_or_called_indirectly() {
        for name in ["execFile", "execFileSync"] {
            for usage in [
                format!("promisify({name})('story');"),
                format!("const run = {name}; run('story');"),
                format!("use({name});"),
                format!("export {{ {name} }};"),
                format!("{name}.call(null, 'story');"),
                format!("{name}.apply(null, ['story']);"),
                format!("{name}.bind(null)('story');"),
                format!("({name})('story');"),
                format!("cp.{name}('story');"),
                format!("cp.\n{name}('story');"),
                format!("cp./*comment*/{name}('story');"),
                format!("{name}?.('story');"),
                format!("{name} ('story');"),
                format!("{name}/*comment*/('story');"),
                format!("new {name}('story');"),
                format!("new /*comment*/ {name}('story');"),
                format!("// prose\n/* prefix\n */ const run = {name};"),
            ] {
                let source = format!("import {{ {name} }} from 'node:child_process';\n{usage}");
                let error = calls("e2e/bypass.ts", &source).expect_err(&source);
                assert!(error.starts_with("e2e/bypass.ts:"), "{error}");
            }
            assert!(calls("e2e/bypass.ts", &format!("{name}(storyBinary(), []);")).is_err());
        }
    }
}
