//! Long press: a key with a second action fires its first on a short tap and
//! its second once held past a threshold, never both.
//!
//! The decision waits for the release or the threshold, whichever comes
//! first, so such a key acts on release (short) or while still held (long).
//! Keys without a second action are not tracked here and act on press.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Which of a key's two actions to fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fire<A> {
    /// The short action: released before the threshold.
    Short(A),
    /// The long action: held past the threshold.
    Long(A),
}

/// Presses waiting to be decided, by key.
#[derive(Debug)]
pub struct LongPress<K, A> {
    threshold: Duration,
    /// Since when each undecided key is down, with its short and long actions
    /// as they were bound at the press.
    pending: HashMap<K, (Instant, A, A)>,
}

impl<K: Eq + Hash + Copy, A> LongPress<K, A> {
    /// Track presses with `threshold` between a short and a long one.
    #[must_use]
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            pending: HashMap::new(),
        }
    }

    /// Change the threshold for presses from now on.
    pub fn set_threshold(&mut self, threshold: Duration) {
        self.threshold = threshold;
    }

    /// `key` went down at `now`, bound to `short` and `long`.
    pub fn down(&mut self, key: K, now: Instant, short: A, long: A) {
        self.pending.insert(key, (now, short, long));
    }

    /// `key` came up at `now`. A press still undecided fires: short before
    /// the threshold, long at or past it (when no tick caught it in time).
    /// `None` when the press was already decided, or never tracked.
    pub fn up(&mut self, key: K, now: Instant) -> Option<Fire<A>> {
        let (since, short, long) = self.pending.remove(&key)?;
        Some(if now.duration_since(since) < self.threshold {
            Fire::Short(short)
        } else {
            Fire::Long(long)
        })
    }

    /// The long actions of every key held past the threshold by `now`. Each
    /// fires once; its release then does nothing.
    pub fn tick(&mut self, now: Instant) -> Vec<(K, A)> {
        let due: Vec<K> = self
            .pending
            .iter()
            .filter(|(_, (since, _, _))| now.duration_since(*since) >= self.threshold)
            .map(|(key, _)| *key)
            .collect();
        due.into_iter()
            .filter_map(|key| self.pending.remove(&key).map(|(_, _, long)| (key, long)))
            .collect()
    }

    /// When the next undecided press turns long, if any is pending.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|(since, _, _)| *since + self.threshold)
            .min()
    }

    /// Forget every undecided press, as when the keypad goes away.
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(500);

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn a_tap_fires_the_short_action_on_release() {
        let start = Instant::now();
        let mut press = LongPress::new(T);
        press.down(1, start, "short", "long");
        assert!(press.tick(ms(start, 200)).is_empty());
        assert_eq!(press.up(1, ms(start, 300)), Some(Fire::Short("short")));
        assert_eq!(press.next_deadline(), None);
    }

    #[test]
    fn a_hold_fires_the_long_action_once_and_its_release_nothing() {
        let start = Instant::now();
        let mut press = LongPress::new(T);
        press.down(1, start, "short", "long");
        assert_eq!(press.next_deadline(), Some(ms(start, 500)));
        assert_eq!(press.tick(ms(start, 600)), [(1, "long")]);
        assert!(press.tick(ms(start, 700)).is_empty());
        assert_eq!(press.up(1, ms(start, 800)), None);
    }

    #[test]
    fn a_release_exactly_at_the_threshold_is_long() {
        let start = Instant::now();
        let mut press = LongPress::new(T);
        press.down(1, start, "short", "long");
        assert_eq!(press.up(1, ms(start, 500)), Some(Fire::Long("long")));
    }

    #[test]
    fn keys_are_decided_independently() {
        let start = Instant::now();
        let mut press = LongPress::new(T);
        press.down(1, start, "s1", "l1");
        press.down(2, ms(start, 300), "s2", "l2");
        assert_eq!(press.next_deadline(), Some(ms(start, 500)));
        assert_eq!(press.tick(ms(start, 550)), [(1, "l1")]);
        assert_eq!(press.up(2, ms(start, 600)), Some(Fire::Short("s2")));
    }

    #[test]
    fn an_untracked_release_fires_nothing() {
        let mut press: LongPress<u8, &str> = LongPress::new(T);
        assert_eq!(press.up(3, Instant::now()), None);
    }
}
