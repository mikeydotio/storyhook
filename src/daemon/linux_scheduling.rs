//! Linux scheduling evidence bound to a single native process incarnation.

#[cfg(target_os = "linux")]
use super::lifecycle;

/// Effective kernel values, never inferred from the unit's configured policy.
#[cfg(target_os = "linux")]
pub(crate) fn describe(pid: u32, expected: Option<&str>) -> String {
    let Some(token) =
        expected.filter(|token| lifecycle::process_start_time(pid).as_deref() == Some(*token))
    else {
        return "\nscheduling   unavailable (process identity unreadable)".into();
    };
    let observed = read(pid);
    if lifecycle::process_start_time(pid).as_deref() != Some(token) {
        return "\nscheduling   unavailable (process changed during observation)".into();
    }
    match observed {
        Ok(text) => format!("\nscheduling   {text}"),
        Err(e) => format!("\nscheduling   unavailable ({e})"),
    }
}

#[cfg(target_os = "linux")]
fn read(pid: u32) -> Result<String, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    let (nice, policy) = parse_stat(&stat)?;
    let cgroup =
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e| e.to_string())?;
    // SAFETY: ioprio_get reads a process's scheduling metadata; both scalar
    // arguments are valid (IOPRIO_WHO_PROCESS, positive pid).
    let priority = unsafe { libc::syscall(libc::SYS_ioprio_get, 1, pid as libc::pid_t) };
    let io = if priority < 0 {
        format!("unavailable ({})", std::io::Error::last_os_error())
    } else {
        let class = match priority >> 13 {
            0 => "none (derived)",
            1 => "realtime",
            2 => "best-effort",
            3 => "idle",
            _ => "unknown",
        };
        format!("{class}/{}", priority & 0x1fff)
    };
    Ok(format!(
        "nice {nice}, CPU policy {policy}, I/O {io}\ncgroup       {}",
        cgroup.trim().replace('\n', "; ")
    ))
}

#[cfg(any(target_os = "linux", test))]
fn parse_stat(stat: &str) -> Result<(i32, u32), String> {
    // comm can contain spaces and closing parentheses. Fields after its final
    // ')' begin with state (field 3); nice=19 and policy=41.
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or("missing process name")?
        .1
        .split_whitespace()
        .collect();
    let nice = fields
        .get(16)
        .ok_or("missing nice")?
        .parse()
        .map_err(|_| "invalid nice")?;
    let policy = fields
        .get(38)
        .ok_or("missing CPU policy")?
        .parse()
        .map_err(|_| "invalid CPU policy")?;
    Ok((nice, policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stat_parser_handles_embedded_parentheses_and_refuses_partial_evidence() {
        let mut fields = vec!["0"; 39];
        fields[16] = "10";
        let stat = format!("25 (strange ) name) {}", fields.join(" "));
        assert_eq!(parse_stat(&stat).unwrap(), (10, 0));
        assert!(parse_stat("25 (short) S 1").is_err());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn observes_kernel_and_reports_an_absent_process() {
        assert!(
            describe(
                std::process::id(),
                lifecycle::process_start_time(std::process::id()).as_deref()
            )
            .contains("nice ")
        );
        assert!(describe(u32::MAX, Some("absent")).contains("unavailable"));
        assert!(
            describe(std::process::id(), Some("previous process incarnation"))
                .contains("unavailable")
        );
    }
}
