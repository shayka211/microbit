//! Host side of the micro:bit challenge-response login.
//!
//! - [`link`]: the protocol client (works on any `Read + Write`, so it is
//!   tested against an in-memory fake device — see `tests/fake_device.rs`)
//! - [`port`]: finding / opening the real serial port
//! - [`keyfile`]: the host's copy of the shared secret
//!
//! `main.rs` is only the CLI and the PAM exit-code contract.

#![cfg_attr(target_os = "none", no_std)]

#[cfg(target_os = "none")]
compile_error!(
    "microbit-authenticator is the PC tool, not the firmware. \
     To flash the micro:bit, run cargo embed from the firmware folder: \
     cd ../../src/12-authenticate && cargo embed-rel"
);

#[cfg(not(target_os = "none"))]
pub mod keyfile;
#[cfg(not(target_os = "none"))]
pub mod link;
#[cfg(not(target_os = "none"))]
pub mod port;
