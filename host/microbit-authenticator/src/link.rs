//! Line-oriented AUTH/1.0 client over any byte stream.
//!
//! [`Link`] works on anything that is `Read + Write` — the real serial port,
//! or the in-memory fake device used by the tests. A read that times out is
//! *not* an error: it just means "nothing yet", and partial lines are kept
//! until the rest arrives.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use authenticate::constants::MAX_FRAME_LEN;
use authenticate::crypto::verify;
use authenticate::protocol::{AuthErrKind, Frame, Key, Nonce, ProvErrKind};

pub struct Link<T> {
    io: T,
    buf: Vec<u8>,
    echo: bool,
}

impl<T: Read + Write> Link<T> {
    pub fn new(io: T) -> Self {
        Link {
            io,
            buf: Vec::with_capacity(2 * MAX_FRAME_LEN),
            echo: false,
        }
    }

    /// Print every received line (frames *and* firmware debug logs) to stderr.
    pub fn with_echo(mut self, on: bool) -> Self {
        self.echo = on;
        self
    }

    pub fn send(&mut self, frame: &Frame) -> io::Result<()> {
        let mut buf = [0u8; MAX_FRAME_LEN + 2];
        let n = frame
            .encode(&mut buf)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too long"))?;
        buf[n..n + 2].copy_from_slice(b"\r\n");
        self.io.write_all(&buf[..n + 2])?;
        self.io.flush()
    }

    /// One complete line (CR/LF stripped), or `None` if nothing complete
    /// arrived before the underlying read timed out.
    pub fn poll_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if self.echo {
                    eprintln!("  < {}", String::from_utf8_lossy(&line));
                }
                return Ok(Some(line));
            }
            if self.buf.len() > MAX_FRAME_LEN + 2 {
                // Garbage without a newline (e.g. wrong baud rate): drop it.
                self.buf.clear();
            }
            let mut chunk = [0u8; 128];
            match self.io.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if is_timeout(&e) => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Throw away everything already queued (stale replies, boot banner, logs).
    pub fn drain(&mut self) -> io::Result<()> {
        while self.poll_line()?.is_some() {}
        self.buf.clear();
        Ok(())
    }

    /// Read frames until `pick` accepts one, or `timeout` passes (`None`).
    /// Unparseable lines (firmware debug logs) are skipped.
    pub fn wait_for<R>(
        &mut self,
        timeout: Duration,
        mut pick: impl FnMut(Frame) -> Option<R>,
    ) -> io::Result<Option<R>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(line) = self.poll_line()? {
                if let Ok(frame) = Frame::parse(&line) {
                    if let Some(r) = pick(frame) {
                        return Ok(Some(r));
                    }
                }
            }
        }
        Ok(None)
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

// ── operations ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthOutcome {
    /// Button pressed and the MAC is correct.
    Ok,
    /// Button pressed but the MAC is wrong (board has a different key).
    BadMac,
    /// The board armed but nobody pressed the button in time.
    ButtonTimeout,
    /// The board refused the challenge.
    Refused(AuthErrKind),
    /// No answer at all before our deadline.
    NoAnswer,
}

/// Send a challenge and wait for the board's verdict.
///
/// Outcome frames are only accepted *after* this challenge's `READY`, so a
/// stale `AUTH_OK` / `AUTH_TIMEOUT` from an earlier session can't be mistaken
/// for the answer.
pub fn authenticate<T: Read + Write>(
    link: &mut Link<T>,
    key: &Key,
    nonce: &Nonce,
    timeout: Duration,
) -> io::Result<AuthOutcome> {
    authenticate_with(link, key, nonce, timeout, || {})
}

/// [`authenticate`], calling `on_ready` once the board has armed (i.e. the
/// moment the user should press the button).
pub fn authenticate_with<T: Read + Write>(
    link: &mut Link<T>,
    key: &Key,
    nonce: &Nonce,
    timeout: Duration,
    mut on_ready: impl FnMut(),
) -> io::Result<AuthOutcome> {
    link.send(&Frame::Auth(*nonce))?;
    let mut armed = false;
    let outcome = link.wait_for(timeout, |f| match f {
        Frame::Ready if !armed => {
            armed = true;
            on_ready();
            None
        }
        Frame::AuthErr(kind) => Some(AuthOutcome::Refused(kind)),
        Frame::AuthOk(mac) if armed => Some(if verify(&mac, key, nonce) {
            AuthOutcome::Ok
        } else {
            AuthOutcome::BadMac
        }),
        Frame::AuthTimeout if armed => Some(AuthOutcome::ButtonTimeout),
        _ => None,
    })?;
    Ok(outcome.unwrap_or(AuthOutcome::NoAnswer))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BtnOutcome {
    /// A real press, `ms` after arming.
    Pressed(u32),
    /// The board's own window elapsed without a press.
    DeviceTimeout,
    NoAnswer,
}

pub fn btn_test<T: Read + Write>(link: &mut Link<T>, timeout: Duration) -> io::Result<BtnOutcome> {
    link.send(&Frame::BtnTest)?;
    let mut armed = false;
    let outcome = link.wait_for(timeout, |f| match f {
        Frame::BtnTestReady => {
            armed = true;
            None
        }
        Frame::BtnPressed(ms) if armed => Some(BtnOutcome::Pressed(ms)),
        Frame::BtnTestTimeout if armed => Some(BtnOutcome::DeviceTimeout),
        _ => None,
    })?;
    Ok(outcome.unwrap_or(BtnOutcome::NoAnswer))
}

/// `true` if the board answered `PONG`.
pub fn ping<T: Read + Write>(link: &mut Link<T>, timeout: Duration) -> io::Result<bool> {
    link.send(&Frame::Ping)?;
    Ok(link
        .wait_for(timeout, |f| (f == Frame::Pong).then_some(()))?
        .is_some())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProvOutcome {
    Ok,
    Refused(ProvErrKind),
    NoAnswer,
}

/// Write `key` into the board (no lock).
pub fn provision<T: Read + Write>(
    link: &mut Link<T>,
    key: &Key,
    timeout: Duration,
) -> io::Result<ProvOutcome> {
    link.send(&Frame::Prov(*key))?;
    let outcome = link.wait_for(timeout, |f| match f {
        Frame::ProvOk => Some(ProvOutcome::Ok),
        Frame::ProvErr(kind) => Some(ProvOutcome::Refused(kind)),
        _ => None,
    })?;
    Ok(outcome.unwrap_or(ProvOutcome::NoAnswer))
}

/// Enable readback protection. `true` if the board confirmed (it then resets).
pub fn lock<T: Read + Write>(link: &mut Link<T>, timeout: Duration) -> io::Result<bool> {
    link.send(&Frame::Lock)?;
    Ok(link
        .wait_for(timeout, |f| (f == Frame::LockOk).then_some(()))?
        .is_some())
}
