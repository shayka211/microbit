//! Pure, host-testable logic: the "armed session" and the debounce filter.

/// What the device is waiting for while armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArmedKind {
    /// An `AUTH` challenge: the press answers with an HMAC.
    Auth,
    /// The `BTN_TEST` self-test: the press is the only thing that produces output.
    BtnTest,
}

/// Outcome of an armed session, emitted exactly once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArmedEvent {
    /// Button press observed, `elapsed_ms` since the session started.
    Press(u32),
    /// The window elapsed with no press.
    Timeout,
}

/// An armed ["press or timeout"] session.
///
/// The interesting property this encodes (and that the unit tests pin down):
/// while armed, **nothing is emitted until a press event or the timeout** —
/// that is the "silence until press" behaviour of both the auth flow and the
/// button self-test.
pub struct Session {
    pub kind: ArmedKind,
    pub window_ms: u32,
    elapsed_ms: u32,
    done: bool,
}

impl Session {
    pub fn new(kind: ArmedKind, window_ms: u32) -> Self {
        Session {
            kind,
            window_ms,
            elapsed_ms: 0,
            done: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn elapsed_ms(&self) -> u32 {
        self.elapsed_ms
    }

    /// Advance with a monotonic `now_ms` (relative to session start) and an
    /// optional press (elapsed ms when a debounced press fired).
    ///
    /// Returns `Some(event)` at most once, then stays `None` forever.
    pub fn step(&mut self, now_ms: u32, press: Option<u32>) -> Option<ArmedEvent> {
        if self.done {
            return None;
        }
        self.elapsed_ms = now_ms;
        if let Some(elapsed) = press {
            self.done = true;
            return Some(ArmedEvent::Press(elapsed));
        }
        if now_ms >= self.window_ms {
            self.done = true;
            return Some(ArmedEvent::Timeout);
        }
        None
    }
}

/// Rising-edge debounce filter for a normally-open button pulled high.
///
/// A press is confirmed when the input has been continuously low for at least
/// `require_ms` (sampled every `sample_ms`). The confirmation fires exactly
/// once; further low samples stay silent until the button is released.
pub struct Debounce {
    pub sample_ms: u32,
    pub require_ms: u32,
    low_run_ms: u32,
    fired: bool,
}

impl Debounce {
    pub fn new(sample_ms: u32, require_ms: u32) -> Self {
        Debounce {
            sample_ms,
            require_ms,
            low_run_ms: 0,
            fired: false,
        }
    }

    pub fn reset(&mut self) {
        self.low_run_ms = 0;
        self.fired = false;
    }

    pub fn confirmed(&self) -> bool {
        self.low_run_ms >= self.require_ms
    }

    /// Feed one sample. Returns `Some(())` exactly once, on the sample that
    /// crosses the `require_ms` threshold. Held presses are then silent.
    pub fn feed(&mut self, pressed: bool) -> Option<()> {
        if pressed {
            if self.fired {
                return None;
            }
            self.low_run_ms += self.sample_ms;
            if self.confirmed() {
                self.low_run_ms = self.require_ms;
                self.fired = true;
                return Some(());
            }
        } else {
            self.low_run_ms = 0;
            self.fired = false;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armed_session_is_silent_until_press() {
        let mut s = Session::new(ArmedKind::BtnTest, 1_000);
        // Whole window elapses, no press → no output at all.
        for t in (0..1_000).step_by(10) {
            assert!(s.step(t, None).is_none(), "silence until something happens");
        }
        assert!(!s.is_done());
        // Timeout is the first and only event.
        assert_eq!(s.step(1_000, None), Some(ArmedEvent::Timeout));
        assert!(s.is_done());
        // And it never fires twice.
        assert_eq!(s.step(2_000, Some(99)), None);
        assert_eq!(s.step(10_000, None), None);
    }

    #[test]
    fn armed_session_press_wins_over_timeout() {
        let mut s = Session::new(ArmedKind::Auth, 5_000);
        for t in (0..4_900).step_by(100) {
            assert!(s.step(t, None).is_none());
        }
        // Press arrives late in the window → Press, exactly once.
        assert_eq!(s.step(4_900, Some(4_900)), Some(ArmedEvent::Press(4_900)));
        assert_eq!(s.step(5_000, Some(5_000)), None);
        assert_eq!(s.step(10_000, None), None);
    }

    #[test]
    fn debounce_requires_minimum_stable_time() {
        let mut d = Debounce::new(10, 20);
        // Single sample below threshold → no event.
        assert!(d.feed(true).is_none());
        assert!(!d.confirmed());
        // Two samples (20 ms) → confirmed event fires once.
        assert_eq!(d.feed(true), Some(()));
        assert!(d.confirmed());
        // Held low keeps producing nothing.
        assert!(d.feed(true).is_none());
        assert!(d.feed(true).is_none());
        // Release resets.
        assert!(d.feed(false).is_none());
        assert!(!d.confirmed());
    }

    #[test]
    fn debounce_rejects_short_glitch() {
        let mut d = Debounce::new(10, 30);
        assert!(d.feed(true).is_none()); // 10 ms
        assert!(d.feed(false).is_none()); // released
        assert!(!d.confirmed());
        assert!(d.feed(true).is_none()); // back low, restarts the run
        assert!(d.feed(true).is_none());
        assert!(d.feed(true).is_some()); // 30 ms → confirmed only on 3rd
    }
}