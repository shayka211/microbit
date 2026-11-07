//! `authenticate` — micro:bit v2 challenge-response authenticator.
//!
//! The library part is deliberately **hardware and HAL free**: it holds all
//! pure logic so it can be unit-tested on the host with `cargo test --lib` and
//! reused by the host authenticator crate:
//!
//! - `protocol`: AUTH/1.0 frame codec (the wire contract)
//! - `crypto`: HMAC-SHA256 sign / constant-time verify
//! - `device`: what the firmware answers to each frame
//! - `state`: armed-session and debounce state machines
//! - `pattern`: LED images, blinking and the display timeline
//!
//! The firmware (side effects only) lives in `src/main.rs`; the on-target
//! (defmt-test) suites live in `tests/`.

#![no_std]

pub mod constants;
pub mod crypto;
pub mod device;
pub mod pattern;
pub mod protocol;
pub mod state;