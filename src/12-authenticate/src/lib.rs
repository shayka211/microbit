//! `authenticate` — micro:bit v2 challenge-response authenticator.
//!
//! The library part is deliberately **hardware and HAL free**: it holds all pure
//! logic (protocol codec, HMAC-SHA256 crypto, debounce + armed-session state
//! machines) so it can be unit-tested on the host with a plain `cargo test --lib`
//! and reused by the host authenticator crate. The firmware lives in
//! `src/main.rs` and the on-target (defmt-test) suites live in `tests/`.

#![no_std]

pub mod constants;
pub mod crypto;
pub mod protocol;
pub mod state;