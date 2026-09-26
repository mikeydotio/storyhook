//! The durable executable search path carried by a login agent (SH-819).

use std::ffi::OsStr;

/// A PATH whose entries keep their meaning when a child changes directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPath(String);

impl ExecutionPath {
    /// Validates an installer or daemon PATH without changing its search order.
    ///
    /// # Errors
    /// Returns a diagnostic when the value cannot safely name a durable PATH.
    pub fn parse(value: Option<&OsStr>) -> Result<Self, String> {
        let value = value.ok_or("PATH is not set")?;
        let value = value.to_str().ok_or("PATH is not UTF-8")?;
        if value.is_empty() {
            return Err("PATH is empty".into());
        }
        if !value.chars().all(xml_character) {
            return Err("PATH contains a character that XML cannot represent".into());
        }
        for entry in std::env::split_paths(value) {
            if !entry.is_absolute() {
                return Err(format!(
                    "PATH entry `{}` is empty or relative; use nonempty absolute directories",
                    entry.display()
                ));
            }
        }
        Ok(Self(value.to_owned()))
    }

    /// The original, validated spelling, suitable for a child's environment.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Reads PATH from the string dictionary this agent's writer emits.
///
/// `None` denotes a legacy agent with no explicit PATH.
///
/// # Errors
/// Unsupported or ambiguous dictionaries are reported, never guessed at.
pub fn registered_path(text: &str) -> Result<Option<ExecutionPath>, String> {
    let mut rest = space(text);
    take(&mut rest, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    take(
        &mut rest,
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
    );
    if !(take(&mut rest, "<plist version=\"1.0\">") || take(&mut rest, "<plist>"))
        || !take(&mut rest, "<dict>")
    {
        return Err("unrecognized plist root dictionary".into());
    }
    let mut keys = std::collections::HashSet::new();
    let mut path = None;
    while !take(&mut rest, "</dict>") {
        let key = element(&mut rest, "key")?;
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate plist key `{key}`"));
        }
        if key == "EnvironmentVariables" {
            path = environment(&mut rest)?;
        } else if space(rest).starts_with("<string>") {
            element(&mut rest, "string")?;
        } else if take(&mut rest, "<array>") {
            while !take(&mut rest, "</array>") {
                element(&mut rest, "string")?;
            }
        } else if !(take(&mut rest, "<true/>") || take(&mut rest, "<false/>")) {
            return Err(format!("unsupported plist value for `{key}`"));
        }
    }
    if !take(&mut rest, "</plist>") || !space(rest).is_empty() {
        return Err("incomplete plist or trailing content".into());
    }
    Ok(path)
}

fn environment(rest: &mut &str) -> Result<Option<ExecutionPath>, String> {
    if !take(rest, "<dict>") {
        return Err("EnvironmentVariables is not a string dictionary".into());
    }
    let mut keys = std::collections::HashSet::new();
    let mut path = None;
    while !take(rest, "</dict>") {
        let key = element(rest, "key")?;
        if !keys.insert(key.clone()) {
            return Err(format!("duplicate EnvironmentVariables key `{key}`"));
        }
        let value = element(rest, "string")?;
        if key == "PATH" {
            path = Some(ExecutionPath::parse(Some(OsStr::new(&value)))?);
        }
    }
    Ok(path)
}

fn take(rest: &mut &str, literal: &str) -> bool {
    if let Some(tail) = space(rest).strip_prefix(literal) {
        *rest = tail;
        true
    } else {
        false
    }
}

fn space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t', '\r', '\n'])
}

fn xml_character(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

/// Consume one string-valued element. Nested markup is outside our format.
fn element(rest: &mut &str, tag: &str) -> Result<String, String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let inner = space(rest)
        .strip_prefix(&open)
        .ok_or_else(|| format!("expected {open} in login agent plist"))?;
    let (raw, tail) = inner
        .split_once(&close)
        .ok_or_else(|| format!("missing {close} in login agent plist"))?;
    if raw.contains('<') || raw.contains("]]>") || !raw.chars().all(xml_character) {
        return Err("invalid string or nested markup in login agent plist".into());
    }
    let mut decoded = String::new();
    // XML normalizes literal line endings before expanding character references.
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut remaining = normalized.as_str();
    while let Some((prefix, entity)) = remaining.split_once('&') {
        decoded.push_str(prefix);
        let (name, tail) = entity.split_once(';').ok_or("unterminated XML entity")?;
        let character = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            number => {
                let parsed = if let Some(hex) = number.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16)
                        .ok()
                        .filter(|_| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                } else {
                    number.strip_prefix('#').and_then(|decimal| {
                        decimal
                            .parse()
                            .ok()
                            .filter(|_| decimal.bytes().all(|byte| byte.is_ascii_digit()))
                    })
                };
                parsed
                    .and_then(char::from_u32)
                    .filter(|c| xml_character(*c))
                    .ok_or_else(|| format!("unsupported XML entity &{name};"))?
            }
        };
        decoded.push(character);
        remaining = tail;
    }
    decoded.push_str(remaining);
    *rest = tail;
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn execution_path_preserves_search_order_and_literal_spellings() {
        let value =
            "/custom links/工具 & <bin>:/missing/bin:/custom links/工具 & <bin>:/usr/bin:/bin";
        assert_eq!(
            ExecutionPath::parse(Some(OsStr::new(value)))
                .unwrap()
                .as_str(),
            value
        );
    }

    #[test]
    fn execution_path_rejects_unprovable_or_xml_invalid_values() {
        for value in [
            None,
            Some(""),
            Some("bin:/usr/bin"),
            Some("/bin:"),
            Some(":/bin"),
            Some("/bin::/usr/bin"),
            Some("/bin:\u{1}"),
            Some("/bin/\u{fffe}"),
        ] {
            assert!(
                ExecutionPath::parse(value.map(OsStr::new)).is_err(),
                "accepted {value:?}"
            );
        }
        assert!(ExecutionPath::parse(Some(OsStr::from_bytes(b"/bad\xff"))).is_err());
    }

    fn document(contents: &str) -> String {
        format!(
            "<plist version=\"1.0\"><dict><key>EnvironmentVariables</key><dict>{contents}</dict></dict></plist>"
        )
    }

    #[test]
    fn registered_path_distinguishes_legacy_and_invalid_agents() {
        assert_eq!(
            registered_path("<plist><dict></dict></plist>").unwrap(),
            None
        );
        assert_eq!(
            registered_path(&document("<key>LANG</key><string>en_US.UTF-8</string>")).unwrap(),
            None
        );
        for contents in [
            "<key>PATH</key><array><string>/bin</string></array>",
            "<key>PATH</key><string>/bin</string><key>PATH</key><string>/usr/bin</string>",
            "<key>PATH</key><string>relative</string>",
            "<key>PATH</key><string>/bin",
            "<key>nested</key><dict><key>PATH</key><string>/bin</string></dict>",
            "<key>PATH</key><string>/bin&unknown;</string>",
        ] {
            assert!(
                registered_path(&document(contents)).is_err(),
                "accepted {contents}"
            );
        }
        let duplicated = document("<key>PATH</key><string>/bin</string>").replace(
            "</dict></plist>",
            "<key>EnvironmentVariables</key><dict></dict></dict></plist>",
        );
        assert!(registered_path(&duplicated).is_err());
    }

    #[test]
    fn registered_path_requires_one_complete_root_dictionary() {
        let valid = document("<key>PATH</key><string>/bin</string>");
        for invalid in [
            format!("<!--{valid}-->"),
            format!("<plist><dict><key>nested</key><dict>{valid}</dict></dict></plist>"),
            valid.replace("</plist>", ""),
            valid.replace(
                "</dict></plist>",
                "<key>Environment&#86;ariables</key><dict></dict></dict></plist>",
            ),
            format!("{valid}trailing garbage"),
            valid.replace("/bin", "&#x+2f;bin"),
            valid.replace("/bin", "&#+47;bin"),
            valid.replace("<dict>", "<dict>\u{a0}"),
            "not a plist".to_string(),
        ] {
            assert!(registered_path(&invalid).is_err(), "accepted {invalid}");
        }
    }
}
