//! Host side of the micro:bit challenge-response login.
//!
//! - [`link`]: the protocol client (works on any `Read + Write`, so it is
//!   tested against an in-memory fake device — see `tests/fake_device.rs`)
//! - [`port`]: finding / opening the real serial port
//! - [`keyfile`]: the host's copy of the shared secret
//!
//! `main.rs` is only the CLI and the PAM exit-code contract.

pub mod keyfile;
pub mod link;
pub mod port;
