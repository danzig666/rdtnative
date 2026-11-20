//! Lean-back timing for the frame loop: how long nobody has pressed a button, when the idle screen
//! is due, and whether the TV's screensaver is held off. Only real input counts (keys, the
//! pointer, the wheel, the dev remote), never the events the app makes itself. The screensaver is
//! held off only while something on screen changes by itself (a slideshow moving on, a video
//! playing), and no longer after hours without a button: the TV may have been left on.

/// After this long without a button the TV's screensaver may come again, whatever plays.
pub const AWAKE_MAX_MS: u32 = 4 * 60 * 60 * 1000;

pub struct Watch {
    /// The last real input.
    input_at: u32,
    /// The last real input or activity (a video playing, a slideshow open, a menu, the background).
    busy_at: u32,
    /// The screensaver is held off.
    awake: bool,
}

impl Watch {
    pub fn new(now: u32) -> Watch {
        Watch { input_at: now, busy_at: now, awake: false }
    }

    /// Once per frame: `input` when real input arrived, `busy` while the idle screen must wait.
    pub fn note(&mut self, now: u32, input: bool, busy: bool) {
        if input {
            self.input_at = now;
        }
        if input || busy {
            self.busy_at = now;
        }
    }

    /// The idle screen is due after `minutes` (0: never) without input or activity; asking when
    /// it is due starts the wait again.
    pub fn idle_due(&mut self, now: u32, minutes: u32) -> bool {
        if minutes == 0 || now.wrapping_sub(self.busy_at) < minutes.saturating_mul(60_000) {
            return false;
        }
        self.busy_at = now;
        true
    }

    /// Whether the screensaver is held off, given that the picture is `changing` by itself:
    /// `Some(on)` when that changed.
    pub fn awake(&mut self, now: u32, changing: bool) -> Option<bool> {
        let want = changing && now.wrapping_sub(self.input_at) < AWAKE_MAX_MS;
        (want != self.awake).then(|| {
            self.awake = want;
            want
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_screen_after_minutes_without_input_or_activity() {
        let mut w = Watch::new(1_000);
        assert!(!w.idle_due(1_000 + 4 * 60_000, 5));
        assert!(!w.idle_due(1_000 + 10 * 60_000, 0), "off");
        assert!(w.idle_due(1_000 + 5 * 60_000, 5));
        // Once due, the wait starts again.
        assert!(!w.idle_due(1_000 + 6 * 60_000, 5));
        // A key, or a video playing, puts it off.
        w.note(20 * 60_000, true, false);
        assert!(!w.idle_due(24 * 60_000, 5));
        w.note(24 * 60_000, false, true);
        assert!(!w.idle_due(28 * 60_000, 5));
        assert!(w.idle_due(29 * 60_000, 5));
        // The SDL clock wraps.
        let mut w = Watch::new(u32::MAX - 1_000);
        assert!(w.idle_due(5 * 60_000, 5));
    }

    #[test]
    fn screensaver_held_off_while_moving_for_hours_at_most() {
        let mut w = Watch::new(0);
        assert_eq!(w.awake(10, false), None);
        assert_eq!(w.awake(20, true), Some(true));
        assert_eq!(w.awake(30, true), None);
        assert_eq!(w.awake(40, false), Some(false));
        assert_eq!(w.awake(50, true), Some(true));
        // Hours without a button: the TV may sleep, until someone presses one.
        assert_eq!(w.awake(AWAKE_MAX_MS, true), Some(false));
        w.note(AWAKE_MAX_MS + 10, true, false);
        assert_eq!(w.awake(AWAKE_MAX_MS + 20, true), Some(true));
        // Activity alone (a video) is not a button.
        w.note(2 * AWAKE_MAX_MS, false, true);
        assert_eq!(w.awake(2 * AWAKE_MAX_MS + 30, true), Some(false));
        // Coming back from the background hours later counts as one (the frame loop says so).
        w.note(3 * AWAKE_MAX_MS, true, true);
        assert_eq!(w.awake(3 * AWAKE_MAX_MS + 1, true), Some(true));
    }
}
