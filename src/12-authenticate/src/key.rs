//! UICR key store + readback protection (nRF52833).
//!
//! The 32-byte secret lives in the UICR customer region. It is readable by the
//! code at runtime (the auth flow needs it), but once `APPROTECT` is enabled
//! external debuggers (SWD) can no longer read it without a full-chip erase.

use core::ptr;

use cortex_m::peripheral::SCB;
use nrf52833_pac as pac;
use pac::{nvmc, NVMC};

use authenticate::constants::{UICR_APPROTECT_ADDR, UICR_KEY_BASE, UICR_MAGIC, UICR_MAGIC_ADDR};

fn regs() -> &'static nvmc::RegisterBlock {
    unsafe { &*NVMC::ptr() }
}

fn wait_ready() {
    while regs().ready.read().ready().is_busy() {}
}

/// Read the provisioned key. `None` when the magic word is absent (unprovisioned).
pub fn load_key() -> Option<[u8; 32]> {
    let magic = unsafe { ptr::read_volatile(UICR_MAGIC_ADDR as *const u32) };
    if magic != UICR_MAGIC {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, chunk) in key.chunks_exact_mut(4).enumerate() {
        let word = unsafe { ptr::read_volatile((UICR_KEY_BASE + (i as u32) * 4) as *const u32) };
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    Some(key)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyError {
    /// UICR did not read back as all-ones after the erase.
    EraseFail,
    /// The written key / magic did not read back correctly.
    ReadbackFail,
}

/// Erase the whole UICR page back to all-ones (needed before writing new
/// bits: flash writes can only turn 1s into 0s).
///
/// Note: this also clears the other UICR settings (APPROTECT, NFCPINS, ...);
/// they are all at their erased defaults on this board anyway.
fn erase_uicr() {
    let r = regs();
    r.config.write(|w| w.wen().een()); // erase mode
    wait_ready();
    // ERASEUICR is a trigger register: writing 1 starts the erase.
    r.eraseuicr.write(|w| w.eraseuicr().erase());
    wait_ready();
    r.config.write(|w| w.wen().ren());
    wait_ready();
}

fn uicr_word(addr: u32) -> u32 {
    unsafe { ptr::read_volatile(addr as *const u32) }
}

/// Flash a new key (plus magic word) into UICR and verify by read-back.
pub fn provision(key: &[u8; 32]) -> Result<(), KeyError> {
    if load_key().as_ref() == Some(key) {
        return Ok(()); // already there — don't wear the flash
    }
    erase_uicr();
    let erased = (0..8).all(|i| uicr_word(UICR_KEY_BASE + i * 4) == 0xFFFF_FFFF)
        && uicr_word(UICR_MAGIC_ADDR) == 0xFFFF_FFFF;
    if !erased {
        return Err(KeyError::EraseFail);
    }

    let r = regs();
    r.config.write(|w| w.wen().wen()); // write mode
    wait_ready();
    for (i, chunk) in key.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        unsafe {
            ptr::write_volatile((UICR_KEY_BASE + (i as u32) * 4) as *mut u32, word);
        }
    }
    unsafe {
        ptr::write_volatile(UICR_MAGIC_ADDR as *mut u32, UICR_MAGIC);
    }
    r.config.write(|w| w.wen().ren());
    wait_ready();

    if load_key().as_ref() != Some(key) {
        return Err(KeyError::ReadbackFail);
    }
    Ok(())
}

/// Enable *hard* readback protection: `UICR.APPROTECT = 0x0000_0000`.
/// Takes effect after the next reset. From then on SWD can't read flash/RAM/UICR
/// (the key becomes unrecoverable — by design). Unlock = full chip erase.
pub fn enable_readback_protection() {
    let r = regs();
    r.config.write(|w| w.wen().wen());
    wait_ready();
    unsafe {
        ptr::write_volatile(UICR_APPROTECT_ADDR as *mut u32, 0x0000_0000);
    }
    r.config.write(|w| w.wen().ren());
    wait_ready();
}

/// `true` once `enable_readback_protection` has been written (UICR value 0).
/// An erased UICR reads `0xFFFF_FFFF`.
pub fn is_locked() -> bool {
    unsafe { ptr::read_volatile(UICR_APPROTECT_ADDR as *const u32) == 0 }
}

/// Hard system reset (required for APPROTECT to become active).
pub fn reset_chip() -> ! {
    SCB::sys_reset()
}
