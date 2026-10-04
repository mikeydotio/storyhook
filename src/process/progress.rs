//! Append-only journal observation for a subprocess's renewable idle deadline.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

/// The journal's identity and last observed extent, owned by one attempt.
pub(super) struct IdleDeadline<'a> {
    journal: &'a Path,
    observed: Metadata,
    offset: u64,
    timeout: Duration,
    deadline: Instant,
}

impl<'a> IdleDeadline<'a> {
    /// Starts observing an existing regular journal before the child starts.
    pub(super) fn new(journal: &'a Path, timeout: Duration) -> io::Result<Self> {
        let (_, observed) = open(journal)?;
        Ok(Self {
            journal,
            offset: observed.len(),
            observed,
            timeout,
            deadline: Instant::now() + timeout,
        })
    }

    /// Renews on complete progress records, never resource observations.
    pub(super) fn remaining(&mut self) -> io::Result<Duration> {
        let (mut file, current) = open(self.journal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (current.dev(), current.ino()) != (self.observed.dev(), self.observed.ino()) {
                return Err(invalid(self.journal, "was replaced during verification"));
            }
        }
        if current.len() < self.observed.len() {
            return Err(invalid(self.journal, "shrank during verification"));
        }
        if current.len() > self.offset {
            // Bound both a single record and each observation. A growing producer
            // must not keep this deadline check busy forever. Partial lines remain
            // at the cursor until their complete record can establish its kind.
            const MAX_RECORD: u64 = 1_048_576;
            file.seek(SeekFrom::Start(self.offset))?;
            let mut data = Vec::new();
            file.take((current.len() - self.offset).min(MAX_RECORD + 1))
                .read_to_end(&mut data)?;
            for line in data.split_inclusive(|byte| *byte == b'\n') {
                if line.len() as u64 > MAX_RECORD {
                    return Err(invalid(self.journal, "contains an oversized record"));
                }
                if !line.ends_with(b"\n") {
                    break;
                }
                self.offset += line.len() as u64;
                let resource = serde_json::from_slice::<serde_json::Value>(line)
                    .is_ok_and(|row| row["kind"] == "resource");
                if !resource {
                    self.deadline = Instant::now() + self.timeout;
                }
            }
        }
        self.observed = current;
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
}

fn open(journal: &Path) -> io::Result<(File, Metadata)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(journal).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("progress journal {}: {error}", journal.display()),
        )
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid(journal, "is not a regular file"));
    }
    Ok((file, metadata))
}

fn invalid(journal: &Path, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("progress journal {} {detail}", journal.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resource_records_and_partial_lines_do_not_renew_idle_time() {
        let root = storyhook_test_support::scratch_dir();
        let path = root.path().join("journal");
        let mut output = std::fs::File::create(&path).unwrap();
        let mut idle = IdleDeadline::new(&path, Duration::from_secs(10)).unwrap();
        let initial = idle.deadline;
        output
            .write_all(b"{\"kind\":\"resource\"}\n{\"kind\":")
            .unwrap();
        idle.remaining().unwrap();
        assert_eq!(
            idle.deadline, initial,
            "telemetry is not execution progress"
        );
        output.write_all(b"\"resource\"}\n").unwrap();
        idle.remaining().unwrap();
        assert_eq!(idle.deadline, initial);
        output.write_all(b"{\"kind\":\"case\"}").unwrap();
        idle.remaining().unwrap();
        assert_eq!(
            idle.deadline, initial,
            "an incomplete record is not progress"
        );
        output.write_all(b"\n").unwrap();
        idle.remaining().unwrap();
        assert!(idle.deadline > initial);
        let renewed = idle.deadline;
        output.write_all(b"legacy progress\n").unwrap();
        idle.remaining().unwrap();
        assert!(idle.deadline > renewed);
    }

    #[test]
    fn oversized_or_replaced_journal_is_not_progress() {
        let root = storyhook_test_support::scratch_dir();
        let path = root.path().join("journal");
        let mut output = File::create(&path).unwrap();
        let mut idle = IdleDeadline::new(&path, Duration::from_secs(10)).unwrap();
        output.write_all(&vec![b'x'; 1_048_577]).unwrap();
        assert!(
            idle.remaining()
                .unwrap_err()
                .to_string()
                .contains("oversized")
        );
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"{}\n").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(
            idle.remaining()
                .unwrap_err()
                .to_string()
                .contains("replaced")
        );
    }

    #[test]
    fn truncation_after_an_incomplete_record_is_refused() {
        let root = storyhook_test_support::scratch_dir();
        let path = root.path().join("journal");
        let mut output = File::create(&path).unwrap();
        let mut idle = IdleDeadline::new(&path, Duration::from_secs(10)).unwrap();
        output.write_all(b"partial").unwrap();
        idle.remaining().unwrap();
        output.set_len(0).unwrap();
        assert!(idle.remaining().unwrap_err().to_string().contains("shrank"));
    }
}
