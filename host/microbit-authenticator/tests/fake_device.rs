//! End-to-end tests of the host client against an in-memory micro:bit.
//!
//! The fake board answers using `authenticate::device` — the exact decision
//! logic the firmware runs — so these tests cover host + firmware protocol
//! behaviour together, without hardware. Only the button and the UICR are
//! simulated.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::time::Duration;

use authenticate::constants::MAX_FRAME_LEN;
use authenticate::device::{self, Action, DeviceState};
use authenticate::protocol::{AuthErrKind, Frame, Key, ProvErrKind};
use authenticate::state::ArmedEvent;
use microbit_authenticator::link::{self, AuthOutcome, BtnOutcome, Link, ProvOutcome};

const KEY: Key = [0x5a; 32];
const T: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
enum Button {
    PressAfter(u32),
    Never,
}

struct FakeBoard {
    key: Option<Key>,
    locked: bool,
    button: Button,
    /// Emit firmware-style `#INF` debug lines around every reply.
    chatty: bool,
    /// Deliver at most one byte per read, with a timeout in between.
    trickle: bool,
    /// Never answer.
    dead: bool,
    rx: Vec<u8>,
    tx: VecDeque<u8>,
    tick: bool,
    received: Vec<Frame>,
}

impl FakeBoard {
    fn new(key: Option<Key>) -> Self {
        FakeBoard {
            key,
            locked: false,
            button: Button::PressAfter(250),
            chatty: false,
            trickle: false,
            dead: false,
            rx: Vec::new(),
            tx: VecDeque::new(),
            tick: false,
            received: Vec::new(),
        }
    }

    fn emit_raw(&mut self, bytes: &[u8]) {
        self.tx.extend(bytes);
        self.tx.extend(b"\r\n");
    }

    fn emit(&mut self, f: Frame) {
        if self.chatty {
            self.emit_raw(b"#INF proto: about to reply");
        }
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = f.encode(&mut buf).unwrap();
        self.emit_raw(&buf[..n]);
    }

    fn handle(&mut self, line: &[u8]) {
        if let Ok(f) = Frame::parse(line) {
            self.received.push(f);
        }
        if self.dead {
            return;
        }
        let st = DeviceState {
            provisioned: self.key.is_some(),
            locked: self.locked,
        };
        match device::on_line(line, st) {
            Action::Reply(f) => self.emit(f),
            Action::Arm { kind, nonce, .. } => {
                self.emit(kind.ready_frame());
                let event = match self.button {
                    Button::PressAfter(ms) => ArmedEvent::Press(ms),
                    Button::Never => ArmedEvent::Timeout,
                };
                let reply = device::armed_outcome(kind, event, self.key.as_ref(), &nonce);
                self.emit(reply);
            }
            Action::Provision(k) => {
                self.key = Some(k);
                self.emit(Frame::ProvOk);
            }
            Action::Lock => {
                self.locked = true;
                self.emit(Frame::LockOk);
            }
            Action::Ignore(_) => {}
        }
    }
}

impl Write for FakeBoard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            match b {
                b'\n' => {
                    let line = std::mem::take(&mut self.rx);
                    self.handle(&line);
                }
                b'\r' => {}
                b => self.rx.push(b),
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for FakeBoard {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.tick = !self.tick;
        if self.tx.is_empty() || (self.trickle && self.tick) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
        }
        let max = if self.trickle { 1 } else { buf.len() };
        let n = max.min(self.tx.len());
        for (i, b) in self.tx.drain(..n).enumerate() {
            buf[i] = b;
        }
        Ok(n)
    }
}

fn auth(board: &mut FakeBoard, host_key: &Key) -> AuthOutcome {
    let mut l = Link::new(board);
    link::authenticate(&mut l, host_key, &[0x11; 32], T).unwrap()
}

// ── auth ───────────────────────────────────────────────────────────────────

#[test]
fn auth_succeeds_with_matching_keys_and_a_press() {
    let mut b = FakeBoard::new(Some(KEY));
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::Ok);
    assert_eq!(b.received, vec![Frame::Auth([0x11; 32])]);
}

#[test]
fn auth_fails_when_keys_differ() {
    let mut b = FakeBoard::new(Some([0x99; 32]));
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::BadMac);
}

#[test]
fn auth_fails_without_a_press() {
    let mut b = FakeBoard::new(Some(KEY));
    b.button = Button::Never;
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::ButtonTimeout);
}

#[test]
fn auth_refused_by_unprovisioned_board() {
    let mut b = FakeBoard::new(None);
    assert_eq!(
        auth(&mut b, &KEY),
        AuthOutcome::Refused(AuthErrKind::Unprovisioned)
    );
}

#[test]
fn auth_gives_up_on_a_silent_board() {
    let mut b = FakeBoard::new(Some(KEY));
    b.dead = true;
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::NoAnswer);
}

#[test]
fn auth_ignores_firmware_debug_lines() {
    let mut b = FakeBoard::new(Some(KEY));
    b.chatty = true;
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::Ok);
}

#[test]
fn auth_survives_lines_split_across_read_timeouts() {
    let mut b = FakeBoard::new(Some(KEY));
    b.trickle = true;
    b.chatty = true;
    let mut l = Link::new(&mut b);
    let outcome = link::authenticate(&mut l, &KEY, &[0x11; 32], Duration::from_secs(2)).unwrap();
    assert_eq!(outcome, AuthOutcome::Ok);
}

#[test]
fn stale_verdict_before_ready_is_not_trusted() {
    // A leftover AUTH_OK from an earlier session sits in the buffer.
    let mut b = FakeBoard::new(Some(KEY));
    b.button = Button::Never;
    b.emit(Frame::AuthOk([0; 32]));
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::ButtonTimeout);
}

#[test]
fn drain_discards_boot_banner_and_stale_frames() {
    let mut b = FakeBoard::new(Some(KEY));
    b.emit(Frame::Hello { magic: 1 });
    b.emit(Frame::AuthTimeout);
    let mut l = Link::new(&mut b);
    l.drain().unwrap();
    assert_eq!(l.poll_line().unwrap(), None);
}

// ── other commands ─────────────────────────────────────────────────────────

#[test]
fn ping_pong() {
    let mut b = FakeBoard::new(None);
    assert!(link::ping(&mut Link::new(&mut b), T).unwrap());
    b.dead = true;
    assert!(!link::ping(&mut Link::new(&mut b), T).unwrap());
}

#[test]
fn btn_test_reports_press_and_timeout() {
    let mut b = FakeBoard::new(None);
    b.button = Button::PressAfter(1234);
    assert_eq!(
        link::btn_test(&mut Link::new(&mut b), T).unwrap(),
        BtnOutcome::Pressed(1234)
    );
    b.button = Button::Never;
    assert_eq!(
        link::btn_test(&mut Link::new(&mut b), T).unwrap(),
        BtnOutcome::DeviceTimeout
    );
}

#[test]
fn provision_then_auth_round_trip() {
    let mut b = FakeBoard::new(None);
    assert_eq!(
        auth(&mut b, &KEY),
        AuthOutcome::Refused(AuthErrKind::Unprovisioned)
    );
    assert_eq!(
        link::provision(&mut Link::new(&mut b), &KEY, T).unwrap(),
        ProvOutcome::Ok
    );
    assert_eq!(b.key, Some(KEY));
    assert_eq!(auth(&mut b, &KEY), AuthOutcome::Ok);
}

#[test]
fn unlocked_board_can_be_reprovisioned_locked_one_cannot() {
    let mut b = FakeBoard::new(Some([0x01; 32]));
    assert_eq!(
        link::provision(&mut Link::new(&mut b), &KEY, T).unwrap(),
        ProvOutcome::Ok
    );
    assert!(link::lock(&mut Link::new(&mut b), T).unwrap());
    assert_eq!(
        link::provision(&mut Link::new(&mut b), &[0x02; 32], T).unwrap(),
        ProvOutcome::Refused(ProvErrKind::Already)
    );
    assert_eq!(b.key, Some(KEY), "locked key unchanged");
}

#[test]
fn ready_callback_fires_once_and_only_when_armed() {
    let mut calls = 0;
    let mut b = FakeBoard::new(Some(KEY));
    let out = link::authenticate_with(&mut Link::new(&mut b), &KEY, &[1; 32], T, || calls += 1);
    assert_eq!((out.unwrap(), calls), (AuthOutcome::Ok, 1));

    let mut calls = 0;
    let mut b = FakeBoard::new(None);
    let out = link::authenticate_with(&mut Link::new(&mut b), &KEY, &[1; 32], T, || calls += 1);
    assert_eq!(calls, 0, "no 'press A' prompt when the board refuses");
    assert_eq!(out.unwrap(), AuthOutcome::Refused(AuthErrKind::Unprovisioned));
}
