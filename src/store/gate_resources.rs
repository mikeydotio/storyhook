//! Validation for additive host observations. No record confers execution authority.

use serde_json::Value;

fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
        .ok_or_else(|| format!("invalid resource evidence {key}"))
}

fn number(row: &Value, key: &str, minimum: i64) -> Result<i64, String> {
    row[key]
        .as_i64()
        .filter(|v| *v >= minimum)
        .ok_or_else(|| format!("invalid resource evidence {key}"))
}

fn vector(row: &Value, minimum: i64) -> Result<(), String> {
    if row.as_object().is_none_or(|r| r.len() != 2) {
        return Err("invalid resource vector".into());
    }
    if minimum != 0 || !row.get("cpu").is_some_and(Value::is_null) {
        number(row, "cpu", minimum)?;
    }
    number(row, "memory", minimum)?;
    Ok(())
}

/// Require an exact attempt, execution and positive submission generation.
pub(crate) fn binding(
    row: &Value,
    attempt: &str,
    execution: &str,
    generation: Option<i64>,
) -> Result<(), String> {
    if row["attempt_id"].as_str() != Some(attempt)
        || row["execution_id"].as_str() != Some(execution)
        || generation.is_none_or(|g| g <= 0 || row["generation"].as_i64() != Some(g))
    {
        return Err("foreign resource attempt, execution or generation".into());
    }
    Ok(())
}

/// Validate one observation without treating it as execution or certification proof.
pub(crate) fn validate(
    row: &Value,
    attempt: &str,
    execution: &str,
    generation: Option<i64>,
) -> Result<(), String> {
    if row["version"].as_u64() != Some(1) {
        return Err("unsupported resource evidence version".into());
    }
    for key in ["authority", "host", "boot"] {
        text(row, key)?;
    }
    let policy = text(row, "policy")?;
    if policy.len() != 64 || !policy.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid resource policy digest".into());
    }
    number(row, "sequence", 1)?;
    number(row, "at", 0)?;
    let kind = text(row, "event")?;
    if !matches!(kind, "pressure" | "sensor-error") {
        text(row, "lease")?;
        text(row, "project")?;
        if !matches!(text(row, "work")?, "build" | "test" | "release" | "repair") {
            return Err("invalid resource work class".into());
        }
        if !row["parent"].is_null() {
            text(row, "parent")?;
        }
        vector(&row["resources"], 1)?;
        binding(&row["binding"], attempt, execution, generation)?;
    } else if !row["lease"].is_null() {
        return Err("host-wide resource event carries a lease".into());
    }
    match kind {
        "grant" | "subgrant" | "denial" => {
            number(row, "wait_ms", 0)?;
        }
        "usage" => {
            vector(&row["sample"], 0)?;
            vector(&row["peaks"], 0)?;
            for key in ["cpu", "memory"] {
                if row["sample"][key].as_i64() > row["peaks"][key].as_i64() {
                    return Err("resource peak is smaller than its sample".into());
                }
            }
        }
        "cancel" | "cleanup" | "quarantine" | "recovery" | "release" | "pressure"
        | "sensor-error" => {
            text(row, "reason")?;
        }
        "attach" => {
            text(row, "execution")?;
        }
        "request" => {}
        _ => return Err(format!("unknown resource event {kind}")),
    }
    Ok(())
}

/// Check retained ordering separately from delivery: exact replay is not stored twice.
pub(crate) fn ordered(records: &[Value]) -> Result<(), String> {
    let mut last = std::collections::BTreeMap::<&str, &Value>::new();
    for row in records {
        let authority = text(row, "authority")?;
        let sequence = number(row, "sequence", 1)?;
        if let Some(old) = last.insert(authority, row) {
            if sequence <= number(old, "sequence", 1)? {
                return Err("resource evidence is duplicated or out of order".into());
            }
            if row["host"] != old["host"]
                || row["policy"] != old["policy"]
                || (row["boot"] == old["boot"] && row["at"].as_i64() < old["at"].as_i64())
            {
                return Err("resource authority identity or clock changed".into());
            }
        }
    }
    Ok(())
}
