//! `mx-keypad`: run a JSON keypad setup, or poke the keypad by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hidapi::HidApi;

use mx_keypad::blink;
use mx_keypad::claim;
use mx_keypad::config::{self, Action, Setup};
use mx_keypad::geometry::DISPLAY;
use mx_keypad::keys::{self, Chord};
use mx_keypad::press::{Fire, LongPress};
use mx_keypad::render::{self, Face, Rgb};
use mx_keypad::shift::{self, Shift};
use mx_keypad::{Button, Event, KeyImageJpeg, Keypad};

/// `eprintln!` with the local time in front, for the agent's log.
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("{} {}", crate::timestamp(), format_args!($($arg)*))
    };
}

#[cfg(target_os = "macos")]
mod service;

/// A session that ran this long before failing resets the backoff.
const STABLE: Duration = Duration::from_secs(30);

/// How often `run` wakes to check for a config change or a signal.
const TICK: Duration = Duration::from_millis(200);

#[derive(Parser)]
#[command(
    version,
    about = "Drive the Logitech MX Creative Keypad from a JSON setup"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the setup and act on presses until interrupted. Reloads when the
    /// config file changes.
    Run {
        /// Config file [default: ~/.config/mx-keypad/config.json]
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Validate a config and render every tile, no keypad needed.
    Check {
        /// Config file [default: ~/.config/mx-keypad/config.json]
        #[arg(long)]
        config: Option<PathBuf>,
        /// Write each tile as <page>-<key>.jpg here.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Draw one key.
    Draw {
        /// Key, 1–9, row-major from the top-left.
        key: u8,
        /// Built-in icon name or image path.
        icon: String,
        /// Background colour, #rrggbb.
        #[arg(long, default_value = "#000000")]
        bg: Rgb,
        /// Icon and label colour, #rrggbb.
        #[arg(long, default_value = "#ffffff")]
        fg: Rgb,
        /// Text under the icon.
        #[arg(long)]
        label: Option<String>,
    },
    /// Set the panel brightness, 1–100.
    Brightness {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=100))]
        percent: u8,
    },
    /// Print key and page-button presses until interrupted.
    Events,
    /// Run `run` as a LaunchAgent: at login, restarted if it stops, with its
    /// own Accessibility permission.
    #[cfg(target_os = "macos")]
    Install {
        /// Config file [default: ~/.config/mx-keypad/config.json]
        #[arg(long)]
        config: Option<PathBuf>,
        /// Code-signing identity (SHA-1 or name) [default: the first Apple
        /// Development identity in the keychain]
        #[arg(long)]
        identity: Option<String>,
    },
    /// Stop and remove the LaunchAgent.
    #[cfg(target_os = "macos")]
    Uninstall,
    /// List the built-in icon names.
    Icons,
    /// Show what a shortcut presses on the current keyboard layout.
    Resolve {
        /// A shortcut, such as `cmd+[`.
        chord: String,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Run { config } => run(&config.unwrap_or_else(config::default_path)),
        Cmd::Check { config, out } => {
            check(&config.unwrap_or_else(config::default_path), out.as_deref())
        }
        Cmd::Draw {
            key,
            icon,
            bg,
            fg,
            label,
        } => {
            let face = Face {
                icon: Some(config::icon_source(&icon, Path::new("."))?),
                label,
                background: bg,
                foreground: fg,
                ..Face::default()
            };
            let tile = render::render_tile(&face, DISPLAY.key_size)?;
            let api = HidApi::new()?;
            open(&api)?.draw(key, &tile)
        }
        Cmd::Brightness { percent } => {
            let api = HidApi::new()?;
            open(&api)?.set_brightness(percent)
        }
        Cmd::Events => events(),
        #[cfg(target_os = "macos")]
        Cmd::Install { config, identity } => {
            let path = config.unwrap_or_else(config::default_path);
            Setup::load(&path)?;
            service::install(&path, identity.as_deref())
        }
        #[cfg(target_os = "macos")]
        Cmd::Uninstall => service::uninstall(),
        Cmd::Resolve { chord } => {
            let resolved = Chord::parse(&chord)?.resolve();
            let m = resolved.modifiers;
            let names: Vec<&str> = [
                (m.cmd, "cmd"),
                (m.ctrl, "ctrl"),
                (m.alt, "alt"),
                (m.shift, "shift"),
            ]
            .into_iter()
            .filter_map(|(held, name)| held.then_some(name))
            .collect();
            println!("key code {} with [{}]", resolved.code, names.join(", "));
            Ok(())
        }
        Cmd::Icons => {
            for name in render::builtin_icon_names() {
                println!("{name}");
            }
            Ok(())
        }
    }
}

fn open(api: &HidApi) -> Result<Keypad> {
    Keypad::open(api)?.context("no MX Creative Keypad found (it only works over USB)")
}

/// Set once SIGINT or SIGTERM arrives, so `Drop` hands the page buttons back.
fn interrupted() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    Ok(stop)
}

fn check(path: &Path, out: Option<&Path>) -> Result<()> {
    let setup = Setup::load(path)?;
    if let Some(out) = out {
        std::fs::create_dir_all(out)?;
    }
    for page in &setup.pages {
        for (i, (face, _)) in page.keys.iter().enumerate() {
            let tile = render::render_tile(face, DISPLAY.key_size)
                .with_context(|| format!("page {:?} key {}", page.name, i + 1))?;
            if let Some(out) = out {
                std::fs::write(
                    out.join(format!("{}-{}.jpg", page.name, i + 1)),
                    tile.as_bytes(),
                )?;
            }
            if let Some(spec) = &page.blink[i] {
                let tile = render::render_tile(&spec.alternate(face), DISPLAY.key_size)
                    .with_context(|| format!("page {:?} key {} blink", page.name, i + 1))?;
                if let Some(out) = out {
                    std::fs::write(
                        out.join(format!("{}-{}-blink.jpg", page.name, i + 1)),
                        tile.as_bytes(),
                    )?;
                }
            }
            let shifted = &page.shifted_keys[i].0;
            if shifted != face {
                let tile = render::render_tile(shifted, DISPLAY.key_size)
                    .with_context(|| format!("page {:?} key {} shifted", page.name, i + 1))?;
                if let Some(out) = out {
                    std::fs::write(
                        out.join(format!("{}-{}-shifted.jpg", page.name, i + 1)),
                        tile.as_bytes(),
                    )?;
                }
            }
        }
    }
    println!(
        "{}: {} page(s), every tile renders",
        path.display(),
        setup.pages.len()
    );
    Ok(())
}

fn events() -> Result<()> {
    let stop = interrupted()?;
    let api = HidApi::new()?;
    let mut keypad = open(&api)?;
    keypad.divert_page_buttons(true)?;
    println!("press keys; Ctrl-C to stop");
    while !stop.load(Ordering::Relaxed) {
        for event in poll(&mut keypad, &stop, TICK)? {
            println!("{event:?}");
        }
    }
    Ok(())
}

/// The running setup and what the keypad currently shows.
struct Runner {
    path: PathBuf,
    setup: Setup,
    loaded: Option<SystemTime>,
    page: usize,
    children: Vec<Child>,
    /// Rendered tiles by face, so a page is rendered once per config load.
    tiles: HashMap<String, KeyImageJpeg>,
    /// What each pressed button is holding until it is released.
    held: HashMap<Button, Held>,
    /// Keys with a long-press action, down and not yet decided.
    long: LongPress<Button, Action>,
    /// The shared start every blinking key counts its phase from.
    blink_epoch: Instant,
    /// The keypad's own shift.
    shift: Shift<Button>,
}

impl Runner {
    /// Draw the current page. Tiles come from the cache, and `Keypad::draw`
    /// skips keys that already show theirs. A key that fails does not stop
    /// the others; the first failure is returned once all were tried.
    fn show(&mut self, keypad: &mut Keypad) -> Result<()> {
        let now = Instant::now();
        let mut first_error = None;
        for i in 0..self.setup.pages[self.page].keys.len() {
            let face = self.face(i, now);
            // `Face` holds an f32, so it is keyed by its full Debug form.
            let key = format!("{face:?}");
            let tile = match self.tiles.get(&key) {
                Some(tile) => tile.clone(),
                None => {
                    let tile = render::render_tile(&face, DISPLAY.key_size)?;
                    self.tiles.insert(key, tile.clone());
                    tile
                }
            };
            if let Err(error) = keypad.draw(u8::try_from(i + 1)?, &tile) {
                first_error.get_or_insert(error.context(format!("key {}", i + 1)));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The face key `i` (0-based) of the current page shows at `now`: its
    /// blink face during a blinking key's alternate phase.
    fn face(&self, i: usize, now: Instant) -> Face {
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
    fn next_flip(&self, now: Instant) -> Option<Instant> {
        self.setup.pages[self.page]
            .blink
            .iter()
            .flatten()
            .map(|spec| blink::next_flip(self.blink_epoch, now, spec.interval))
            .min()
    }

    /// Take the page buttons over only when the setup uses them.
    fn follow_page_buttons(&self, keypad: &mut Keypad) -> Result<()> {
        keypad.divert_page_buttons(self.setup.needs_page_buttons())
    }

    /// Reload when the file changed; keep the old setup if the new one is
    /// broken, so a half-saved edit does not blank the keypad.
    fn reload(&mut self, keypad: &mut Keypad) -> Result<()> {
        let modified = mtime(&self.path);
        if modified == self.loaded {
            return Ok(());
        }
        self.loaded = modified;
        match Setup::load(&self.path) {
            Ok(setup) => {
                let name = &self.setup.pages[self.page].name;
                self.page = setup
                    .pages
                    .iter()
                    .position(|p| &p.name == name)
                    .unwrap_or(0);
                self.setup = setup;
                self.long.set_threshold(self.setup.long_press);
                // Image files may have changed along with the config.
                self.tiles.clear();
                self.follow_page_buttons(keypad)?;
                if let Some(level) = self.setup.brightness {
                    keypad.set_brightness(level)?;
                }
                log!("reloaded {}", self.path.display());
                self.show(keypad)
            }
            Err(error) => {
                log!("{error:#}; keeping the previous setup");
                Ok(())
            }
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

    /// A key went down: act now, or start timing it when it has a long-press
    /// action.
    fn down(&mut self, keypad: &mut Keypad, button: Button) -> Result<()> {
        let action = self.action(button);
        match self.long_action(button) {
            Some(long) => {
                self.long.down(button, Instant::now(), action, long);
                Ok(())
            }
            None => self.press(keypad, button, &action),
        }
    }

    /// A key came up: decide a timed press, and let go of anything held.
    fn up(&mut self, keypad: &mut Keypad, button: Button) -> Result<()> {
        if let Some(Fire::Short(action) | Fire::Long(action)) = self.long.up(button, Instant::now())
        {
            self.press(keypad, button, &action)?;
        }
        self.release(keypad, button)
    }

    /// Fire the long-press actions of keys held past the threshold.
    fn tick(&mut self, keypad: &mut Keypad) -> Result<()> {
        for (button, action) in self.long.tick(Instant::now()) {
            self.press(keypad, button, &action)?;
        }
        Ok(())
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
    fn press(&mut self, keypad: &mut Keypad, button: Button, action: &Action) -> Result<()> {
        if let Action::Shift(mode) = action {
            self.shift.press(button, shift::Mode::parse(mode)?);
            return self.show(keypad);
        }
        let result = self.perform_action(keypad, button, action);
        // A `once` shift is spent by the next grid key, whatever it did.
        if matches!(button, Button::Key(_)) && self.shift.consume() {
            self.show(keypad)?;
        }
        result
    }

    /// Do what `action` says, for `button`.
    fn perform_action(
        &mut self,
        keypad: &mut Keypad,
        button: Button,
        action: &Action,
    ) -> Result<()> {
        match action {
            Action::Page(_) => {
                if let Some(to) = self.setup.target(self.page, action) {
                    if to != self.page {
                        self.page = to;
                        self.show(keypad)?;
                    }
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
                // Resolve shortcuts here, on the main thread, where the
                // keyboard layout API must be called; delays then run off it.
                let steps = steps.iter().map(prepare).collect::<Result<Vec<_>>>()?;
                std::thread::spawn(move || {
                    for step in &steps {
                        if let Err(error) = perform(step, true) {
                            log!("sequence stopped: {error:#}");
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
    fn release(&mut self, keypad: &mut Keypad, button: Button) -> Result<()> {
        if self.shift.release(button) {
            self.show(keypad)?;
        }
        match self.held.remove(&button) {
            Some(held) => held.release(),
            None => Ok(()),
        }
    }

    /// Let go of everything held, when the session ends.
    fn release_all(&mut self) {
        self.long.clear();
        self.shift.clear();
        for (_, held) in self.held.drain() {
            if let Err(error) = held.release() {
                log!("{error:#}");
            }
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
/// handled by [`Runner::press`] and never get here.
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
        (Some(rest), Some(home)) => Path::new(&home).join(rest).display().to_string(),
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

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn run(path: &Path) -> Result<()> {
    let stop = interrupted()?;
    #[cfg(target_os = "macos")]
    service::trim_log();
    let setup = Setup::load(path)?;
    if setup.sends_keys() && !keys::trusted(true) {
        log!(
            "keystroke keys need Accessibility: allow mx-keypad under System Settings → \
             Privacy & Security → Accessibility; they fail until then"
        );
    }
    let setup_long_press = setup.long_press;
    let mut runner = Runner {
        path: path.to_path_buf(),
        loaded: mtime(path),
        setup,
        page: 0,
        children: Vec::new(),
        tiles: HashMap::new(),
        held: HashMap::new(),
        long: LongPress::new(setup_long_press),
        blink_epoch: Instant::now(),
        shift: Shift::default(),
    };
    let mut api = HidApi::new()?;
    let mut waiting = false;
    // Sessions in a row that ended in an error before running `STABLE`.
    let mut failures = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if let Some((pid, owner)) = claim::foreign() {
            log!("yielding to {owner} (pid {pid})");
            while claim::foreign().is_some() {
                if sleep_unless_stopped(Duration::from_secs(1), &stop) {
                    return Ok(());
                }
            }
            api.refresh_devices()?;
            continue;
        }
        let Some(mut keypad) = Keypad::open(&api)? else {
            if !waiting {
                log!("waiting for the MX Creative Keypad (USB)");
                waiting = true;
            }
            if sleep_unless_stopped(Duration::from_secs(1), &stop) {
                return Ok(());
            }
            api.refresh_devices()?;
            continue;
        };
        if waiting {
            log!("keypad connected");
            waiting = false;
        }
        let started = Instant::now();
        let served = session(&mut runner, &mut keypad, &stop);
        runner.release_all();
        let error = match served {
            // Blank the keys on the way out: images left behind would look
            // live while nothing answers the presses.
            Ok(Outcome::Stopped) => return keypad.clear(),
            Ok(Outcome::Yielding(pid, owner)) => {
                log!("yielding to {owner} (pid {pid})");
                // The claimant draws next: drop without clearing, and wait
                // for the claim to go away before reopening and redrawing.
                drop(keypad);
                runner.tiles.clear();
                while claim::foreign().is_some() {
                    if sleep_unless_stopped(Duration::from_secs(1), &stop) {
                        return Ok(());
                    }
                }
                api.refresh_devices()?;
                continue;
            }
            Err(error) => {
                let _ = keypad.clear();
                error
            }
        };
        // Unplugged, or the keypad refused a write. Back off before trying
        // again, longer each time a session fails quickly, so a keypad that
        // keeps refusing is not flooded with retries.
        if started.elapsed() > STABLE {
            failures = 0;
        }
        failures = (failures + 1).min(5);
        let delay = Duration::from_secs(1 << failures);
        log!("{error:#}; reconnecting in {}s", delay.as_secs());
        if sleep_unless_stopped(delay, &stop) {
            return Ok(());
        }
        api.refresh_devices()?;
    }
    Ok(())
}

/// What ended a serving session.
enum Outcome {
    /// The stop signal fired.
    Stopped,
    /// A foreign claim appeared: `run` steps aside without clearing the
    /// keypad, since the claimant draws next.
    Yielding(u32, String),
}

/// Set up a freshly opened keypad and serve it until `stop`, an error, or a
/// foreign claim.
fn session(runner: &mut Runner, keypad: &mut Keypad, stop: &AtomicBool) -> Result<Outcome> {
    runner.follow_page_buttons(keypad)?;
    if let Some(level) = runner.setup.brightness {
        keypad.set_brightness(level)?;
    }
    runner.show(keypad)?;
    serve(runner, keypad, stop)
}

/// Act on presses and config changes until `stop` is set or a foreign claim
/// on the keypad appears.
fn serve(runner: &mut Runner, keypad: &mut Keypad, stop: &AtomicBool) -> Result<Outcome> {
    while !stop.load(Ordering::Relaxed) {
        if let Some((pid, owner)) = claim::foreign() {
            return Ok(Outcome::Yielding(pid, owner));
        }
        // Wake in time for a long press to fire while the key is held, and
        // for the next blink.
        let now = Instant::now();
        let flip = runner.next_flip(now);
        let wake = [runner.long.next_deadline(), flip]
            .into_iter()
            .flatten()
            .min();
        let timeout = wake.map_or(TICK, |at| at.saturating_duration_since(now).min(TICK));
        for event in poll(keypad, stop, timeout)? {
            let result = match event {
                Event::Down(button) => runner.down(keypad, button),
                Event::Up(button) => runner.up(keypad, button),
            };
            if let Err(error) = result {
                log!("{error:#}");
            }
        }
        if let Err(error) = runner.tick(keypad) {
            log!("{error:#}");
        }
        if flip.is_some_and(|at| Instant::now() >= at) {
            if let Err(error) = runner.show(keypad) {
                log!("blink: {error:#}");
            }
        }
        runner
            .children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        runner.reload(keypad)?;
    }
    Ok(Outcome::Stopped)
}

/// Presses within one tick. A read the signal interrupted is the signal, not
/// a failure: the loop ends and `Drop` hands the page buttons back.
fn poll(keypad: &mut Keypad, stop: &AtomicBool, timeout: Duration) -> Result<Vec<Event>> {
    match keypad.poll(timeout) {
        Err(_) if stop.load(Ordering::Relaxed) => Ok(Vec::new()),
        result => result,
    }
}

/// Sleep for `delay` in ticks; `true` when `stop` was set meanwhile.
fn sleep_unless_stopped(delay: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(TICK);
    }
    stop.load(Ordering::Relaxed)
}

/// The local time as `YYYY-MM-DD HH:MM:SS`.
fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = libc::time_t::try_from(now).unwrap_or_default();
    // SAFETY: `localtime_r` writes into the `tm` we own and reads `secs`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return format!("@{now}");
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::{open_args, timestamp};

    #[test]
    fn timestamps_read_as_local_date_and_time() {
        let stamp = timestamp();
        let shape: String = stamp
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect();
        assert_eq!(shape, "9999-99-99 99:99:99", "{stamp}");
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
}
