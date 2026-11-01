//! `mx-keypad`: run a JSON keypad setup, or poke the keypad by hand.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hidapi::HidApi;

use mx_keypad::config::{self, Action, Setup};
use mx_keypad::geometry::DISPLAY;
use mx_keypad::render::{self, Face, Rgb};
use mx_keypad::{Button, Event, Keypad};

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
    /// List the built-in icon names.
    Icons,
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
}

impl Runner {
    fn show(&self, keypad: &mut Keypad) -> Result<()> {
        for (i, (face, _)) in self.setup.pages[self.page].keys.iter().enumerate() {
            let tile = render::render_tile(face, DISPLAY.key_size)?;
            keypad.draw(u8::try_from(i + 1)?, &tile)?;
        }
        Ok(())
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
    let setup = Setup::load(path)?;
    let api = HidApi::new()?;
    let mut keypad = open(&api)?;
    keypad.divert_page_buttons(true)?;
    let mut runner = Runner {
        path: path.to_path_buf(),
        loaded: mtime(path),
        setup,
        page: 0,
        children: Vec::new(),
    };
    if let Some(level) = runner.setup.brightness {
        keypad.set_brightness(level)?;
    }
    runner.show(&mut keypad)?;

    while !stop.load(Ordering::Relaxed) {
        for event in poll(&mut keypad, &stop)? {
            let Event::Down(button) = event else { continue };
            let action = match button {
                Button::Key(n) => runner.setup.pages[runner.page].keys[usize::from(n) - 1]
                    .1
                    .clone(),
                Button::PageLeft => runner.setup.left.clone(),
                Button::PageRight => runner.setup.right.clone(),
            };
            if let Err(error) = runner.act(&mut keypad, &action) {
                eprintln!("{error:#}");
            }
        }
        runner
            .children
            .retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        runner.reload(&mut keypad)?;
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
