//! Blinking keys: which phase a blinking key is in, and when it next flips.
//!
//! The keypad has no blink of its own, so a blinking key is redrawn every
//! interval, alternating between two images. Every key counts from one
//! shared start, so keys with the same interval flip together and a page
//! switch does not reset them.

use std::time::{Duration, Instant};

/// Whether a key blinking every `interval` since `epoch` shows its alternate
/// face at `now`: the first interval shows the normal face, the next the
/// alternate, and so on.
#[must_use]
pub fn alternate(epoch: Instant, now: Instant, interval: Duration) -> bool {
    let interval = interval.as_millis().max(1);
    let elapsed = now.saturating_duration_since(epoch).as_millis();
    (elapsed / interval) % 2 == 1
}

/// When a key blinking every `interval` since `epoch` next flips, after `now`.
#[must_use]
pub fn next_flip(epoch: Instant, now: Instant, interval: Duration) -> Instant {
    let step = interval.as_millis().max(1);
    let elapsed = now.saturating_duration_since(epoch).as_millis();
    let flips = elapsed / step + 1;
    let at = u64::try_from(flips * step).unwrap_or(u64::MAX);
    epoch + Duration::from_millis(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: Duration = Duration::from_millis(500);

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn phases_alternate_every_interval() {
        let start = Instant::now();
        assert!(!alternate(start, start, HALF));
        assert!(!alternate(start, ms(start, 499), HALF));
        assert!(alternate(start, ms(start, 500), HALF));
        assert!(alternate(start, ms(start, 999), HALF));
        assert!(!alternate(start, ms(start, 1000), HALF));
        assert!(alternate(start, ms(start, 60_700), HALF));
    }

    #[test]
    fn the_next_flip_is_the_next_interval_boundary() {
        let start = Instant::now();
        assert_eq!(next_flip(start, start, HALF), ms(start, 500));
        assert_eq!(next_flip(start, ms(start, 499), HALF), ms(start, 500));
        assert_eq!(next_flip(start, ms(start, 500), HALF), ms(start, 1000));
        assert_eq!(next_flip(start, ms(start, 60_700), HALF), ms(start, 61_000));
    }

    #[test]
    fn intervals_are_independent() {
        let start = Instant::now();
        let fast = Duration::from_millis(200);
        let at = ms(start, 450);
        assert!(!alternate(start, at, fast));
        assert!(!alternate(start, at, HALF));
        assert_eq!(next_flip(start, at, fast), ms(start, 600));
        assert_eq!(next_flip(start, at, HALF), ms(start, 500));
    }
}
