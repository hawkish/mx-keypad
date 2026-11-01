//! The keypad's own shift: a key that switches the other keys to their
//! shifted bindings, while held, until tapped again, or for one press.
//!
//! The modes combine: shift is on while any shift key is held, while a
//! toggle is latched, or until the next press after a `once`.

use std::collections::HashSet;
use std::hash::Hash;

/// How a shift key turns shift on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// While the key is held.
    Hold,
    /// Each tap turns it on or off.
    Toggle,
    /// For the next key press only; a second tap cancels it.
    Once,
}

impl Mode {
    /// Parse `hold`, `toggle` or `once`.
    pub fn parse(name: &str) -> anyhow::Result<Mode> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "hold" => Mode::Hold,
            "toggle" => Mode::Toggle,
            "once" => Mode::Once,
            _ => anyhow::bail!("{name:?}: shift modes are hold, toggle and once"),
        })
    }
}

/// Whether shift is on, and why.
#[derive(Debug)]
pub struct Shift<K> {
    /// Shift keys in `hold` mode that are down.
    held: HashSet<K>,
    /// A `toggle` shift key turned it on.
    latched: bool,
    /// A `once` shift key turned it on for the next press.
    once: bool,
}

impl<K> Default for Shift<K> {
    fn default() -> Self {
        Self {
            held: HashSet::new(),
            latched: false,
            once: false,
        }
    }
}

impl<K: Eq + Hash + Copy> Shift<K> {
    /// Whether the other keys use their shifted bindings now.
    #[must_use]
    pub fn active(&self) -> bool {
        !self.held.is_empty() || self.latched || self.once
    }

    /// Shift key `key`, in `mode`, went down.
    pub fn press(&mut self, key: K, mode: Mode) {
        match mode {
            Mode::Hold => {
                self.held.insert(key);
            }
            Mode::Toggle => self.latched = !self.latched,
            Mode::Once => self.once = !self.once,
        }
    }

    /// `key` came up; `true` when that turned off a held shift.
    pub fn release(&mut self, key: K) -> bool {
        self.held.remove(&key) && !self.active()
    }

    /// Another key fired while shift was on: a `once` shift is spent.
    /// `true` when that turned shift off.
    pub fn consume(&mut self) -> bool {
        let was = self.once;
        self.once = false;
        was && !self.active()
    }

    /// Turn every shift off, as when the keypad goes away.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_lasts_while_the_key_is_down() {
        let mut shift = Shift::default();
        shift.press(8, Mode::Hold);
        assert!(shift.active());
        assert!(!shift.release(4));
        assert!(shift.active());
        assert!(shift.release(8));
        assert!(!shift.active());
    }

    #[test]
    fn toggle_flips_on_each_tap() {
        let mut shift = Shift::default();
        shift.press(8, Mode::Toggle);
        assert!(!shift.release(8));
        assert!(shift.active());
        assert!(!shift.consume());
        assert!(shift.active());
        shift.press(8, Mode::Toggle);
        assert!(!shift.active());
    }

    #[test]
    fn once_is_spent_by_the_next_key_or_cancelled_by_a_second_tap() {
        let mut shift = Shift::default();
        shift.press(8, Mode::Once);
        assert!(shift.active());
        assert!(shift.consume());
        assert!(!shift.active());

        shift.press(8, Mode::Once);
        shift.press(8, Mode::Once);
        assert!(!shift.active());
    }

    #[test]
    fn modes_combine() {
        let mut shift = Shift::default();
        shift.press(8, Mode::Toggle);
        shift.press(9, Mode::Hold);
        assert!(!shift.release(9));
        assert!(shift.active());
        shift.clear();
        assert!(!shift.active());
    }

    #[test]
    fn modes_parse_their_names() {
        assert_eq!(Mode::parse("Toggle").unwrap(), Mode::Toggle);
        assert!(Mode::parse("caps").is_err());
    }
}
