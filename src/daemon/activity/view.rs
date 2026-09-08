//! Read the daily journal directly; no daemon connection or terminal required.

use super::{Record, day_path};
use crate::{env::Environment, error::AppError};
use chrono::Utc;
use std::io::{self, BufRead, IsTerminal, Seek, Write};

fn render(record: &Record, color: bool) -> String {
    let text = format!(
        "{} {:5} [{}:{}] pid={} {} {}",
        record.at,
        record.level,
        record.source,
        record.stream,
        record.pid,
        record.context,
        record.message
    );
    if !color {
        return text;
    }
    let code = match record.level.as_str() {
        "ERROR" => 31,
        "WARN" => 33,
        _ if record.source == "verifier" => 35,
        _ => 36,
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

/// Prints today's activity, optionally following across UTC midnight. JSON
/// output is NDJSON; ordinary redirected output never contains ANSI escapes.
/// Reading an absent log is an empty result and never starts a daemon.
pub fn read_logs(env: &Environment, follow: bool, json: bool) -> Result<(), AppError> {
    read(env, follow, json)
        .map_err(|error| AppError::Storage(format!("reading activity journal: {error}")))
}

struct Follower {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
    offset: u64,
}

impl Follower {
    fn new(directory: std::path::PathBuf, now: chrono::DateTime<Utc>) -> Self {
        Self {
            path: day_path(&directory, now),
            directory,
            offset: 0,
        }
    }

    fn drain(&mut self, out: &mut impl Write, json: bool, color: bool) -> io::Result<()> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() < self.offset {
            self.offset = 0;
        }
        file.seek(io::SeekFrom::Start(self.offset))?;
        let mut reader = io::BufReader::new(file);
        let mut line = String::new();
        loop {
            let count = reader.read_line(&mut line)?;
            if count == 0 || !line.ends_with('\n') {
                break;
            }
            let record: Record = serde_json::from_str(&line).map_err(|error| {
                io::Error::other(format!(
                    "{} at byte {}: {error}",
                    self.path.display(),
                    self.offset
                ))
            })?;
            if json {
                out.write_all(line.as_bytes())?;
            } else {
                writeln!(out, "{}", render(&record, color))?;
            }
            self.offset += count as u64;
            line.clear();
        }
        out.flush()
    }

    fn tick(
        &mut self,
        now: chrono::DateTime<Utc>,
        out: &mut impl Write,
        json: bool,
        color: bool,
    ) -> io::Result<()> {
        // Drain the old day once more before switching, including any writes
        // observed between the final pre-midnight tick and this one.
        self.drain(out, json, color)?;
        let today = day_path(&self.directory, now);
        if today != self.path {
            self.path = today;
            self.offset = 0;
            self.drain(out, json, color)?;
        }
        Ok(())
    }
}

fn read(env: &Environment, follow: bool, json: bool) -> io::Result<()> {
    let stdout = io::stdout();
    let color = !json && stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut out = stdout.lock();
    let mut follower = Follower::new(env.daemon_state_dir().join("activity"), Utc::now());
    loop {
        follower.tick(Utc::now(), &mut out, json, color)?;
        if !follow {
            return Ok(());
        }
        std::thread::sleep(super::super::serve::SHUTDOWN_CHECK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follower_handles_absence_partial_lines_midnight_and_truncation_without_replay() {
        let root = storyhook_test_support::scratch_dir();
        let day = chrono::DateTime::parse_from_rfc3339("2026-09-07T23:59:59Z")
            .unwrap()
            .to_utc();
        let next = day + chrono::Duration::seconds(1);
        let log = super::super::Journal::new(root.path().to_path_buf());
        let mut follower = Follower::new(root.path().to_path_buf(), day);
        let mut out = Vec::new();
        follower.tick(day, &mut out, true, false).unwrap();
        assert!(out.is_empty());
        log.append(day, "INFO", "daemon", "event", "", "first")
            .unwrap();
        follower.tick(day, &mut out, true, false).unwrap();
        let first_len = out.len();
        follower.tick(day, &mut out, true, false).unwrap();
        assert_eq!(out.len(), first_len);
        log.append(day, "INFO", "daemon", "event", "", "last old day")
            .unwrap();
        log.append(next, "INFO", "daemon", "event", "", "new day")
            .unwrap();
        follower.tick(next, &mut out, true, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.find("last old day").unwrap() < text.find("new day").unwrap());
        let path = day_path(root.path(), next);
        let record = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &record[..record.len() - 1]).unwrap();
        let mut out = Vec::new();
        follower.tick(next, &mut out, true, false).unwrap();
        assert!(
            out.is_empty(),
            "partial records must wait for their newline"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        follower.tick(next, &mut out, true, false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), record);
    }
    #[test]
    fn color_is_presentation_only_and_labels_survive_without_it() {
        let record = Record {
            at: "now".into(),
            level: "ERROR".into(),
            source: "script.sh".into(),
            stream: "stderr".into(),
            pid: 1,
            context: "SH-590".into(),
            message: "failed".into(),
        };
        assert!(render(&record, true).starts_with("\u{1b}[31m"));
        let plain = render(&record, false);
        assert!(!plain.contains('\u{1b}'));
        assert!(plain.contains("ERROR [script.sh:stderr] pid=1 SH-590 failed"));
    }
}
