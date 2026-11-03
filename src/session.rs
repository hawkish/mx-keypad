//! A running [`Setup`](crate::config::Setup): what each key shows and does
//! right now, held keys, shift, long press, blink, and the child processes a
//! `run` or `open` action started. This is the engine behind `mx-keypad
//! run`, exposed so a program driving its own [`Keypad`] can run the same
//! kind of config without reimplementing action dispatch.
//!
//! It also holds *remote pages*: pages another program adds over the agent's
//! socket ([`crate::remote`]), drawn after the config's pages, whose presses
//! are queued for that program instead of acted on here.
//!
//! A `Session` never touches a [`Keypad`] or a terminal: it decides what to
//! draw ([`Session::face`]) and performs an action's side effects
//! (keystrokes, a spawned process, a changed page) when told a button went
//! down or up. Drawing, reading presses, and diverting the page buttons stay
//! the caller's job — that's what keeps this type testable without hardware.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::blink;
use crate::config::{Action, DialpadSetup, ResolvedPage, Setup, Turn};
use crate::dialpad::Wheel;
use crate::keys::{self, Chord};
use crate::press::{Fire, LongPress};
use crate::remote::ConnId;
use crate::render::Face;
use crate::shift::{self, Shift};
use crate::{Button, Event};

/// A page a socket client added, resolved like a config page, and which
/// connection owns it.
struct RemotePage {
    owner: ConnId,
    page: ResolvedPage,
}

/// A press to forward to the connection that owns a remote page.
#[derive(Debug, PartialEq, Eq)]
pub struct RemoteEvent {
    pub owner: ConnId,
    pub page: String,
    pub event: Event,
}

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
    /// Client pages, in the order they were added.
    remote: Vec<RemotePage>,
    /// Grid keys that went down on a remote page, and which one, so the
    /// release goes there too.
    remote_held: HashMap<Button, (ConnId, String)>,
    /// Presses on remote pages, waiting for the caller to forward them.
    remote_events: Vec<RemoteEvent>,
    /// The remote page that was on screen when its client went away, and
    /// when: a client that reconnects soon and adds it again gets it back on
    /// screen, unless someone turned pages in between.
    returning: Option<(String, Instant)>,
    /// Turn not yet acted on: the dial's, then the roller's.
    turned: [i32; 2],
    /// Dialpad bindings a client sent, which win over the config's while
    /// that client is connected.
    remote_dialpad: Option<(ConnId, DialpadSetup)>,
}

/// How long after a client drops its visible page that page still returns
/// to the screen when added again — long enough for a reconnect's backoff.
const RETURN_WINDOW: Duration = Duration::from_secs(60);

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
            remote: Vec::new(),
            remote_held: HashMap::new(),
            remote_events: Vec::new(),
            returning: None,
            turned: [0; 2],
            remote_dialpad: None,
        }
    }

    /// Replace the setup — after a config file reload, say — trying to stay
    /// on a page of the same name rather than jumping back to the first.
    /// Held keys and shift are left as they are. A config that takes a remote
    /// page's name is ignored; call [`Session::check_reload`] for the error.
    pub fn reload(&mut self, setup: Setup) {
        if self.check_reload(&setup).is_err() {
            return;
        }
        let name = self.page_name().to_owned();
        self.long.set_threshold(setup.long_press);
        self.setup = setup;
        self.page = self.page_index(&name).unwrap_or(self.visible().start);
    }

    /// Reject a config reload that would collide with a connected client's page.
    pub fn check_reload(&self, setup: &Setup) -> Result<()> {
        for page in &setup.pages {
            if self.remote.iter().any(|p| p.page.name == page.name) {
                bail!("page {:?} belongs to a remote client", page.name);
            }
        }
        Ok(())
    }

    /// The current page's name, for either kind of page.
    pub fn page_name(&self) -> &str {
        &self.current().name
    }

    /// The pages the page buttons go through: the clients' pages while any
    /// client has one — they stand in for the config's, which come back when
    /// the last client page goes — and the config's otherwise.
    fn visible(&self) -> std::ops::Range<usize> {
        let local = self.setup.pages.len();
        if self.remote.is_empty() {
            0..local
        } else {
            local..local + self.remote.len()
        }
    }

    /// The page on screen: a config page, or a client's after them.
    fn current(&self) -> &ResolvedPage {
        match self.setup.pages.get(self.page) {
            Some(page) => page,
            None => &self.remote[self.page - self.setup.pages.len()].page,
        }
    }

    /// The client owning the page on screen, if it is a client's.
    fn current_owner(&self) -> Option<ConnId> {
        let at = self.page.checked_sub(self.setup.pages.len())?;
        self.remote.get(at).map(|p| p.owner)
    }

    fn page_index(&self, name: &str) -> Option<usize> {
        self.setup
            .pages
            .iter()
            .map(|p| p.name.as_str())
            .chain(self.remote.iter().map(|p| p.page.name.as_str()))
            .position(|n| n == name)
    }

    /// Append a client's page, or replace one owned by the same connection.
    pub fn add_remote(&mut self, owner: ConnId, page: ResolvedPage) -> Result<()> {
        let name = page.name.clone();
        if self.setup.pages.iter().any(|p| p.name == name) {
            bail!("page {name:?} belongs to the config");
        }
        if let Some(existing) = self.remote.iter_mut().find(|p| p.page.name == name) {
            if existing.owner != owner {
                bail!("page {name:?} belongs to another client");
            }
            existing.page = page;
        } else {
            // The first client page replaces the config's pages on screen.
            let first = self.remote.is_empty();
            self.remote.push(RemotePage { owner, page });
            if first {
                self.page = self.setup.pages.len();
            }
            if let Some((returning, at)) = self.returning.take() {
                if returning == name && at.elapsed() < RETURN_WINDOW {
                    self.page = self.setup.pages.len() + self.remote.len() - 1;
                } else {
                    self.returning = Some((returning, at));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn owned_index(&self, owner: ConnId, name: &str) -> Result<usize> {
        self.remote
            .iter()
            .position(|p| p.owner == owner && p.page.name == name)
            .with_context(|| format!("no owned page named {name:?}"))
    }

    /// Copy keys `slots` (0–8) from `from` onto the owned page of the same
    /// name, leaving its other keys as they are.
    pub fn update_remote(
        &mut self,
        owner: ConnId,
        from: &ResolvedPage,
        slots: &[usize],
    ) -> Result<()> {
        let at = self.owned_index(owner, &from.name)?;
        let page = &mut self.remote[at].page;
        for &i in slots {
            if i >= 9 {
                bail!("key index must be 0–8");
            }
            page.keys[i] = from.keys[i].clone();
            page.long[i].clone_from(&from.long[i]);
            page.blink[i].clone_from(&from.blink[i]);
            page.shifted_keys[i] = from.shifted_keys[i].clone();
            page.shifted_long[i].clone_from(&from.shifted_long[i]);
        }
        Ok(())
    }

    /// Turn to an owned remote page.
    pub fn show_remote(&mut self, owner: ConnId, name: &str) -> Result<()> {
        self.page = self.setup.pages.len() + self.owned_index(owner, name)?;
        Ok(())
    }

    /// Remove an owned page, returning to page zero if it was visible.
    pub fn remove_remote(&mut self, owner: ConnId, name: &str) -> Result<()> {
        let at = self.owned_index(owner, name)?;
        let current = self.page_name().to_owned();
        self.remote.remove(at);
        self.page = self.page_index(&current).unwrap_or(self.visible().start);
        Ok(())
    }

    /// Remove all pages belonging to a disconnected client.
    pub fn drop_owner(&mut self, owner: ConnId) {
        let current = self.page_name().to_owned();
        if self
            .remote
            .iter()
            .any(|p| p.owner == owner && p.page.name == current)
        {
            self.returning = Some((current.clone(), Instant::now()));
        }
        self.remote.retain(|p| p.owner != owner);
        self.remote_held.retain(|_, (id, _)| *id != owner);
        self.remote_events.retain(|e| e.owner != owner);
        self.set_remote_dialpad(owner, None);
        self.page = self.page_index(&current).unwrap_or(self.visible().start);
    }

    /// Drain the remote press/release events awaiting forwarding.
    pub fn take_remote_events(&mut self) -> Vec<RemoteEvent> {
        std::mem::take(&mut self.remote_events)
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
        let page = self.current();
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
        self.current()
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
        !self.remote.is_empty() || self.setup.needs_page_buttons()
    }

    /// The Dialpad bindings in force: a client's, else the config's. `None`
    /// when neither binds it, and it should keep its firmware behaviour.
    #[must_use]
    pub fn dialpad(&self) -> Option<&DialpadSetup> {
        self.remote_dialpad
            .as_ref()
            .map(|(_, dialpad)| dialpad)
            .or(self.setup.dialpad.as_ref())
    }

    /// Bind the Dialpad to `dialpad` for client `owner`, replacing any
    /// client's bindings, or drop `owner`'s with `None`.
    pub fn set_remote_dialpad(&mut self, owner: ConnId, dialpad: Option<DialpadSetup>) {
        match dialpad {
            Some(dialpad) => self.remote_dialpad = Some((owner, dialpad)),
            None if self
                .remote_dialpad
                .as_ref()
                .is_some_and(|(o, _)| *o == owner) =>
            {
                self.remote_dialpad = None;
            }
            None => return,
        }
        self.turned = [0; 2];
    }

    /// The configured panel brightness, if the config sets one. Apply this
    /// once per connection — the brightness is global hardware state, not
    /// per-session.
    #[must_use]
    pub fn brightness(&self) -> Option<u8> {
        self.setup.brightness
    }

    /// `button` went down: act now, or start timing it when it has a
    /// long-press action. A grid key on a client's page with neither is
    /// queued for that client instead (see [`Session::take_remote_events`]).
    pub fn down(&mut self, button: Button) -> Result<()> {
        if self.remote_held.contains_key(&button) {
            return Ok(());
        }
        let action = self.action(button);
        let long = self.long_action(button);
        if let (Some(owner), Button::Key(_), Action::None, None) =
            (self.current_owner(), button, &action, &long)
        {
            let page = self.page_name().to_owned();
            self.remote_held.insert(button, (owner, page.clone()));
            self.remote_events.push(RemoteEvent {
                owner,
                page,
                event: Event::Down(button),
            });
            return Ok(());
        }
        match long {
            Some(long) => {
                self.long.down(button, Instant::now(), action, long);
                Ok(())
            }
            None => self.press(button, &action),
        }
    }

    /// `button` came up: decide a timed press, and let go of anything held.
    pub fn up(&mut self, button: Button) -> Result<()> {
        if let Some((owner, page)) = self.remote_held.remove(&button) {
            self.remote_events.push(RemoteEvent {
                owner,
                page,
                event: Event::Up(button),
            });
            return Ok(());
        }
        if let Some(Fire::Short(action) | Fire::Long(action)) = self.long.up(button, Instant::now())
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
        for (button, (owner, page)) in self.remote_held.drain() {
            self.remote_events.push(RemoteEvent {
                owner,
                page,
                event: Event::Up(button),
            });
        }
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
                let page = self.current();
                let long = if self.shift.active() {
                    &page.shifted_long
                } else {
                    &page.long
                };
                long[usize::from(n) - 1].clone()
            }
            Button::PageLeft | Button::PageRight | Button::Dial(_) => None,
        }
    }

    /// The action `button` has on the current page.
    fn action(&self, button: Button) -> Action {
        match button {
            Button::Key(n) => {
                let page = self.current();
                let keys = if self.shift.active() {
                    &page.shifted_keys
                } else {
                    &page.keys
                };
                keys[usize::from(n) - 1].1.clone()
            }
            Button::PageLeft => self.setup.left.clone(),
            Button::PageRight => self.setup.right.clone(),
            Button::Dial(n) => self
                .dialpad()
                .and_then(|dialpad| dialpad.buttons.get(usize::from(n).checked_sub(1)?))
                .cloned()
                .unwrap_or_default(),
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

    /// The Dialpad's `wheel` turned by `delta`: act once for every whole
    /// `step` of turn, carrying the rest to the next turn. Turning back
    /// drops what was carried, so reversing acts at once.
    pub fn turn(&mut self, wheel: Wheel, delta: i32) -> Result<()> {
        let Some(dialpad) = self.dialpad() else {
            return Ok(());
        };
        let (slot, turn) = match wheel {
            Wheel::Dial => (0, dialpad.dial.clone()),
            Wheel::Roller => (1, dialpad.roller.clone()),
        };
        let Turn { minus, plus, step } = turn;
        let step = i32::try_from(step).unwrap_or(i32::MAX);
        let turned = &mut self.turned[slot];
        if turned.signum() * delta.signum() < 0 {
            *turned = 0;
        }
        *turned += delta;
        let times = *turned / step;
        *turned %= step;
        let action = if times > 0 { plus } else { minus };
        for _ in 0..times.unsigned_abs() {
            self.act(&action)?;
        }
        Ok(())
    }

    /// Do what `action` says, for `button`.
    fn perform_action(&mut self, button: Button, action: &Action) -> Result<()> {
        match action {
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
            action => self.act(action)?,
        }
        Ok(())
    }

    /// Do what an `action` that holds nothing says.
    fn act(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::Page(name) => {
                self.returning = None;
                let visible = self.visible();
                let count = visible.len();
                let at = self.page.saturating_sub(visible.start) % count;
                let to = match name.as_str() {
                    "next" => Some(visible.start + (at + 1) % count),
                    "prev" => Some(visible.start + (at + count - 1) % count),
                    name => self.page_index(name),
                };
                if let Some(to) = to {
                    self.page = to;
                }
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
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest).display().to_string(),
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
    fn turns_act_once_a_step_and_reversing_starts_over() {
        let mut s = session(
            r#"{ "pages": [{ "name": "a" }, { "name": "b" }, { "name": "c" }], "dialpad": {
                "dial": { "left": { "page": "prev" }, "right": { "page": "next" }, "step": 3 }
            } }"#,
        );
        let name = |s: &Session| s.page_name().to_owned();
        s.turn(Wheel::Dial, 2).unwrap();
        assert_eq!(name(&s), "a");
        s.turn(Wheel::Dial, 1).unwrap();
        assert_eq!(name(&s), "b");
        // A fast spin acts once per step in one report.
        s.turn(Wheel::Dial, 7).unwrap();
        assert_eq!(name(&s), "a");
        // One step of the carried turn is left; turning back drops it.
        s.turn(Wheel::Dial, -3).unwrap();
        assert_eq!(name(&s), "c");
        // The roller has nothing bound.
        s.turn(Wheel::Roller, 5).unwrap();
        assert_eq!(name(&s), "c");
    }

    #[test]
    fn dialpad_buttons_act_like_page_buttons() {
        let mut s = session(
            r#"{ "pages": [{ "name": "a" }, { "name": "b" }], "dialpad": {
                "buttons": { "4": { "page": "b" } }
            } }"#,
        );
        s.down(Button::Dial(1)).unwrap();
        s.up(Button::Dial(1)).unwrap();
        assert_eq!(s.page_name(), "a");
        s.down(Button::Dial(4)).unwrap();
        s.up(Button::Dial(4)).unwrap();
        assert_eq!(s.page_name(), "b");
        // A config without a dialpad section ignores them.
        let mut plain = session(r#"{ "pages": [{ "name": "a" }] }"#);
        plain.down(Button::Dial(2)).unwrap();
        plain.turn(Wheel::Dial, 90).unwrap();
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

    /// A client page called `name` with `keys` in config JSON.
    fn remote(name: &str, keys: &str) -> ResolvedPage {
        let keys = serde_json::from_str(keys).unwrap();
        crate::config::resolve_remote_page(name, &keys).unwrap()
    }

    fn blank(name: &str) -> ResolvedPage {
        remote(name, "{}")
    }

    #[test]
    fn remote_pages_cycle_in_insertion_order_and_keep_the_current_name_on_removal() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        assert!(!s.needs_page_buttons());
        s.add_remote(1, blank("z")).unwrap();
        // The first client page replaces the config's on screen.
        assert_eq!(s.page_name(), "z");
        s.add_remote(2, blank("b")).unwrap();
        assert_eq!(s.page_name(), "z");
        assert!(s.needs_page_buttons());
        for name in ["b", "z", "b"] {
            s.down(Button::PageRight).unwrap();
            assert_eq!(s.page_name(), name);
        }
        s.remove_remote(1, "z").unwrap();
        assert_eq!(s.page_name(), "b");
        s.down(Button::PageLeft).unwrap();
        assert_eq!(s.page_name(), "b");
        s.add_remote(1, blank("z")).unwrap();
        s.show_remote(1, "z").unwrap();
        s.remove_remote(1, "z").unwrap();
        // The page on screen went: the first client page left shows.
        assert_eq!(s.page_name(), "b");
        s.drop_owner(2);
        // The config's pages come back with the last client page gone.
        assert_eq!(s.page_name(), "a");
        assert!(!s.needs_page_buttons());
    }

    #[test]
    fn remote_up_follows_down_across_page_turns_and_removal() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        s.add_remote(7, blank("agents")).unwrap();
        s.show_remote(7, "agents").unwrap();
        s.down(Button::Key(3)).unwrap();
        s.down(Button::Key(3)).unwrap(); // repeated down is not another press
        s.remove_remote(7, "agents").unwrap();
        s.up(Button::Key(3)).unwrap();
        assert_eq!(
            s.take_remote_events(),
            vec![
                RemoteEvent {
                    owner: 7,
                    page: "agents".into(),
                    event: Event::Down(Button::Key(3))
                },
                RemoteEvent {
                    owner: 7,
                    page: "agents".into(),
                    event: Event::Up(Button::Key(3))
                },
            ]
        );
        s.add_remote(7, blank("agents")).unwrap();
        s.show_remote(7, "agents").unwrap();
        s.down(Button::Key(1)).unwrap();
        s.take_remote_events();
        s.down(Button::PageLeft).unwrap();
        s.release_all();
        assert_eq!(s.take_remote_events()[0].event, Event::Up(Button::Key(1)));
    }

    #[test]
    fn a_client_page_runs_its_actions_here_and_forwards_only_keys_without_one() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        s.add_remote(
            1,
            remote(
                "keys",
                r#"{"1":{"action":{"page":"next"}},"2":{"label":"agent"},"8":{"action":{"shift":"hold"}}}"#,
            ),
        )
        .unwrap();
        s.show_remote(1, "keys").unwrap();
        s.down(Button::Key(2)).unwrap();
        s.up(Button::Key(2)).unwrap();
        assert_eq!(s.take_remote_events().len(), 2);

        // Shift, from a key on the client's own page, applies to that page.
        s.down(Button::Key(8)).unwrap();
        assert!(s.shift.active());
        s.up(Button::Key(8)).unwrap();
        assert!(s.take_remote_events().is_empty());

        // A page action runs here, turning between the client's pages.
        s.add_remote(1, blank("agents")).unwrap();
        s.down(Button::Key(1)).unwrap();
        assert!(s.take_remote_events().is_empty());
        assert_eq!(s.page_name(), "agents");
    }

    #[test]
    fn a_client_page_may_only_name_next_or_prev() {
        let keys = serde_json::from_str(r#"{"1":{"action":{"page":"a"}}}"#).unwrap();
        assert!(crate::config::resolve_remote_page("keys", &keys).is_err());
        let keys = serde_json::from_str(r#"{"1":{"icon":"icons/x.svg"}}"#).unwrap();
        assert!(crate::config::resolve_remote_page("keys", &keys).is_err());
    }

    #[test]
    fn update_remote_replaces_only_the_named_keys() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        s.add_remote(
            1,
            remote("r", r#"{"1":{"label":"one"},"2":{"label":"two"}}"#),
        )
        .unwrap();
        s.show_remote(1, "r").unwrap();
        s.update_remote(1, &remote("r", r#"{"2":{"label":"TWO"}}"#), &[1])
            .unwrap();
        let now = Instant::now();
        assert_eq!(s.face(0, now).label.as_deref(), Some("one"));
        assert_eq!(s.face(1, now).label.as_deref(), Some("TWO"));
        assert!(s.update_remote(2, &blank("r"), &[0]).is_err());
        assert!(s.update_remote(1, &blank("r"), &[9]).is_err());
    }

    #[test]
    fn remote_names_and_operations_are_owned_and_reload_preserves_pages() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        assert!(s.add_remote(1, blank("a")).is_err());
        s.add_remote(1, blank("remote")).unwrap();
        assert!(s.add_remote(2, blank("remote")).is_err());
        assert!(s.show_remote(2, "remote").is_err());
        assert!(s.remove_remote(2, "remote").is_err());
        s.add_remote(1, remote("remote", r#"{"5":{"label":"updated"}}"#))
            .unwrap();
        s.show_remote(1, "remote").unwrap();
        let setup = session(r#"{"pages":[{"name":"z"},{"name":"a"}]}"#).setup;
        s.reload(setup);
        assert_eq!(s.page_name(), "remote");
        assert_eq!(s.face(4, Instant::now()).label.as_deref(), Some("updated"));
        let collision = session(r#"{"pages":[{"name":"remote"}]}"#).setup;
        assert!(s.check_reload(&collision).is_err());
        s.reload(collision);
        assert_eq!(s.setup.pages[0].name, "z");
        s.drop_owner(1);
        assert_eq!(s.page_name(), "z");
    }

    #[test]
    fn a_reconnecting_client_gets_its_visible_page_back() {
        let mut s = session(r#"{"pages":[{"name":"a"}]}"#);
        s.add_remote(1, blank("keys")).unwrap();
        s.add_remote(1, blank("agents")).unwrap();
        s.show_remote(1, "agents").unwrap();
        s.drop_owner(1);
        assert_eq!(s.page_name(), "a");
        s.add_remote(2, blank("keys")).unwrap();
        s.add_remote(2, blank("agents")).unwrap();
        assert_eq!(s.page_name(), "agents");

        // Turning pages in between means the user has moved on: the
        // client's first page shows, as for any client arriving.
        s.drop_owner(2);
        s.down(Button::PageRight).unwrap();
        s.add_remote(3, blank("keys")).unwrap();
        s.add_remote(3, blank("agents")).unwrap();
        assert_eq!(s.page_name(), "keys");
    }
}
