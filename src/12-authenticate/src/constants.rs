//! Shared AUTH/1.0 protocol constants and on-board memory layout constants.

/// Wire protocol version reported in the `HELLO` frame.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Maximum length of one protocol frame (excluding CR/LF terminator).
pub const MAX_FRAME_LEN: usize = 96;

/// Lengths used by the challenge-response math (all 32 bytes).
pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 32;
pub const MAC_LEN: usize = 32;

/// How long the device waits for Button A after accepting an `AUTH` challenge.
pub const AUTH_WINDOW_MS: u32 = 5_000;

/// How long the button self-test (`BTN_TEST`) waits for a click.
pub const BTN_TEST_WINDOW_MS: u32 = 30_000;

/// Minimum stable-low time for which a press is considered real (debounce floor).
pub const BTN_DEBOUNCE_MS: u32 = 20;

// ── UICR layout for the key store (micro:bit v2 / nRF52833) ──────────────
/// Customer region start of the 32-byte secret key.
pub const UICR_KEY_BASE: u32 = 0x1000_1080;
/// Validity magic word written next to the key (erased UICR == 0xFFFF_FFFF).
pub const UICR_MAGIC_ADDR: u32 = 0x1000_1100;
/// APPROTECT (readback protection) register inside UICR.
pub const UICR_APPROTECT_ADDR: u32 = 0x1000_1208;
/// Magic value proving the key has been provisioned.
pub const UICR_MAGIC: u32 = 0xA17E_E2B5;

/// Serial settings used on the DAPLink UART bridge.
pub const UART_BAUD: u32 = 115_200;