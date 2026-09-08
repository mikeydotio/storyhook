//! Observe file-backed output without sharing the writer's seek cursor.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

const CHUNK: usize = 16 * 1024;
const INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(super::super::serve::SHUTDOWN_CHECK.as_millis() as u64 / 5);

struct Cursor {
    stream: &'static str,
    file: File,
    offset: u64,
    pending: Vec<u8>,
}

impl Cursor {
    fn drain(&mut self, source: &str, context: &str, final_read: bool) -> io::Result<()> {
        // Snapshot the end: an indefinitely writing descendant cannot extend
        // this read forever, including on the observer's shutdown path.
        let end = self.file.metadata()?.len();
        let mut bytes = [0; CHUNK];
        while self.offset < end {
            let size = (end - self.offset).min(CHUNK as u64) as usize;
            let n = self.file.read_at(&mut bytes[..size], self.offset)?;
            if n == 0 {
                break;
            }
            self.offset += n as u64;
            for &byte in &bytes[..n] {
                if byte == b'\n' || byte == b'\r' {
                    self.flush(source, context);
                } else {
                    self.pending.push(byte);
                    if self.pending.len() >= CHUNK {
                        self.flush(source, context);
                    }
                }
            }
        }
        if final_read {
            self.flush(source, context);
        }
        Ok(())
    }

    fn flush(&mut self, source: &str, context: &str) {
        if self.pending.is_empty() {
            return;
        }
        super::emit(
            if self.stream == "stderr" {
                "WARN"
            } else {
                "INFO"
            },
            source,
            self.stream,
            context,
            &String::from_utf8_lossy(&self.pending),
        );
        self.pending.clear();
    }
}

/// Joins a live observer at command completion, without waiting on output EOF.
pub(crate) struct OutputWatch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OutputWatch {
    /// Starts only inside the serving daemon. Input descriptors are regular
    /// files and independent-offset reads cannot corrupt existing capture.
    pub(crate) fn start(
        source: &str,
        context: &str,
        files: Vec<(&'static str, File, u64)>,
    ) -> Option<Self> {
        if !super::enabled() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let source = source.to_string();
        let context = context.to_string();
        let mut cursors: Vec<_> = files
            .into_iter()
            .map(|(stream, file, offset)| Cursor {
                stream,
                file,
                offset,
                pending: Vec::new(),
            })
            .collect();
        let thread = std::thread::spawn(move || {
            loop {
                let final_read = stopped.load(Ordering::Acquire);
                for cursor in &mut cursors {
                    if let Err(error) = cursor.drain(&source, &context, final_read) {
                        super::report_failure(&error);
                    }
                }
                if final_read {
                    break;
                }
                std::thread::park_timeout(INTERVAL);
            }
        });
        Some(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Observes duplicated capture descriptors; failure is a diagnostic only.
    pub(crate) fn capture(
        source: &str,
        context: &str,
        stdout: &File,
        stderr: &File,
    ) -> Option<Self> {
        if !super::enabled() {
            return None;
        }
        match (stdout.try_clone(), stderr.try_clone()) {
            (Ok(out), Ok(err)) => Self::start(
                source,
                context,
                vec![("stdout", out, 0), ("stderr", err, 0)],
            ),
            (Err(error), _) | (_, Err(error)) => {
                super::report_failure(&error);
                None
            }
        }
    }
}

impl Drop for OutputWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                super::emit("ERROR", "logger", "event", "", "output observer panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};

    #[test]
    fn observation_never_moves_a_writers_shared_file_offset() {
        let mut writer = tempfile::tempfile().unwrap();
        writer.write_all(b"first line\n").unwrap();
        let mut cursor = Cursor {
            stream: "stdout",
            file: writer.try_clone().unwrap(),
            offset: 0,
            pending: Vec::new(),
        };
        writer.seek(SeekFrom::Start(2)).unwrap();
        cursor.drain("probe", "", false).unwrap();
        assert_eq!(writer.stream_position().unwrap(), 2);
        writer.write_all(b"XX").unwrap();
        writer.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = String::new();
        writer.read_to_string(&mut actual).unwrap();
        assert_eq!(actual, "fiXXt line\n");
    }
}
