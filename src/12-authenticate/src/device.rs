//! The firmware's protocol decisions, as pure functions (host-testable).
//!
//! `main.rs` only performs the side effects (UART, UICR, LEDs, button). Every
//! "what do we answer to this line?" decision lives here, so it is covered by
//! `cargo test --lib` and the host crate can run the *same* logic in a fake
//! device for its end-to-end tests.

use crate::constants::{AUTH_WINDOW_MS, BTN_TEST_WINDOW_MS, MAX_FRAME_LEN};
use crate::crypto;
use crate::protocol::{AuthErrKind, Frame, Key, Nonce, ParseError, ProvErrKind};
use crate::state::{ArmedEvent, ArmedKind};

/// What the device knows about itself when a line arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceState {
    /// A key is stored in UICR.
    pub provisioned: bool,
    /// Readback protection (APPROTECT) is enabled.
    pub locked: bool,
}

/// Why a line produced no reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignored {
    TooLong,
    Parse(ParseError),
    /// A valid frame that the host should never send (e.g. a device→host frame).
    OutOfContext,
}

/// What the firmware must do in response to one received line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Send this frame; stay idle.
    Reply(Frame),
    /// Send [`ArmedKind::ready_frame`], then wait for Button A or `window_ms`,
    /// then send [`armed_outcome`].
    Arm {
        kind: ArmedKind,
        window_ms: u32,
        nonce: Nonce,
    },
    /// Write this key to UICR, then reply `PROV_OK` / `PROV_ERR writefail`.
    Provision(Key),
    /// Enable readback protection, reply `LOCK_OK`, reset.
    Lock,
    /// Say nothing.
    Ignore(Ignored),
}

impl ArmedKind {
    /// Frame sent when the session is armed.
    pub const fn ready_frame(self) -> Frame {
        match self {
            ArmedKind::Auth => Frame::Ready,
            ArmedKind::BtnTest => Frame::BtnTestReady,
        }
    }
}

/// Decide what to do with one received line (CR/LF already stripped).
pub fn on_line(line: &[u8], st: DeviceState) -> Action {
    if line.len() > MAX_FRAME_LEN {
        return Action::Ignore(Ignored::TooLong);
    }
    match Frame::parse(line) {
        Ok(frame) => on_frame(frame, st),
        Err(e) => Action::Ignore(Ignored::Parse(e)),
    }
}

/// Decide what to do with one parsed frame.
pub fn on_frame(frame: Frame, st: DeviceState) -> Action {
    match frame {
        Frame::Ping => Action::Reply(Frame::Pong),
        Frame::Auth(nonce) if st.provisioned => Action::Arm {
            kind: ArmedKind::Auth,
            window_ms: AUTH_WINDOW_MS,
            nonce,
        },
        Frame::Auth(_) => Action::Reply(Frame::AuthErr(AuthErrKind::Unprovisioned)),
        Frame::BtnTest => Action::Arm {
            kind: ArmedKind::BtnTest,
            window_ms: BTN_TEST_WINDOW_MS,
            nonce: Nonce::default(),
        },
        // Once locked, the key must never change (that would also wipe the
        // lock, since re-provisioning erases the whole UICR page). While
        // unlocked, re-provisioning is allowed — handy during development.
        Frame::Prov(_) if st.locked => Action::Reply(Frame::ProvErr(ProvErrKind::Already)),
        Frame::Prov(key) => Action::Provision(key),
        Frame::Lock => Action::Lock,
        Frame::Hello { .. }
        | Frame::Pong
        | Frame::Ready
        | Frame::AuthOk(_)
        | Frame::AuthTimeout
        | Frame::AuthErr(_)
        | Frame::ProvOk
        | Frame::ProvErr(_)
        | Frame::LockOk
        | Frame::BtnTestReady
        | Frame::BtnPressed(_)
        | Frame::BtnTestTimeout => Action::Ignore(Ignored::OutOfContext),
    }
}

/// The frame that ends an armed session.
///
/// `key` is only needed for an `Auth` press; `None` there (which `on_frame`
/// never allows) answers `AUTH_ERR unprovisioned` instead of panicking.
pub fn armed_outcome(
    kind: ArmedKind,
    event: ArmedEvent,
    key: Option<&Key>,
    nonce: &Nonce,
) -> Frame {
    match (kind, event) {
        (ArmedKind::Auth, ArmedEvent::Press(_)) => match key {
            Some(k) => Frame::AuthOk(crypto::sign(k, nonce)),
            None => Frame::AuthErr(AuthErrKind::Unprovisioned),
        },
        (ArmedKind::Auth, ArmedEvent::Timeout) => Frame::AuthTimeout,
        (ArmedKind::BtnTest, ArmedEvent::Press(ms)) => Frame::BtnPressed(ms),
        (ArmedKind::BtnTest, ArmedEvent::Timeout) => Frame::BtnTestTimeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESH: DeviceState = DeviceState {
        provisioned: false,
        locked: false,
    };
    const READY: DeviceState = DeviceState {
        provisioned: true,
        locked: false,
    };
    const LOCKED: DeviceState = DeviceState {
        provisioned: true,
        locked: true,
    };

    fn line(f: Frame) -> ([u8; MAX_FRAME_LEN], usize) {
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = f.encode(&mut buf).unwrap();
        (buf, n)
    }

    #[test]
    fn ping_always_answers_pong() {
        for st in [FRESH, READY, LOCKED] {
            assert_eq!(on_line(b"PING", st), Action::Reply(Frame::Pong));
        }
    }

    #[test]
    fn auth_arms_only_when_provisioned() {
        let (buf, n) = line(Frame::Auth([7; 32]));
        assert_eq!(
            on_line(&buf[..n], READY),
            Action::Arm {
                kind: ArmedKind::Auth,
                window_ms: AUTH_WINDOW_MS,
                nonce: [7; 32]
            }
        );
        assert_eq!(
            on_line(&buf[..n], FRESH),
            Action::Reply(Frame::AuthErr(AuthErrKind::Unprovisioned))
        );
    }

    #[test]
    fn btn_test_works_without_a_key() {
        assert!(matches!(
            on_line(b"BTN_TEST", FRESH),
            Action::Arm {
                kind: ArmedKind::BtnTest,
                ..
            }
        ));
    }

    #[test]
    fn prov_is_refused_once_locked() {
        let (buf, n) = line(Frame::Prov([1; 32]));
        assert_eq!(on_line(&buf[..n], FRESH), Action::Provision([1; 32]));
        assert_eq!(on_line(&buf[..n], READY), Action::Provision([1; 32]));
        assert_eq!(
            on_line(&buf[..n], LOCKED),
            Action::Reply(Frame::ProvErr(ProvErrKind::Already))
        );
    }

    #[test]
    fn junk_and_echoes_are_ignored() {
        assert_eq!(
            on_line(b"", READY),
            Action::Ignore(Ignored::Parse(ParseError::Empty))
        );
        assert_eq!(
            on_line(b"HELLO?", READY),
            Action::Ignore(Ignored::Parse(ParseError::Unknown))
        );
        assert_eq!(
            on_line(b"PONG", READY),
            Action::Ignore(Ignored::OutOfContext)
        );
        assert_eq!(
            on_line(&[b'A'; MAX_FRAME_LEN + 1], READY),
            Action::Ignore(Ignored::TooLong)
        );
    }

    #[test]
    fn armed_outcomes() {
        let key = [0x42; 32];
        let nonce = [0x24; 32];
        assert_eq!(
            armed_outcome(ArmedKind::Auth, ArmedEvent::Press(10), Some(&key), &nonce),
            Frame::AuthOk(crypto::sign(&key, &nonce))
        );
        assert_eq!(
            armed_outcome(ArmedKind::Auth, ArmedEvent::Timeout, Some(&key), &nonce),
            Frame::AuthTimeout
        );
        assert_eq!(
            armed_outcome(ArmedKind::Auth, ArmedEvent::Press(10), None, &nonce),
            Frame::AuthErr(AuthErrKind::Unprovisioned)
        );
        assert_eq!(
            armed_outcome(ArmedKind::BtnTest, ArmedEvent::Press(321), None, &nonce),
            Frame::BtnPressed(321)
        );
        assert_eq!(
            armed_outcome(ArmedKind::BtnTest, ArmedEvent::Timeout, None, &nonce),
            Frame::BtnTestTimeout
        );
        assert_eq!(ArmedKind::Auth.ready_frame(), Frame::Ready);
        assert_eq!(ArmedKind::BtnTest.ready_frame(), Frame::BtnTestReady);
    }
}
