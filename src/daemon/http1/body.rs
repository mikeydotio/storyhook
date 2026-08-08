//! Reading a request body once its [`super::parse::Framing`] is known.
//!
//! Every read here goes through [`PeerReader`] and the connection's
//! [`super::conn::Deadline`] — the reader is what makes a leftover byte from
//! the head read visible to the body decoder instead of silently dropped,
//! and the deadline is what makes a body read bounded in wall-clock time
//! rather than merely per-syscall (see the module docs on
//! `super::conn::Deadline`) — the exact gap SH-177 exists to close.

use std::cell::RefCell;
use std::io::{self, Read};
use std::net::TcpStream;
use std::rc::Rc;

use super::conn::Deadline;
use super::parse::Framing;

/// A chunk-size or trailer line longer than this is refused rather than
/// accumulated — a chunk-size line is a handful of hex digits at most, so
/// this is generous headroom, not a real limit on anything legitimate.
const MAX_CHUNK_LINE: usize = 256;

/// A body's byte source: whatever arrived past the request head in the same
/// initial read (`prefix`) — `httparse` reports only how many bytes of a
/// read were the head, and a small body routinely arrives in the very same
/// TCP segment as its head on a loopback connection — then the live socket
/// once `prefix` is exhausted. Every body read, length-framed or chunked,
/// goes through this rather than the socket directly, so those leftover
/// bytes are decoded, never dropped.
pub(super) struct PeerReader {
    stream: Rc<RefCell<TcpStream>>,
    prefix: Vec<u8>,
    prefix_pos: usize,
}

impl PeerReader {
    pub(super) fn new(stream: Rc<RefCell<TcpStream>>, prefix: Vec<u8>) -> PeerReader {
        PeerReader {
            stream,
            prefix,
            prefix_pos: 0,
        }
    }

    fn read(&mut self, deadline: &Deadline, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.prefix_pos < self.prefix.len() {
            let available = self.prefix.len() - self.prefix_pos;
            let n = available.min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + n]);
            self.prefix_pos += n;
            return Ok(n);
        }
        let mut stream = self.stream.borrow_mut();
        deadline.arm_read(&stream)?;
        stream.read(buf)
    }
}

/// Where a chunked body's decoder is, between calls to [`BodyState::read`].
#[derive(Debug)]
pub(super) enum ChunkState {
    /// Waiting for the next `<size>[;ext]\r\n` line.
    Size,
    /// Reading the `remaining` data bytes of the current chunk.
    Data(u64),
    /// The chunk's data has been read; waiting for its trailing `\r\n`.
    DataCrlf,
    /// The zero-size chunk has been read; waiting for trailer lines and the
    /// final blank line that ends the body.
    Trailers,
    /// The body has been read to its end, trailers included.
    Done,
}

/// How much of a request body remains, and how to read the next piece of it.
#[derive(Debug)]
pub(super) enum BodyState {
    /// No body was declared.
    None,
    /// A `Content-Length` body; the field is how many bytes are left.
    Length(u64),
    /// A `Transfer-Encoding: chunked` body, mid-decode.
    Chunked(ChunkState),
}

impl BodyState {
    pub(super) fn from_framing(framing: Framing) -> BodyState {
        match framing {
            Framing::None => BodyState::None,
            Framing::Length(n) => BodyState::Length(n),
            Framing::Chunked => BodyState::Chunked(ChunkState::Size),
        }
    }

    /// Whether every byte this body declared has actually been read off the
    /// wire. `false` means the connection must not be reused for another
    /// request — see [`super::conn::Request::respond`] for why: draining the
    /// remainder here would re-open the exact unbounded-block trap SH-172
    /// found in `tiny_http`'s own `EqualReader::drop`.
    pub(super) fn fully_consumed(&self) -> bool {
        match self {
            BodyState::None => true,
            BodyState::Length(remaining) => *remaining == 0,
            BodyState::Chunked(ChunkState::Done) => true,
            BodyState::Chunked(_) => false,
        }
    }

    pub(super) fn read(
        &mut self,
        peer: &mut PeerReader,
        deadline: &Deadline,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        match self {
            BodyState::None => Ok(0),
            BodyState::Length(remaining) => read_length(peer, deadline, remaining, buf),
            BodyState::Chunked(state) => read_chunked(peer, deadline, state, buf),
        }
    }
}

fn read_length(
    peer: &mut PeerReader,
    deadline: &Deadline,
    remaining: &mut u64,
    buf: &mut [u8],
) -> io::Result<usize> {
    if *remaining == 0 || buf.is_empty() {
        return Ok(0);
    }
    let want = (buf.len() as u64).min(*remaining) as usize;
    let n = peer.read(deadline, &mut buf[..want])?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed the connection before its declared request body arrived",
        ));
    }
    *remaining -= n as u64;
    Ok(n)
}

fn read_chunked(
    peer: &mut PeerReader,
    deadline: &Deadline,
    state: &mut ChunkState,
    buf: &mut [u8],
) -> io::Result<usize> {
    loop {
        match state {
            ChunkState::Size => {
                let line = read_line(peer, deadline, MAX_CHUNK_LINE)?;
                let text =
                    std::str::from_utf8(&line).map_err(|_| invalid_data("malformed chunk size"))?;
                let size_hex = text.split(';').next().unwrap_or("").trim();
                let size = u64::from_str_radix(size_hex, 16)
                    .map_err(|_| invalid_data("malformed chunk size"))?;
                *state = if size == 0 {
                    ChunkState::Trailers
                } else {
                    ChunkState::Data(size)
                };
            }
            ChunkState::Data(remaining) => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let want = (buf.len() as u64).min(*remaining) as usize;
                let n = peer.read(deadline, &mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed the connection mid-chunk",
                    ));
                }
                *remaining -= n as u64;
                if *remaining == 0 {
                    *state = ChunkState::DataCrlf;
                }
                return Ok(n);
            }
            ChunkState::DataCrlf => {
                let line = read_line(peer, deadline, 2)?;
                if !line.is_empty() {
                    return Err(invalid_data("malformed chunk terminator"));
                }
                *state = ChunkState::Size;
            }
            ChunkState::Trailers => {
                let line = read_line(peer, deadline, MAX_CHUNK_LINE)?;
                if line.is_empty() {
                    *state = ChunkState::Done;
                    return Ok(0);
                }
                // Trailer header content is read, to keep the connection in
                // sync, and discarded: nothing downstream of this daemon's
                // routes consults a trailer.
            }
            ChunkState::Done => return Ok(0),
        }
    }
}

/// Reads one line (without its terminating `\r\n` or bare `\n`), byte by
/// byte, deadline-bound. Chunk framing lines are always short, so a
/// byte-at-a-time read costs nothing observable here.
fn read_line(peer: &mut PeerReader, deadline: &Deadline, max_len: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = peer.read(deadline, &mut b)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed the connection mid-chunk",
            ));
        }
        if b[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        line.push(b[0]);
        if line.len() > max_len {
            return Err(invalid_data("chunk framing line too long"));
        }
    }
}

fn invalid_data(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
