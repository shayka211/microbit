//! Hardware-in-the-loop tests against the real, plugged-in micro:bit.
//!
//! Skipped by default. Run with the board attached:
//!
//! ```text
//! cargo test --test hardware -- --ignored --test-threads=1
//! ```
//!
//! None of these needs a button press — they check the firmware answers,
//! ignores junk, and times out correctly. For the press itself use
//! `microbit-authenticator btn-test` / `test` (see README "Testing").

use std::time::{Duration, Instant};

use authenticate::constants::AUTH_WINDOW_MS;
use authenticate::protocol::{AuthErrKind, Key};
use microbit_authenticator::link::{self, AuthOutcome, Link};
use microbit_authenticator::port;
use serialport::SerialPort;

fn board() -> Link<Box<dyn SerialPort>> {
    let path =
        port::discover(std::env::var("MICROBIT_PORT").ok().as_deref()).expect("board plugged in?");
    let mut l = Link::new(port::open(&path).unwrap()).with_echo(true);
    l.drain().unwrap();
    l
}

#[test]
#[ignore = "needs the micro:bit"]
fn hw_ping() {
    let mut l = board();
    assert!(link::ping(&mut l, Duration::from_secs(2)).unwrap());
}

#[test]
#[ignore = "needs the micro:bit"]
fn hw_ping_is_fast() {
    let mut l = board();
    let t = Instant::now();
    for _ in 0..20 {
        assert!(link::ping(&mut l, Duration::from_secs(1)).unwrap());
    }
    let avg = t.elapsed() / 20;
    eprintln!("average PING round trip: {avg:?}");
    assert!(avg < Duration::from_millis(100));
}

#[test]
#[ignore = "needs the micro:bit"]
fn hw_junk_lines_do_not_break_the_board() {
    use std::io::Write;
    let path = port::discover(std::env::var("MICROBIT_PORT").ok().as_deref()).unwrap();
    let mut raw = port::open(&path).unwrap();
    raw.write_all(b"HELLO?\r\n\r\nAUTH zz\r\nPONG\r\n").unwrap();
    raw.write_all(&[b'X'; 300]).unwrap(); // overlong line
    raw.write_all(b"\r\n").unwrap();
    let mut l = Link::new(raw).with_echo(true);
    std::thread::sleep(Duration::from_millis(100));
    l.drain().unwrap();
    assert!(link::ping(&mut l, Duration::from_secs(2)).unwrap());
}

/// Don't touch the board during this one (~5 s).
#[test]
#[ignore = "needs the micro:bit; do NOT press button A"]
fn hw_auth_without_press_times_out() {
    let mut l = board();
    let key: Key = [0; 32]; // the MAC is never produced, so any key will do
    let t = Instant::now();
    let outcome = link::authenticate(
        &mut l,
        &key,
        &rand::random(),
        Duration::from_millis(AUTH_WINDOW_MS as u64 + 2_000),
    )
    .unwrap();
    match outcome {
        AuthOutcome::ButtonTimeout => {
            let took = t.elapsed();
            eprintln!("board timed out after {took:?}");
            assert!(took >= Duration::from_millis(AUTH_WINDOW_MS as u64 - 100));
        }
        AuthOutcome::Refused(AuthErrKind::Unprovisioned) => {
            eprintln!("board is unprovisioned — refusal is the correct answer");
        }
        other => panic!("unexpected {other:?}"),
    }
    // And it's listening again right away.
    assert!(link::ping(&mut l, Duration::from_secs(2)).unwrap());
}
