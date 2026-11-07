//! What the 5×5 LED matrix shows, as pure data (host-testable).
//!
//! The firmware's display task only does the pin wiggling; everything about
//! *which* LEDs are lit and *when* that changes lives here:
//!
//! - [`Pattern`]: one image, optionally blinking.
//! - [`View`]: what the supervisor asks for — a resting pattern plus an
//!   optional short "flash" (tick / cross) shown on top of it.
//! - [`Screen`]: the display task's state; given the time, says what bitmap
//!   to draw and when it will next change (so the task can sleep until then).
//!
//! All times are plain milliseconds from any monotonic clock.

/// One byte per row (row 0 = top). Bit 4 is the leftmost column, so a binary
/// literal reads like the picture: `0b10001` = both outer LEDs lit.
pub type Bitmap = [u8; 5];

/// All LEDs off.
pub const BLANK: Bitmap = [0; 5];

/// A blinking pattern is on for this long, then off for this long.
pub const BLINK_HALF_PERIOD_MS: u64 = 500;

/// How long a [`View::flash`] pattern stays up before the resting one returns.
pub const FLASH_MS: u64 = 1_000;

#[rustfmt::skip]
const TICK: Bitmap = [
    0b00000,
    0b00001,
    0b00010,
    0b10100,
    0b01000,
];

#[rustfmt::skip]
const CROSS: Bitmap = [
    0b10001,
    0b01010,
    0b00100,
    0b01010,
    0b10001,
];

#[rustfmt::skip]
const BAR: Bitmap = [
    0b00000,
    0b00000,
    0b11111,
    0b00000,
    0b00000,
];

#[rustfmt::skip]
const CORNERS: Bitmap = [
    0b10001,
    0b00000,
    0b00000,
    0b00000,
    0b10001,
];

/// Images the device can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// Everything off (provisioned, nothing to do).
    Idle,
    /// Blinking middle bar — "challenge accepted, press A".
    Waiting,
    /// Tick — the press was accepted.
    Success,
    /// Cross — the window elapsed without a press.
    Fail,
    /// Blinking corner dots — no key provisioned yet.
    Unprovisioned,
}

impl Pattern {
    /// The resting pattern for a device with / without a key.
    pub const fn resting(provisioned: bool) -> Pattern {
        if provisioned {
            Pattern::Idle
        } else {
            Pattern::Unprovisioned
        }
    }

    /// The image while "on".
    pub const fn image(self) -> Bitmap {
        match self {
            Pattern::Idle => BLANK,
            Pattern::Waiting => BAR,
            Pattern::Success => TICK,
            Pattern::Fail => CROSS,
            Pattern::Unprovisioned => CORNERS,
        }
    }

    pub const fn blinks(self) -> bool {
        matches!(self, Pattern::Waiting | Pattern::Unprovisioned)
    }

    /// Bitmap `elapsed_ms` after the pattern started. Blinking patterns start
    /// in the *on* phase so the user sees feedback immediately.
    pub fn bitmap_at(self, elapsed_ms: u64) -> Bitmap {
        if self.blinks() && (elapsed_ms / BLINK_HALF_PERIOD_MS) % 2 == 1 {
            BLANK
        } else {
            self.image()
        }
    }

    /// Next time (ms since pattern start, strictly after `elapsed_ms`) at
    /// which the bitmap changes. `None` = static forever.
    pub fn next_change_ms(self, elapsed_ms: u64) -> Option<u64> {
        if self.blinks() {
            Some((elapsed_ms / BLINK_HALF_PERIOD_MS + 1) * BLINK_HALF_PERIOD_MS)
        } else {
            None
        }
    }
}

/// What the supervisor wants on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    /// Shown indefinitely.
    pub base: Pattern,
    /// Shown for [`FLASH_MS`] first, then `base` takes over.
    pub flash: Option<Pattern>,
}

impl View {
    pub const fn steady(base: Pattern) -> View {
        View { base, flash: None }
    }

    pub const fn flash(flash: Pattern, then: Pattern) -> View {
        View {
            base: then,
            flash: Some(flash),
        }
    }
}

/// Display-task state: the current view plus when each part started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Screen {
    base: Pattern,
    base_since: u64,
    /// `(pattern, started, ends)`.
    flash: Option<(Pattern, u64, u64)>,
}

impl Screen {
    pub const fn new(now_ms: u64) -> Screen {
        Screen {
            base: Pattern::Idle,
            base_since: now_ms,
            flash: None,
        }
    }

    pub fn apply(&mut self, view: View, now_ms: u64) {
        // When the flash is over, the base pattern starts fresh (blink on).
        let flash_end = view.flash.map_or(now_ms, |_| now_ms + FLASH_MS);
        self.base = view.base;
        self.base_since = flash_end;
        self.flash = view.flash.map(|p| (p, now_ms, flash_end));
    }

    /// The pattern on screen at `now_ms`, and when it started.
    fn active(&self, now_ms: u64) -> (Pattern, u64) {
        match self.flash {
            Some((p, start, end)) if now_ms < end => (p, start),
            _ => (self.base, self.base_since),
        }
    }

    pub fn bitmap(&self, now_ms: u64) -> Bitmap {
        let (p, start) = self.active(now_ms);
        p.bitmap_at(now_ms.saturating_sub(start))
    }

    /// Absolute time of the next bitmap change, or `None` if the screen will
    /// stay as it is until a new [`View`] arrives.
    pub fn next_change(&self, now_ms: u64) -> Option<u64> {
        if let Some((p, start, end)) = self.flash {
            if now_ms < end {
                let own = p.next_change_ms(now_ms - start).map(|t| t + start);
                return Some(own.map_or(end, |t| t.min(end)));
            }
        }
        let elapsed = now_ms.saturating_sub(self.base_since);
        self.base
            .next_change_ms(elapsed)
            .map(|t| t + self.base_since)
    }
}

/// Is the LED at (`row`, `col`) lit?
pub const fn lit(bitmap: &Bitmap, row: usize, col: usize) -> bool {
    bitmap[row] & (0b10000 >> col) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_order_matches_the_picture() {
        assert!(lit(&CORNERS, 0, 0));
        assert!(lit(&CORNERS, 0, 4));
        assert!(!lit(&CORNERS, 0, 1));
        assert!(lit(&TICK, 1, 4));
        assert!(lit(&TICK, 4, 1));
    }

    #[test]
    fn static_patterns_never_change() {
        for p in [Pattern::Idle, Pattern::Success, Pattern::Fail] {
            assert_eq!(p.bitmap_at(0), p.bitmap_at(12_345));
            assert_eq!(p.next_change_ms(0), None);
        }
        assert_eq!(Pattern::Idle.bitmap_at(0), BLANK);
    }

    #[test]
    fn blinking_starts_on_and_toggles_every_half_period() {
        let p = Pattern::Waiting;
        assert_eq!(p.bitmap_at(0), BAR, "visible immediately after READY");
        assert_eq!(p.bitmap_at(499), BAR);
        assert_eq!(p.bitmap_at(500), BLANK);
        assert_eq!(p.bitmap_at(999), BLANK);
        assert_eq!(p.bitmap_at(1_000), BAR);
        assert_eq!(p.next_change_ms(0), Some(500));
        assert_eq!(p.next_change_ms(500), Some(1_000));
        assert_eq!(p.next_change_ms(731), Some(1_000));
    }

    #[test]
    fn resting_pattern_depends_on_provisioning() {
        assert_eq!(Pattern::resting(true), Pattern::Idle);
        assert_eq!(Pattern::resting(false), Pattern::Unprovisioned);
    }

    #[test]
    fn idle_screen_sleeps_forever() {
        let mut s = Screen::new(0);
        s.apply(View::steady(Pattern::Idle), 100);
        assert_eq!(s.bitmap(100), BLANK);
        assert_eq!(s.next_change(100), None);
    }

    #[test]
    fn blinking_screen_wakes_only_on_edges() {
        let mut s = Screen::new(0);
        s.apply(View::steady(Pattern::Waiting), 1_000);
        assert_eq!(s.bitmap(1_000), BAR);
        assert_eq!(s.next_change(1_000), Some(1_500));
        assert_eq!(s.bitmap(1_600), BLANK);
        assert_eq!(s.next_change(1_600), Some(2_000));
    }

    #[test]
    fn flash_then_back_to_base() {
        let mut s = Screen::new(0);
        s.apply(View::flash(Pattern::Success, Pattern::Idle), 10_000);
        assert_eq!(s.bitmap(10_000), TICK);
        assert_eq!(s.bitmap(10_999), TICK);
        assert_eq!(s.next_change(10_000), Some(10_000 + FLASH_MS));
        assert_eq!(s.bitmap(11_000), BLANK);
        assert_eq!(s.next_change(11_000), None);
    }

    #[test]
    fn blinking_base_restarts_on_phase_after_flash() {
        let mut s = Screen::new(0);
        s.apply(View::flash(Pattern::Fail, Pattern::Unprovisioned), 0);
        assert_eq!(s.bitmap(500), CROSS);
        assert_eq!(s.bitmap(FLASH_MS), CORNERS, "base starts in the on phase");
        assert_eq!(
            s.next_change(FLASH_MS),
            Some(FLASH_MS + BLINK_HALF_PERIOD_MS)
        );
    }

    #[test]
    fn new_view_cancels_running_flash() {
        let mut s = Screen::new(0);
        s.apply(View::flash(Pattern::Success, Pattern::Idle), 0);
        s.apply(View::steady(Pattern::Waiting), 200);
        assert_eq!(s.bitmap(300), BAR);
    }
}
