//! A running [`Setup`](crate::config::Setup): what each key shows and does
//! right now, held keys, shift, long press, blink, and the child processes a
//! `run` or `open` action started. This is the engine behind `mx-keypad
//! run`, exposed so another program sharing this crate's [`Keypad`] — such
//! as tmux-legion, running its own page alongside an agent-status one —
//! can run the same kind of config without reimplementing action dispatch.
//!
//! A `Session` never touches a [`Keypad`] or a terminal: it decides what to
//! draw ([`Session::face`]) and performs an action's side effects
//! (keystrokes, a spawned process, a changed page) when told a button went
//! down or up. Drawing, reading presses, and diverting the page buttons stay
//! the caller's job — that's what keeps this type testable without hardware.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::blink;
use crate::config::{Action, Setup};
use crate::keys::{self, Chord};
use crate::press::{Fire, LongPress};
use crate::render::Face;
use crate::shift::{self, Shift};
use crate::Button;

/// A running setup: current page, held keys, shift, long press, blink epoch,
/// and the `run`/`open` children still alive.
pub struct Session {
    setup: Setup,
    page: usize,
    held: HashMap<Button, Held>,
    long: LongPress<Button, Action>,
    blink_epoch: Instant,
    shift: Shift<Button>,
    children: Vec<Child>,
}

impl Session {
    /// Start a session on `setup`'s first page.
    #[must_use]
    pub fn new(setup: Setup) -> Session {
        let threshold = setup.long_press;
        Session {
            setup,
            page: 0,
            held: HashMap::new(),
            long: LongPress::new(threshold),
            blink_epoch: Instant::now(),
            shift: Shift::default(),
            children: Vec::new(),
        }
    }

    /// Replace the setup — after a config file reload, say — trying to stay
    /// on a page of the same name rather than jumping back to the first.
    /// Held keys and shift are left as they are.
    pub fn reload(&mut self, setup: Setup) {
        let name = self.setup.pages[self.page].name.clone();
        self.page = setup.pages.iter().position(|p| p.name == name).unwrap_or(0);
        self.long.set_threshold(setup.long_press);
        self.setup = setup;
    }

    /// The setup currently running.
    #[must_use]
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The face key `i` (0-based, 0..9) shows at `now`: its blink face
    /// during a blinking key's alternate phase, and shift's bindings while
    /// shift is active.
    #[must_use]
    pub fn face(&self, i: usize, now: Instant) -> Face {
        let page = &self.setup.pages[self.page];
        let face = if self.shift.active() {
            &page.shifted_keys[i].0
        } else {
            &page.keys[i].0
        };
        match &page.blink[i] {
            Some(spec) if blink::alternate(self.blink_epoch, now, spec.interval) => {
                spec.alternate(face)
            }
            _ => face.clone(),
        }
    }

    /// When the next blinking key on the current page flips, if any blinks.
    #[must_use]
    pub fn next_flip(&self, now: Instant) -> Option<Instant> {
        self.setup.pages[self.page]
            .blink
            .iter()
            .flatten()
            .map(|spec| blink::next_flip(self.blink_epoch, now, spec.interval))
            .min()
    }

    /// When a key timing a long press is next due to fire, for the caller's
    /// poll timeout.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.long.next_deadline()
    }

    /// Whether the page buttons should be taken over: more than one page, or
    /// either button bound to something besides turning pages. Otherwise
    /// they should keep their firmware behaviour.
    #[must_use]
    pub fn needs_page_buttons(&self) -> bool {
        self.setup.needs_page_buttons()
    }

    /// The configured panel brightness, if the config sets one. Apply this
    /// once per connection — the brightness is global hardware state, not
    /// per-session.
    #[must_use]
    pub fn brightness(&self) -> Option<u8> {
        self.setup.brightness
    }

    /// `button` went down: act now, or start timing it when it has a
    /// long-press action.
    pub fn down(&mut self, button: Button) -> Result<()> {
        let action = self.action(button);
        match self.long_action(button) {
            Some(long) => {
                self.long.down(button, Instant::now(), action, long);
                Ok(())
            }
            None => self.press(button, &action),
        }
    }

    /// `button` came up: decide a timed press, and let go of anything held.
    pub fn up(&mut self, button: Button) -> Result<()> {
        if let Some(Fire::Short(action) | Fire::Long(action)) =
            self.long.up(button, Instant::now())
        {
            self.press(button, &action)?;
        }
        self.release(button)
    }

    /// Fire the long-press actions of keys held past the threshold, paired
    /// with whichever button each one was so the caller can log a failure
    /// against it.
    pub fn tick(&mut self) -> Vec<(Button, Result<()>)> {
        self.long
            .tick(Instant::now())
            .into_iter()
            .map(|(button, action)| {
                let result = self.press(button, &action);
                (button, result)
            })
            .collect()
    }

    /// Drop finished `run`/`open` children from the tracked list. Call this
    /// about as often as `tick`.
    pub fn reap(&mut self) {
        self.children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }

    /// Let go of everything held, when the session ends.
    pub fn release_all(&mut self) {
        self.long.clear();
        self.shift.clear();
        for (_, held) in self.held.drain() {
            let _ = held.release();
        }
    }

    /// The long-press action `button` has on the current page, if any.
    fn long_action(&self, button: Button) -> Option<Action> {
        match button {
            Button::Key(n) => {
                let page = &self.setup.pages[self.page];
                let long = if self.shift.active() {
                    &page.shifted_long
                } else {
                    &page.long
                };
                long[usize::from(n) - 1].clone()
            }
            Button::PageLeft | Button::PageRight => None,
        }
    }

    /// The action `button` has on the current page.
    fn action(&self, button: Button) -> Action {
        match button {
            Button::Key(n) => {
                let page = &self.setup.pages[self.page];
                let keys = if self.shift.active() {
                    &page.shifted_keys
                } else {
                    &page.keys
                };
                keys[usize::from(n) - 1].1.clone()
            }
            Button::PageLeft => self.setup.left.clone(),
            Button::PageRight => self.setup.right.clone(),
        }
    }

    /// `button` went down with `action` bound to it.
    fn press(&mut self, button: Button, action: &Action) -> Result<()> {
        if let Action::Shift(mode) = action {
            self.shift.press(button, shift::Mode::parse(mode)?);
            return Ok(());
        }
        let result = self.perform_action(button, action);
        // A `once` shift is spent by the next grid key, whatever it did —
        // not by a page button.
        if matches!(button, Button::Key(_)) {
            self.shift.consume();
        }
        result
    }

    /// Do what `action` says, for `button`.
    fn perform_action(&mut self, button: Button, action: &Action) -> Result<()> {
        match action {
            Action::Page(_) => {
                if let Some(to) = self.setup.target(self.page, action) {
                    self.page = to;
                }
            }
            Action::Hold(chord) => {
                let resolved = Chord::parse(chord)?.resolve();
                keys::press_modifiers(resolved.modifiers, true)?;
                keys::press_key(resolved, true)?;
                self.held.insert(button, Held::Hold(resolved));
            }
            Action::Switch(chord) => {
                let resolved = Chord::parse(chord)?.resolve();
                keys::press_modifiers(resolved.modifiers, true)?;
                keys::press_key(resolved, true)?;
                keys::press_key(resolved, false)?;
                self.held.insert(button, Held::Switch(resolved.modifiers));
            }
            Action::Sequence(steps) => {
                // Resolve shortcuts here, on the caller's thread, where the
                // keyboard layout API must be called; delays then run off it.
                let steps = steps.iter().map(prepare).collect::<Result<Vec<_>>>()?;
                std::thread::spawn(move || {
                    for step in &steps {
                        if perform(step, true).is_err() {
                            break;
                        }
                    }
                });
            }
            action => {
                if let Some(child) = perform(&prepare(action)?, false)? {
                    self.children.push(child);
                }
            }
        }
        Ok(())
    }

    /// `button` came up: let go of whatever its press is holding.
    fn release(&mut self, button: Button) -> Result<()> {
        self.shift.release(button);
        match self.held.remove(&button) {
            Some(held) => held.release(),
            None => Ok(()),
        }
    }
}

/// Input a pressed key is holding down until it is released.
enum Held {
    /// A `hold` shortcut: its key and modifiers.
    Hold(keys::Resolved),
    /// A `switch` shortcut: its modifiers.
    Switch(keys::Modifiers),
}

impl Held {
    fn release(self) -> Result<()> {
        match self {
            Held::Hold(resolved) => {
                keys::press_key(resolved, false)?;
                keys::press_modifiers(resolved.modifiers, false)
            }
            Held::Switch(modifiers) => keys::press_modifiers(modifiers, false),
        }
    }
}

/// An action ready to perform on any thread: shortcuts already resolved.
enum Step {
    Nothing,
    Run(String),
    Open(Vec<String>),
    Tap(keys::Resolved),
    Text(String),
    Media(keys::MediaKey),
    Mouse(keys::MouseButton),
    Delay(Duration),
}

/// Make `action` ready to perform. Page, hold, switch and sequence are
/// handled by [`Session::press`] and never get here.
fn prepare(action: &Action) -> Result<Step> {
    Ok(match action {
        Action::None | Action::Page(_) | Action::Hold(_) | Action::Switch(_) | Action::Shift(_) => {
            Step::Nothing
        }
        Action::Sequence(_) => anyhow::bail!("a sequence cannot contain a sequence"),
        Action::Run(command) => Step::Run(command.clone()),
        Action::Open(target) => Step::Open(open_args(target)),
        Action::Keys(chord) => Step::Tap(Chord::parse(chord)?.resolve()),
        Action::Text(text) => Step::Text(text.clone()),
        Action::Media(name) => Step::Media(keys::MediaKey::parse(name)?),
        Action::Mouse(name) => Step::Mouse(keys::MouseButton::parse(name)?),
        Action::System(name) => match name.as_str() {
            "sleep" => Step::Run("pmset sleepnow".into()),
            "display-sleep" => Step::Run("pmset displaysleepnow".into()),
            _ => Step::Tap(Chord::parse("ctrl+cmd+q")?.resolve()),
        },
        Action::Delay(ms) => Step::Delay(Duration::from_millis(*ms)),
    })
}

/// `open`'s arguments for `target`: an app by name, or a URL or path.
fn open_args(target: &str) -> Vec<String> {
    let target = target.trim();
    let expanded = match (target.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home)
            .join(rest)
            .display()
            .to_string(),
        _ => target.to_string(),
    };
    let is_location = target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with('/')
        || target.starts_with('~')
        || target.starts_with('.');
    if is_location {
        vec![expanded]
    } else {
        vec!["-a".into(), expanded]
    }
}

/// Perform `step`. A command started by `run` or `open` is returned to be
/// reaped later, or waited for when `wait` (inside a sequence, so steps
/// stay in order).
fn perform(step: &Step, wait: bool) -> Result<Option<Child>> {
    let spawn = |command: &mut Command| -> Result<Option<Child>> {
        command.stdin(Stdio::null());
        if wait {
            command.status().context("cannot run a command")?;
            Ok(None)
        } else {
            Ok(Some(command.spawn().context("cannot run a command")?))
        }
    };
    match step {
        Step::Nothing => Ok(None),
        Step::Run(command) => spawn(Command::new("sh").arg("-c").arg(command)),
        Step::Open(args) => spawn(Command::new("/usr/bin/open").args(args)),
        Step::Tap(resolved) => keys::tap(*resolved).map(|()| None),
        Step::Text(text) => keys::type_text(text).map(|()| None),
        Step::Media(key) => keys::send_media(*key).map(|()| None),
        Step::Mouse(button) => keys::click(*button).map(|()| None),
        Step::Delay(delay) => {
            std::thread::sleep(*delay);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::Button;

    fn session(json: &str) -> Session {
        let config: Config = serde_json::from_str(json).unwrap();
        Session::new(Setup::resolve(config, std::path::Path::new(".")).unwrap())
    }

    #[test]
    fn open_takes_names_as_apps_and_the_rest_as_locations() {
        assert_eq!(open_args("Safari"), ["-a", "Safari"]);
        assert_eq!(open_args("System Settings"), ["-a", "System Settings"]);
        assert_eq!(open_args("https://example.com"), ["https://example.com"]);
        assert_eq!(open_args("/Applications"), ["/Applications"]);
        let home = std::env::var("HOME").unwrap();
        assert_eq!(open_args("~/Downloads"), [format!("{home}/Downloads")]);
    }

    #[test]
    fn a_page_action_switches_pages_without_a_device() {
        let mut s = session(
            r#"{ "pages": [
                { "name": "a", "keys": { "1": { "action": { "page": "b" } } } },
                { "name": "b" }
            ] }"#,
        );
        assert_eq!(s.setup().pages[s.page].name, "a");
        s.down(Button::Key(1)).unwrap();
        assert_eq!(s.setup().pages[s.page].name, "b");
    }

    #[test]
    fn toggle_shift_changes_the_face_shown_without_a_device() {
        let mut s = session(
            r#"{ "pages": [{ "name": "a", "keys": {
                "1": { "label": "æ", "action": { "text": "æ" } },
                "8": { "action": { "shift": "toggle" } }
            } }] }"#,
        );
        let now = Instant::now();
        assert_eq!(s.face(0, now).label.as_deref(), Some("æ"));
        s.down(Button::Key(8)).unwrap();
        assert_eq!(s.face(0, now).label.as_deref(), Some("Æ"));
        s.down(Button::Key(8)).unwrap();
        assert_eq!(s.face(0, now).label.as_deref(), Some("æ"));
    }

    #[test]
    fn a_hold_mode_shift_key_releases_when_it_comes_up() {
        let mut s = session(
            r#"{ "pages": [{ "name": "a", "keys": {
                "8": { "action": { "shift": "hold" } }
            } }] }"#,
        );
        s.down(Button::Key(8)).unwrap();
        assert!(s.shift.active());
        s.up(Button::Key(8)).unwrap();
        assert!(!s.shift.active());
    }

    #[test]
    fn reload_keeps_the_same_named_page() {
        let mut s = session(r#"{ "pages": [{ "name": "a" }, { "name": "b" }] }"#);
        s.page = 1;
        let config: Config = serde_json::from_str(
            r#"{ "pages": [{ "name": "z" }, { "name": "a" }, { "name": "b" }] }"#,
        )
        .unwrap();
        s.reload(Setup::resolve(config, std::path::Path::new(".")).unwrap());
        assert_eq!(s.setup().pages[s.page].name, "b");
    }

    #[test]
    fn a_long_press_fires_the_long_action_once_held_past_the_threshold() {
        let mut s = session(
            r#"{ "defaults": { "long_press_ms": 10 }, "pages": [{ "name": "a", "keys": {
                "1": { "action": { "page": "b" }, "long_press": { "page": "c" } }
            } }, { "name": "b" }, { "name": "c" }] }"#,
        );
        s.down(Button::Key(1)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let fired = s.tick();
        assert_eq!(fired.len(), 1);
        assert_eq!(s.setup().pages[s.page].name, "c");
        s.up(Button::Key(1)).unwrap();
    }
}
