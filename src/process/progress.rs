//! Append-only journal observation for a subprocess's renewable idle deadline.

use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// The journal's identity and last observed extent, owned by one attempt.
pub(super) struct IdleDeadline<'a> {
    journal: &'a Path,
    observed: Metadata,
    timeout: Duration,
    deadline: Instant,
}

impl<'a> IdleDeadline<'a> {
    /// Starts observing an existing regular journal before the child starts.
    pub(super) fn new(journal: &'a Path, timeout: Duration) -> io::Result<Self> {
        Ok(Self {
            journal,
            observed: metadata(journal)?,
            timeout,
            deadline: Instant::now() + timeout,
        })
    }

    /// Renews only on growth, refusing loss, replacement, or truncation.
    pub(super) fn remaining(&mut self) -> io::Result<Duration> {
        let current = metadata(self.journal)?;
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
        if current.len() > self.observed.len() {
            self.deadline = Instant::now() + self.timeout;
            self.observed = current;
        }
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
}

fn metadata(journal: &Path) -> io::Result<Metadata> {
    let metadata = std::fs::metadata(journal).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("progress journal {}: {error}", journal.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(invalid(journal, "is not a regular file"));
    }
    Ok(metadata)
}

fn invalid(journal: &Path, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("progress journal {} {detail}", journal.display()),
    )
}
