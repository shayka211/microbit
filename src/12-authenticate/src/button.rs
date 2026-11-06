//! Debounced Button A (P0.14, active-low with internal pull-up).
//!
//! Uses GPIOTE edge-triggered interrupts — the CPU sleeps until a real
//! pin transition occurs. No polling.

use crate::logger::{flush, Level};
use authenticate::constants::BTN_DEBOUNCE_MS;
use embassy_nrf::gpio::{AnyPin, Input, Pull};
use embassy_time::{Duration, Instant, Timer};

pub struct ButtonA {
    input: Input<'static, AnyPin>,
}

impl ButtonA {
    pub fn new(pin: AnyPin) -> Self {
        flush(Level::Debug, format_args!("button: P0.14 as input (pull-up)"));
        Self {
            input: Input::new(pin, Pull::Up),
        }
    }

    /// Wait for a *new* press. First waits for the button to be released
    /// (so a button held over from a previous session does not re-trigger),
    /// then returns the elapsed milliseconds once the press is debounced.
    ///
    /// Uses GPIOTE interrupts — no polling, the CPU sleeps until a real
    /// pin transition occurs.
    pub async fn wait_press(&mut self) -> u32 {
        let start = Instant::now();
        flush(Level::Debug, format_args!("button: waiting for release..."));

        // If the button is currently held, wait for release via GPIOTE.
        if self.input.is_low() {
            self.input.wait_for_high().await;
        }

        flush(Level::Debug, format_args!("button: released, awaiting press"));

        loop {
            // Wait for a falling edge — interrupt-driven, NO polling.
            self.input.wait_for_low().await;

            // Debounce: wait the required stable time, then confirm.
            Timer::after(Duration::from_millis(BTN_DEBOUNCE_MS as u64)).await;

            if self.input.is_low() {
                let elapsed = start.elapsed().as_millis() as u32;
                flush(
                    Level::Info,
                    format_args!("button: debounced press after ~{elapsed}ms"),
                );
                return elapsed;
            }

            // Glitch / noise: pin went back high before debounce window elapsed.
            // Loop and wait for the next real falling edge.
        }
    }
}