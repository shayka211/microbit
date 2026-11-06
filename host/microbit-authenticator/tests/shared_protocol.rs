//! Host-side smoke tests: the exact frames the firmware emits must parse here,
//! and HMAC must agree with the firmware (both link the same `authenticate` lib).

use authenticate::constants::MAX_FRAME_LEN;
use authenticate::crypto::{sign, verify};
use authenticate::protocol::{AuthErrKind, Frame, ProvErrKind};

fn parse_same(f: Frame) {
    let mut buf = [0u8; MAX_FRAME_LEN];
    let n = f.encode(&mut buf).expect("encode");
    assert_eq!(Frame::parse(&buf[..n]), Ok(f));
}

#[test]
fn parses_every_frame_the_firmware_may_send() {
    parse_same(Frame::Hello { magic: 1 });
    parse_same(Frame::Pong);
    parse_same(Frame::Ready);
    parse_same(Frame::AuthOk([0xaa; 32]));
    parse_same(Frame::AuthTimeout);
    parse_same(Frame::AuthErr(AuthErrKind::Busy));
    parse_same(Frame::AuthErr(AuthErrKind::Unprovisioned));
    parse_same(Frame::ProvOk);
    parse_same(Frame::ProvErr(ProvErrKind::BadKey));
    parse_same(Frame::ProvErr(ProvErrKind::WriteFail));
    parse_same(Frame::ProvErr(ProvErrKind::Already));
    parse_same(Frame::LockOk);
    parse_same(Frame::BtnTestReady);
    parse_same(Frame::BtnPressed(1234));
    parse_same(Frame::BtnTestTimeout);
}

#[test]
fn hmac_agrees_with_firmware_vectors() {
    // Same vectors the firmware tests use (RFC-4231-style 32-byte key).
    let key = [0x0bu8; 32];
    let mut nonce = [0u8; 32];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = (0x20 + i) as u8;
    }
    #[rustfmt::skip]
    let expected: [u8; 32] = [
        0x08, 0x96, 0x74, 0x48, 0xb8, 0xc7, 0x40, 0xaf,
        0x61, 0xbd, 0xf3, 0xe8, 0xa3, 0x0f, 0x47, 0xc9,
        0xa7, 0xdf, 0x27, 0xa0, 0x6d, 0x2f, 0x52, 0x0f,
        0xe0, 0xbc, 0xab, 0xb9, 0xe3, 0xb2, 0xb2, 0x1e,
    ];
    let mac = sign(&key, &nonce);
    assert_eq!(mac, expected);
    assert!(verify(&mac, &key, &nonce));
    let mut bad = mac;
    bad[0] ^= 0xff;
    assert!(!verify(&bad, &key, &nonce));
}