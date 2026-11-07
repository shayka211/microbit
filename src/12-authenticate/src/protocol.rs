//! AUTH/1.0 frame codec — the shared wire contract.
//!
//! One frame per line, ASCII, terminated with `\r\n` by the serial layer.
//! Hex payloads are lowercase. This module is hardware-free so both the
//! firmware and the host authenticator use the exact same codec.

use crate::constants::{KEY_LEN, MAC_LEN, MAX_FRAME_LEN, NONCE_LEN};

pub type Key = [u8; KEY_LEN];
pub type Nonce = [u8; NONCE_LEN];
pub type Mac = [u8; MAC_LEN];

/// Reason codes carried by `AUTH_ERR`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthErrKind {
    /// The device is busy (e.g. already armed).
    Busy,
    /// Device has no provisioned key, so it refuses challenges.
    Unprovisioned,
}

impl AuthErrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthErrKind::Busy => "busy",
            AuthErrKind::Unprovisioned => "unprovisioned",
        }
    }
}

/// Reason codes carried by `PROV_ERR`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProvErrKind {
    /// `PROV` sent although a key is already flashed.
    Already,
    /// Key argument was malformed (not 64 lowercase-hex chars).
    BadKey,
    /// NVMC write or read-back verification failed.
    WriteFail,
}

impl ProvErrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProvErrKind::Already => "already",
            ProvErrKind::BadKey => "badkey",
            ProvErrKind::WriteFail => "writefail",
        }
    }
}

/// Every frame of the AUTH/1.0 protocol (both directions).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Frame {
    // ── host → device ────────────────────────────────────────────────────
    /// Presence probe.
    Ping,
    /// Challenge: `AUTH <64-hex nonce>`.
    Auth(Nonce),
    /// Provision: `PROV <64-hex key>` (only accepted while unprovisioned).
    Prov(Key),
    /// Enable UICR readback protection, reply `LOCK_OK`, reset.
    Lock,
    /// Arm the button self-test.
    BtnTest,

    // ── device → host ────────────────────────────────────────────────────
    /// Boot banner: `HELLO AUTH/1.0 MAGIC:<01|00>`.
    Hello { magic: u8 },
    /// Reply to `PING`.
    Pong,
    /// Challenge accepted, waiting for Button A.
    Ready,
    /// Button A pressed: `AUTH_OK <64-hex mac>`.
    AuthOk(Mac),
    /// Auth window elapsed with no press.
    AuthTimeout,
    AuthErr(AuthErrKind),
    ProvOk,
    ProvErr(ProvErrKind),
    LockOk,
    /// Button self-test armed.
    BtnTestReady,
    /// Real press observed: `BTN_PRESSED <elapsed_ms>`.
    BtnPressed(u32),
    /// Button self-test window elapsed with no press.
    BtnTestTimeout,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseError {
    Empty,
    Unknown,
    BadLen,
    BadHex,
    BadInt,
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lowercase-hex encode `data` into `out`. Returns the number of bytes written.
pub fn hex_encode(data: &[u8], out: &mut [u8]) -> Result<usize, ()> {
    if out.len() < data.len() * 2 {
        return Err(());
    }
    for (i, b) in data.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    Ok(data.len() * 2)
}

/// Decode `2*N` hex chars into `[u8; N]`. Accepts both cases.
pub fn hex_decode<const N: usize>(s: &[u8]) -> Result<[u8; N], ()> {
    if s.len() != N * 2 {
        return Err(());
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.chunks_exact(2).enumerate() {
        let hi = hex_val(chunk[0]).ok_or(())?;
        let lo = hex_val(chunk[1]).ok_or(())?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_dec(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(v)
}

/// A small append-only byte writer over a caller-provided buffer.
struct BufW<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> BufW<'a> {
    fn byte(&mut self, b: u8) -> Result<(), ()> {
        if self.len >= self.buf.len() {
            return Err(());
        }
        self.buf[self.len] = b;
        self.len += 1;
        Ok(())
    }
    fn str(&mut self, s: &str) -> Result<(), ()> {
        for b in s.bytes() {
            self.byte(b)?;
        }
        Ok(())
    }
    fn hex(&mut self, data: &[u8]) -> Result<(), ()> {
        for b in data {
            self.byte(HEX[(b >> 4) as usize])?;
            self.byte(HEX[(b & 0x0f) as usize])?;
        }
        Ok(())
    }
    fn dec(&mut self, v: u32) -> Result<(), ()> {
        let mut tmp = [0u8; 10];
        let mut n = 0usize;
        let mut x = v;
        loop {
            tmp[n] = b'0' + (x % 10) as u8;
            n += 1;
            x /= 10;
            if x == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            self.byte(tmp[i])?;
        }
        Ok(())
    }
}

impl Frame {
    /// Encode into `out`, returns the number of bytes written (no terminator).
    /// Fails if the frame does not fit in `out` (or exceeds `MAX_FRAME_LEN`).
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, ()> {
        let cap = out.len().min(MAX_FRAME_LEN);
        let mut w = BufW {
            buf: &mut out[..cap],
            len: 0,
        };
        match *self {
            Frame::Ping => w.str("PING")?,
            Frame::Lock => w.str("LOCK")?,
            Frame::BtnTest => w.str("BTN_TEST")?,
            Frame::Pong => w.str("PONG")?,
            Frame::Ready => w.str("READY")?,
            Frame::AuthTimeout => w.str("AUTH_TIMEOUT")?,
            Frame::ProvOk => w.str("PROV_OK")?,
            Frame::LockOk => w.str("LOCK_OK")?,
            Frame::BtnTestReady => w.str("BTN_TEST_READY")?,
            Frame::BtnTestTimeout => w.str("BTN_TEST_TIMEOUT")?,
            Frame::Hello { magic } => {
                w.str("HELLO AUTH/1.0 MAGIC:")?;
                w.dec(u32::from(magic))?;
            }
            Frame::Auth(nonce) => {
                w.str("AUTH ")?;
                w.hex(&nonce)?;
            }
            Frame::Prov(key) => {
                w.str("PROV ")?;
                w.hex(&key)?;
            }
            Frame::AuthOk(mac) => {
                w.str("AUTH_OK ")?;
                w.hex(&mac)?;
            }
            Frame::BtnPressed(ms) => {
                w.str("BTN_PRESSED ")?;
                w.dec(ms)?;
            }
            Frame::AuthErr(kind) => {
                w.str("AUTH_ERR ")?;
                w.str(kind.as_str())?;
            }
            Frame::ProvErr(kind) => {
                w.str("PROV_ERR ")?;
                w.str(kind.as_str())?;
            }
        }
        Ok(w.len)
    }

    /// The frame's keyword (`"AUTH_OK"`, `"PING"`, ...) — short, for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Frame::Ping => "PING",
            Frame::Auth(_) => "AUTH",
            Frame::Prov(_) => "PROV",
            Frame::Lock => "LOCK",
            Frame::BtnTest => "BTN_TEST",
            Frame::Hello { .. } => "HELLO",
            Frame::Pong => "PONG",
            Frame::Ready => "READY",
            Frame::AuthOk(_) => "AUTH_OK",
            Frame::AuthTimeout => "AUTH_TIMEOUT",
            Frame::AuthErr(_) => "AUTH_ERR",
            Frame::ProvOk => "PROV_OK",
            Frame::ProvErr(_) => "PROV_ERR",
            Frame::LockOk => "LOCK_OK",
            Frame::BtnTestReady => "BTN_TEST_READY",
            Frame::BtnPressed(_) => "BTN_PRESSED",
            Frame::BtnTestTimeout => "BTN_TEST_TIMEOUT",
        }
    }

    /// Parse one line (terminator already stripped by the caller).
    pub fn parse(line: &[u8]) -> Result<Frame, ParseError> {
        if line.is_empty() {
            return Err(ParseError::Empty);
        }
        let mut parts = line.split(|&b| b == b' ');
        let cmd = parts.next().unwrap();
        let mut next = || parts.next().ok_or(ParseError::BadLen);

        match cmd {
            b"PING" => Ok(Frame::Ping),
            b"LOCK" => Ok(Frame::Lock),
            b"BTN_TEST" => Ok(Frame::BtnTest),
            b"PONG" => Ok(Frame::Pong),
            b"READY" => Ok(Frame::Ready),
            b"AUTH_TIMEOUT" => Ok(Frame::AuthTimeout),
            b"PROV_OK" => Ok(Frame::ProvOk),
            b"LOCK_OK" => Ok(Frame::LockOk),
            b"BTN_TEST_READY" => Ok(Frame::BtnTestReady),
            b"BTN_TEST_TIMEOUT" => Ok(Frame::BtnTestTimeout),
            b"AUTH" => {
                let nonce = hex_decode::<NONCE_LEN>(next()?).map_err(|_| ParseError::BadHex)?;
                Ok(Frame::Auth(nonce))
            }
            b"PROV" => {
                let key = hex_decode::<KEY_LEN>(next()?).map_err(|_| ParseError::BadHex)?;
                Ok(Frame::Prov(key))
            }
            b"AUTH_OK" => {
                let mac = hex_decode::<MAC_LEN>(next()?).map_err(|_| ParseError::BadHex)?;
                Ok(Frame::AuthOk(mac))
            }
            b"BTN_PRESSED" => {
                let ms = parse_dec(next()?).ok_or(ParseError::BadInt)?;
                Ok(Frame::BtnPressed(ms))
            }
            b"AUTH_ERR" => match next()? {
                b"busy" => Ok(Frame::AuthErr(AuthErrKind::Busy)),
                b"unprovisioned" => Ok(Frame::AuthErr(AuthErrKind::Unprovisioned)),
                _ => Err(ParseError::BadLen),
            },
            b"PROV_ERR" => match next()? {
                b"already" => Ok(Frame::ProvErr(ProvErrKind::Already)),
                b"badkey" => Ok(Frame::ProvErr(ProvErrKind::BadKey)),
                b"writefail" => Ok(Frame::ProvErr(ProvErrKind::WriteFail)),
                _ => Err(ParseError::BadLen),
            },
            b"HELLO" => {
                // HELLO AUTH/1.0 MAGIC:<01|00>
                let magic_raw = next()?; // expects AUTH/1.0 (ignored)
                let _ = magic_raw;
                let magic_part = next()?;
                let digits = magic_part
                    .strip_prefix(b"MAGIC:")
                    .ok_or(ParseError::BadLen)?;
                let magic = parse_dec(digits).ok_or(ParseError::BadInt)? as u8;
                Ok(Frame::Hello { magic })
            }
            _ => Err(ParseError::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;
    use std::string::String;

    fn roundtrip(f: Frame, expected: &str) {
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = f.encode(&mut buf).expect("encode");
        assert_eq!(&buf[..n], expected.as_bytes(), "wire bytes for {f:?}");
        assert_eq!(Frame::parse(&buf[..n]), Ok(f), "parse for {f:?}");
    }

    #[test]
    fn roundtrip_simple_frames() {
        roundtrip(Frame::Ping, "PING");
        roundtrip(Frame::Pong, "PONG");
        roundtrip(Frame::Lock, "LOCK");
        roundtrip(Frame::BtnTest, "BTN_TEST");
        roundtrip(Frame::Ready, "READY");
        roundtrip(Frame::AuthTimeout, "AUTH_TIMEOUT");
        roundtrip(Frame::ProvOk, "PROV_OK");
        roundtrip(Frame::LockOk, "LOCK_OK");
        roundtrip(Frame::BtnTestReady, "BTN_TEST_READY");
        roundtrip(Frame::BtnTestTimeout, "BTN_TEST_TIMEOUT");
        roundtrip(Frame::Hello { magic: 1 }, "HELLO AUTH/1.0 MAGIC:1");
        roundtrip(Frame::Hello { magic: 0 }, "HELLO AUTH/1.0 MAGIC:0");
        roundtrip(Frame::BtnPressed(1234), "BTN_PRESSED 1234");
        roundtrip(Frame::AuthErr(AuthErrKind::Unprovisioned), "AUTH_ERR unprovisioned");
        roundtrip(Frame::ProvErr(ProvErrKind::WriteFail), "PROV_ERR writefail");
    }

    #[test]
    fn roundtrip_payload_frames() {
        // expected hex is generated on the host (tests compile for x86_64)
        fn rep(b: u8, n: usize) -> String {
            let (h, l) = (HEX[(b >> 4) as usize] as char, HEX[(b & 0xf) as usize] as char);
            (0..n).map(|_| format!("{h}{l}")).collect()
        }
        let nonce = [0x11u8; 32];
        roundtrip(Frame::Auth(nonce), &format!("AUTH {}", rep(0x11, 32)));
        let key = [0xabu8; 32];
        roundtrip(Frame::Prov(key), &format!("PROV {}", rep(0xab, 32)));
        let mac = [0xf0u8; 32];
        roundtrip(Frame::AuthOk(mac), &format!("AUTH_OK {}", rep(0xf0, 32)));
    }

    #[test]
    fn hex_codec() {
        let data = [0x00u8, 0x0f, 0xa0, 0xff];
        let mut out = [0u8; 8];
        let n = hex_encode(&data, &mut out).unwrap();
        assert_eq!(&out[..n], b"000fa0ff");
        assert_eq!(hex_decode::<4>(b"000fa0ff").unwrap(), data);
        assert_eq!(hex_decode::<4>(b"000FA0FF").unwrap(), data); // uppercase ok
        assert!(hex_decode::<4>(b"000fa0f").is_err()); // odd length
        assert!(hex_decode::<4>(b"000fa0ff0").is_err());
        assert!(hex_decode::<4>(b"000fa0g0").is_err()); // non-hex
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(Frame::parse(b""), Err(ParseError::Empty));
        assert_eq!(Frame::parse(b"FOO"), Err(ParseError::Unknown));
        assert_eq!(Frame::parse(b"AUTH"), Err(ParseError::BadLen));
        assert_eq!(Frame::parse(b"AUTH zz"), Err(ParseError::BadHex));
        assert_eq!(Frame::parse(b"BTN_PRESSED abc"), Err(ParseError::BadInt));
        assert_eq!(Frame::parse(b"HELLO AUTH/1.0"), Err(ParseError::BadLen));
    }

    #[test]
    fn encode_accepts_any_big_enough_buffer() {
        let mut big = [0u8; 256];
        let n = Frame::AuthOk([0xab; 32]).encode(&mut big).expect("bigger buffer is fine");
        assert_eq!(n, "AUTH_OK ".len() + 64);
        let mut small = [0u8; 4];
        assert!(Frame::Pong.encode(&mut small).is_ok());
        assert!(Frame::Ready.encode(&mut small).is_err());
    }

    #[test]
    fn name_is_the_wire_keyword() {
        for f in [
            Frame::Ping,
            Frame::Auth([0; 32]),
            Frame::Hello { magic: 1 },
            Frame::AuthOk([0; 32]),
            Frame::AuthErr(AuthErrKind::Busy),
            Frame::BtnPressed(5),
            Frame::BtnTestTimeout,
        ] {
            let mut buf = [0u8; MAX_FRAME_LEN];
            let n = f.encode(&mut buf).unwrap();
            let wire = core::str::from_utf8(&buf[..n]).unwrap();
            assert_eq!(wire.split(' ').next(), Some(f.name()));
        }
    }
}
