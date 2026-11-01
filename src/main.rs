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

use mx_keypad::config::{self, Action, Setup};
use mx_keypad::geometry::DISPLAY;
use mx_keypad::keys::{self, Chord};
use mx_keypad::render::{self, Face, Rgb};
use mx_keypad::{Button, Event, KeyImageJpeg, Keypad};

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
        for event in poll(&mut keypad, &stop)? {
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
}

impl Runner {
    /// Draw the current page. Tiles come from the cache, and `Keypad::draw`
    /// skips keys that already show theirs.
    fn show(&mut self, keypad: &mut Keypad) -> Result<()> {
        for (i, (face, _)) in self.setup.pages[self.page].keys.iter().enumerate() {
            // `Face` holds an f32, so it is keyed by its full Debug form.
            let key = format!("{face:?}");
            let tile = match self.tiles.get(&key) {
                Some(tile) => tile.clone(),
                None => {
                    let tile = render::render_tile(face, DISPLAY.key_size)?;
                    self.tiles.insert(key, tile.clone());
                    tile
                }
            };
            keypad.draw(u8::try_from(i + 1)?, &tile)?;
        }
        Ok(())
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
                // Image files may have changed along with the config.
                self.tiles.clear();
                self.follow_page_buttons(keypad)?;
                if let Some(level) = self.setup.brightness {
                    keypad.set_brightness(level)?;
                }
                eprintln!("reloaded {}", self.path.display());
                self.show(keypad)
            }
            Err(error) => {
                eprintln!("{error:#}; keeping the previous setup");
                Ok(())
            }
        }
    }

    fn act(&mut self, keypad: &mut Keypad, action: &Action) -> Result<()> {
        match action {
            Action::None => {}
            Action::Run(command) => {
                let child = Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .stdin(Stdio::null())
                    .spawn()
                    .with_context(|| format!("cannot run {command:?}"))?;
                self.children.push(child);
            }
            Action::Keys(chord) => keys::send_chord(Chord::parse(chord)?)?,
            Action::Text(text) => keys::type_text(text)?,
            Action::Media(name) => keys::send_media(keys::MediaKey::parse(name)?)?,
            Action::Page(_) => {
                if let Some(to) = self.setup.target(self.page, action) {
                    if to != self.page {
                        self.page = to;
                        self.show(keypad)?;
                    }
                }
            }
        }
        Ok(())
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
        eprintln!(
            "keystroke keys need Accessibility: allow mx-keypad under System Settings → \
             Privacy & Security → Accessibility; they fail until then"
        );
    }
    let mut runner = Runner {
        path: path.to_path_buf(),
        loaded: mtime(path),
        setup,
        page: 0,
        children: Vec::new(),
        tiles: HashMap::new(),
    };
    let mut api = HidApi::new()?;
    let mut waiting = false;
    // Sessions in a row that ended in an error before running `STABLE`.
    let mut failures = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let Some(mut keypad) = Keypad::open(&api)? else {
            if !waiting {
                eprintln!("waiting for the MX Creative Keypad (USB)");
                waiting = true;
            }
            if sleep_unless_stopped(Duration::from_secs(1), &stop) {
                return Ok(());
            }
            api.refresh_devices()?;
            continue;
        };
        if waiting {
            eprintln!("keypad connected");
            waiting = false;
        }
        let started = Instant::now();
        let served = session(&mut runner, &mut keypad, &stop);
        // Blank the keys on the way out, whatever ended the session: images
        // left behind would look live while nothing answers the presses.
        let cleared = keypad.clear();
        let error = match served {
            Ok(()) => return cleared,
            Err(error) => error,
        };
        // Unplugged, or the keypad refused a write. Back off before trying
        // again, longer each time a session fails quickly, so a keypad that
        // keeps refusing is not flooded with retries.
        if started.elapsed() > STABLE {
            failures = 0;
        }
        failures = (failures + 1).min(5);
        let delay = Duration::from_secs(1 << failures);
        eprintln!("{error:#}; reconnecting in {}s", delay.as_secs());
        if sleep_unless_stopped(delay, &stop) {
            return Ok(());
        }
        drop(keypad);
        api.refresh_devices()?;
    }
    Ok(())
}

/// Set up a freshly opened keypad and serve it until `stop` or an error.
fn session(runner: &mut Runner, keypad: &mut Keypad, stop: &AtomicBool) -> Result<()> {
    runner.follow_page_buttons(keypad)?;
    if let Some(level) = runner.setup.brightness {
        keypad.set_brightness(level)?;
    }
    runner.show(keypad)?;
    serve(runner, keypad, stop)
}

/// Act on presses and config changes until `stop` is set.
fn serve(runner: &mut Runner, keypad: &mut Keypad, stop: &AtomicBool) -> Result<()> {
    while !stop.load(Ordering::Relaxed) {
        for event in poll(keypad, stop)? {
            let Event::Down(button) = event else { continue };
            let action = match button {
                Button::Key(n) => runner.setup.pages[runner.page].keys[usize::from(n) - 1]
                    .1
                    .clone(),
                Button::PageLeft => runner.setup.left.clone(),
                Button::PageRight => runner.setup.right.clone(),
            };
            if let Err(error) = runner.act(keypad, &action) {
                eprintln!("{error:#}");
            }
        }
        runner
            .children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        runner.reload(keypad)?;
    }
    Ok(())
}

/// Presses within one tick. A read the signal interrupted is the signal, not
/// a failure: the loop ends and `Drop` hands the page buttons back.
fn poll(keypad: &mut Keypad, stop: &AtomicBool) -> Result<Vec<Event>> {
    match keypad.poll(TICK) {
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
