//! On-target (defmt-test) suites — run on the real micro:bit M4 via probe-rs.
//!
//! ```text
//! cd ~/checkouts/microbit/src/12-authenticate
//! cargo test --target thumbv7em-none-eabihf --test ondevice
//! ```
//!
//! These re-run the host logic suites (PLAN §1.11b) on the actual hardware and
//! therefore also cover compiler codegen for ARM Thumb — something the x86_64
//! host tests cannot.

#![no_std]
#![no_main]

use defmt_rtt as _; // installs the defmt global logger for the harness
use panic_probe as _; // core `panic!` (expect/unwrap) → visible defmt panic
use nrf52833_pac as _; // device crate: provides the interrupt vector table

// defmt's `panic!`/`assert!` route through `_defmt_panic`; install it with the
// official attribute (same as the firmware's logger.rs).
#[defmt::panic_handler]
fn panic_handler() -> ! {
    core::panic!()
}

// defmt always references `_defmt_timestamp` when formatting; the firmware gets
// this from the embassy time driver, so we supply a no-op backend here.
#[no_mangle]
fn _defmt_timestamp(_fmt: defmt::Formatter<'_>) {}

#[defmt_test::tests]
mod tests {
    use authenticate::constants::MAX_FRAME_LEN;
    use authenticate::crypto;
    use authenticate::protocol::Frame;
    use authenticate::device::{self, Action, DeviceState};
    use authenticate::pattern::{Pattern, Screen, View, BLANK};
    use authenticate::state::{ArmedEvent, ArmedKind, Debounce, Session};

    #[init]
    fn init() {
        // Nothing to set up: these suites are pure logic.
    }

    #[test]
    fn hmac_vectors_match_host() {
        // Vector 1: key = nonce = 0x00..0x1f
        let mut k = [0u8; 32];
        let mut n = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        n.copy_from_slice(&k);
        #[rustfmt::skip]
        let expected: [u8; 32] = [
            0xe8, 0x49, 0x9b, 0xe4, 0xf1, 0x98, 0x0d, 0x68,
            0xf1, 0x32, 0x22, 0xa4, 0x18, 0xdf, 0x5c, 0xbd,
            0x97, 0xd5, 0x3f, 0xdd, 0xf5, 0x90, 0xc2, 0x10,
            0x8e, 0x22, 0xd4, 0x00, 0x05, 0xb7, 0x07, 0x13,
        ];
        let mac = crypto::sign(&k, &n);
        defmt::assert!(mac == expected, "HMAC vector 1");

        // Vector 2: 0x0b key * 32, nonce 0x20..0x3f
        let k2 = [0x0bu8; 32];
        let mut n2 = [0u8; 32];
        for (i, b) in n2.iter_mut().enumerate() {
            *b = (0x20 + i) as u8;
        }
        #[rustfmt::skip]
        let expected2: [u8; 32] = [
            0x08, 0x96, 0x74, 0x48, 0xb8, 0xc7, 0x40, 0xaf,
            0x61, 0xbd, 0xf3, 0xe8, 0xa3, 0x0f, 0x47, 0xc9,
            0xa7, 0xdf, 0x27, 0xa0, 0x6d, 0x2f, 0x52, 0x0f,
            0xe0, 0xbc, 0xab, 0xb9, 0xe3, 0xb2, 0xb2, 0x1e,
        ];
        let mac2 = crypto::sign(&k2, &n2);
        defmt::assert!(mac2 == expected2, "HMAC vector 2");
        defmt::assert!(
            crypto::verify(&mac2, &k2, &n2),
            "constant-time verify accepts correct MAC"
        );
        let mut bad = mac2;
        bad[0] ^= 0xff;
        defmt::assert!(
            !crypto::verify(&bad, &k2, &n2),
            "constant-time verify rejects wrong MAC"
        );
    }

    #[test]
    fn device_rules_on_device() {
        let st = DeviceState {
            provisioned: true,
            locked: false,
        };
        defmt::assert!(device::on_line(b"PING", st) == Action::Reply(Frame::Pong));
        let key = [0x42u8; 32];
        let nonce = [0x24u8; 32];
        let reply = device::armed_outcome(ArmedKind::Auth, ArmedEvent::Press(5), Some(&key), &nonce);
        defmt::assert!(reply == Frame::AuthOk(crypto::sign(&key, &nonce)), "AUTH_OK carries the HMAC");
    }

    #[test]
    fn display_timeline_on_device() {
        let mut s = Screen::new(0);
        s.apply(View::steady(Pattern::Waiting), 1_000);
        defmt::assert!(s.bitmap(1_000) != BLANK, "blink starts on");
        defmt::assert!(s.bitmap(1_500) == BLANK);
        defmt::assert!(s.next_change(1_500) == Some(2_000));
        s.apply(View::steady(Pattern::Idle), 3_000);
        defmt::assert!(s.next_change(3_000).is_none(), "idle sleeps forever");
    }

    #[test]
    fn frame_codec_roundtrip_on_device() {
        let cases = [
            Frame::Ping,
            Frame::Pong,
            Frame::Ready,
            Frame::AuthTimeout,
            Frame::ProvOk,
            Frame::LockOk,
            Frame::BtnTestReady,
            Frame::BtnTestTimeout,
            Frame::BtnPressed(1234),
            Frame::BtnPressed(20),
            Frame::Hello { magic: 1 },
            Frame::Auth([0x11; 32]),
            Frame::AuthOk([0xf0; 32]),
            Frame::Prov([0xab; 32]),
        ];
        for f in cases {
            let mut buf = [0u8; MAX_FRAME_LEN];
            match f.encode(&mut buf) {
                Ok(n) => {
                    defmt::assert!(n <= MAX_FRAME_LEN, "frame fits the buffer");
                    defmt::assert!(
                        Frame::parse(&buf[..n]) == Ok(f),
                        "codec roundtrip on device"
                    );
                }
                Err(()) => defmt::assert!(false, "encode failed"),
            }
        }
    }

    #[test]
    fn armed_session_silence_until_press_or_timeout() {
        let mut s = Session::new(ArmedKind::BtnTest, 1_000);
        let mut silence = true;
        for t in (0..1_000).step_by(10) {
            if s.step(t, None).is_some() {
                silence = false;
            }
        }
        defmt::assert!(silence, "nothing emitted while only time passes");
        defmt::assert!(
            s.step(1_000, None) == Some(ArmedEvent::Timeout),
            "timeout is first+only event"
        );
        defmt::assert!(s.step(2_000, Some(5)) == None, "fires at most once");

        let mut s2 = Session::new(ArmedKind::Auth, 5_000);
        for t in (0..4_000).step_by(100) {
            let _ = s2.step(t, None);
        }
        defmt::assert!(
            s2.step(4_000, Some(4_000)) == Some(ArmedEvent::Press(4_000)),
            "press wins over timeout"
        );
    }

    #[test]
    fn debounce_fires_exactly_once() {
        let mut d = Debounce::new(5, 20);
        let mut fired = 0u32;
        for _ in 0..10 {
            if d.feed(true).is_some() {
                fired += 1;
            }
        }
        defmt::assert!(fired == 1, "exactly one confirmation while held");

        // a short glitch must not arm the button
        let mut g = Debounce::new(5, 20);
        defmt::assert!(g.feed(true).is_none(), "glitch start");
        defmt::assert!(g.feed(false).is_none(), "glitch end");
        defmt::assert!(!g.feed(true).is_some(), "needs stable low to confirm");
    }
}