//! The JSON setup: pages of keys, each with a face and an action.
//!
//! ```json
//! {
//!   "brightness": 70,
//!   "defaults": { "background": "#000000", "foreground": "#ffffff" },
//!   "pages": [
//!     { "name": "main", "keys": {
//!         "1": { "icon": "square-terminal", "label": "Shell", "action": { "run": "open -a Terminal" } },
//!         "9": { "icon": "chevron-right", "action": { "page": "next" } } } }
//!   ],
//!   "page_buttons": { "left": { "page": "prev" }, "right": { "page": "next" } }
//! }
//! ```
//!
//! Keys are `"1"`–`"9"`, row-major from the top-left. An `icon` is a built-in
//! name (`mx-keypad icons`) or a path to an `.svg`, `.png` or `.jpg`, relative
//! to the config file.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::render::{self, Face, IconSource, Rgb};

/// A whole setup, as read from disk.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Panel brightness, 1–100. Left alone when absent.
    #[serde(default)]
    pub brightness: Option<u8>,
    /// What a key's face falls back to.
    #[serde(default)]
    pub defaults: Defaults,
    /// The pages, the first shown at start.
    pub pages: Vec<Page>,
    /// What the two page buttons do.
    #[serde(default)]
    pub page_buttons: PageButtons,
}

/// Face settings every key inherits.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Tile colour behind the icon.
    #[serde(default = "black")]
    pub background: Rgb,
    /// Icon and label colour.
    #[serde(default = "white")]
    pub foreground: Rgb,
    /// Share of the tile's side the icon spans.
    #[serde(default = "glyph_scale")]
    pub glyph_scale: f32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            background: black(),
            foreground: white(),
            glyph_scale: glyph_scale(),
        }
    }
}

fn black() -> Rgb {
    Rgb(0, 0, 0)
}
fn white() -> Rgb {
    Rgb(0xff, 0xff, 0xff)
}
fn glyph_scale() -> f32 {
    0.55
}

/// One screenful of keys.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    /// Unique name, which a `page` action can jump to.
    pub name: String,
    /// Keys by index, `"1"`–`"9"`. Missing keys are blank and do nothing.
    #[serde(default)]
    pub keys: BTreeMap<String, Key>,
}

/// One key's face and action.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    /// Built-in icon name or image path.
    #[serde(default)]
    pub icon: Option<String>,
    /// Text under the icon.
    #[serde(default)]
    pub label: Option<String>,
    /// Overrides [`Defaults::background`].
    #[serde(default)]
    pub background: Option<Rgb>,
    /// Overrides [`Defaults::foreground`].
    #[serde(default)]
    pub foreground: Option<Rgb>,
    /// Overrides [`Defaults::glyph_scale`].
    #[serde(default)]
    pub glyph_scale: Option<f32>,
    /// What a press does.
    #[serde(default)]
    pub action: Action,
}

/// What pressing a key does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Nothing.
    #[default]
    None,
    /// Run a command through `sh -c`, without waiting for it.
    Run(String),
    /// Switch page: `"next"`, `"prev"`, or a page name.
    Page(String),
    /// Press a shortcut, such as `"cmd+ctrl+q"` (macOS).
    Keys(String),
    /// Type a string (macOS).
    Text(String),
    /// Press a media key: `mute`, `volume-up`, `volume-down`, `play-pause`,
    /// `next` or `previous` (macOS).
    Media(String),
    /// Open an app by name, or a URL, folder or file (macOS `open`).
    Open(String),
    /// Hold a shortcut for as long as the key is held (macOS).
    Hold(String),
    /// Hold a shortcut's modifiers while the key is held, tapping its key
    /// once on press: `cmd+tab` keeps the app switcher open (macOS).
    Switch(String),
    /// Click a mouse button where the pointer is: `left`, `right`,
    /// `middle`, `back` or `forward` (macOS).
    Mouse(String),
    /// `sleep`, `display-sleep` or `lock` (macOS).
    System(String),
    /// Several actions in order. Steps can be any action except `page`,
    /// `hold`, `switch` and another `sequence`, plus `delay`.
    Sequence(Vec<Action>),
    /// Wait this many milliseconds, up to 10 000; only inside a `sequence`.
    Delay(u64),
}

/// The `system` actions.
pub const SYSTEM_ACTIONS: [&str; 3] = ["sleep", "display-sleep", "lock"];

/// The longest `delay` a sequence may hold, in milliseconds.
pub const MAX_DELAY_MS: u64 = 10_000;

impl Action {
    /// Whether the action posts input events, which needs Accessibility.
    #[must_use]
    pub fn sends_keys(&self) -> bool {
        match self {
            Action::Keys(_)
            | Action::Text(_)
            | Action::Media(_)
            | Action::Hold(_)
            | Action::Switch(_)
            | Action::Mouse(_) => true,
            Action::System(name) => name == "lock",
            Action::Sequence(steps) => steps.iter().any(Action::sends_keys),
            _ => false,
        }
    }

    /// Whether the action acts on release as well as press.
    #[must_use]
    pub fn is_held(&self) -> bool {
        matches!(self, Action::Hold(_) | Action::Switch(_))
    }
}

/// The page buttons' actions.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageButtons {
    /// The left button.
    #[serde(default = "prev")]
    pub left: Action,
    /// The right button.
    #[serde(default = "next")]
    pub right: Action,
}

impl Default for PageButtons {
    fn default() -> Self {
        Self {
            left: prev(),
            right: next(),
        }
    }
}

fn prev() -> Action {
    Action::Page("prev".into())
}
fn next() -> Action {
    Action::Page("next".into())
}

/// A validated setup: every key resolved to a face and an action.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Panel brightness, when the config sets one.
    pub brightness: Option<u8>,
    /// The pages, in order.
    pub pages: Vec<ResolvedPage>,
    /// The left page button's action.
    pub left: Action,
    /// The right page button's action.
    pub right: Action,
}

/// A page with all nine keys filled in.
#[derive(Debug, Clone)]
pub struct ResolvedPage {
    /// The page's name.
    pub name: String,
    /// Keys 1–9 at indexes 0–8.
    pub keys: [(Face, Action); 9],
}

impl Setup {
    /// Whether the page buttons should be taken over: when there is more than
    /// one page to turn, or either button does something besides turning
    /// pages. Otherwise they keep their firmware behaviour, as in OpenLogi.
    #[must_use]
    pub fn needs_page_buttons(&self) -> bool {
        let pages_only = |action: &Action| matches!(action, Action::Page(_) | Action::None);
        self.pages.len() > 1 || !pages_only(&self.left) || !pages_only(&self.right)
    }

    /// Whether any key or page button sends keystrokes.
    #[must_use]
    pub fn sends_keys(&self) -> bool {
        self.left.sends_keys()
            || self.right.sends_keys()
            || self
                .pages
                .iter()
                .any(|page| page.keys.iter().any(|(_, action)| action.sends_keys()))
    }

    /// Read and validate the config at `path`.
    pub fn load(path: &Path) -> Result<Setup> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let config: Config = serde_json::from_str(&text)
            .with_context(|| format!("{} is not a valid keypad config", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        Setup::resolve(config, base).with_context(|| format!("in {}", path.display()))
    }

    /// Validate `config`, resolving icon paths against `base`.
    pub fn resolve(config: Config, base: &Path) -> Result<Setup> {
        if let Some(level) = config.brightness {
            if !(1..=100).contains(&level) {
                bail!("brightness: {level} is outside 1–100");
            }
        }
        if config.pages.is_empty() {
            bail!("pages: at least one page is needed");
        }
        let names: Vec<&str> = config.pages.iter().map(|p| p.name.as_str()).collect();
        let mut seen = HashSet::new();
        for name in &names {
            if !seen.insert(*name) {
                bail!("pages: the name {name:?} is used twice");
            }
        }
        check_action(&config.page_buttons.left, &names, "page_buttons.left")?;
        check_action(&config.page_buttons.right, &names, "page_buttons.right")?;

        let defaults = &config.defaults;
        let mut pages = Vec::new();
        for (p, page) in config.pages.iter().enumerate() {
            let mut keys: [(Face, Action); 9] = std::array::from_fn(|_| {
                (
                    Face {
                        background: defaults.background,
                        foreground: defaults.foreground,
                        glyph_scale: defaults.glyph_scale,
                        ..Face::default()
                    },
                    Action::None,
                )
            });
            for (index, key) in &page.keys {
                let at = format!("pages[{p}].keys.{index:?}");
                let slot = index
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=9).contains(n))
                    .with_context(|| format!("{at}: keys are \"1\" to \"9\""))?;
                check_action(&key.action, &names, &format!("{at}.action"))?;
                let icon = key
                    .icon
                    .as_deref()
                    .map(|icon| icon_source(icon, base))
                    .transpose()
                    .with_context(|| format!("{at}.icon"))?;
                let glyph_scale = key.glyph_scale.unwrap_or(defaults.glyph_scale);
                if !(0.05..=1.0).contains(&glyph_scale) {
                    bail!("{at}.glyph_scale: {glyph_scale} is outside 0.05–1");
                }
                keys[slot - 1] = (
                    Face {
                        icon,
                        label: key.label.clone(),
                        background: key.background.unwrap_or(defaults.background),
                        foreground: key.foreground.unwrap_or(defaults.foreground),
                        glyph_scale,
                    },
                    key.action.clone(),
                );
            }
            pages.push(ResolvedPage {
                name: page.name.clone(),
                keys,
            });
        }
        Ok(Setup {
            brightness: config.brightness,
            pages,
            left: config.page_buttons.left,
            right: config.page_buttons.right,
        })
    }

    /// The page an action switches to from page `current`, or `None` when it
    /// is not a page action.
    #[must_use]
    pub fn target(&self, current: usize, action: &Action) -> Option<usize> {
        let Action::Page(to) = action else {
            return None;
        };
        let count = self.pages.len();
        Some(match to.as_str() {
            "next" => (current + 1) % count,
            "prev" => (current + count - 1) % count,
            name => self.pages.iter().position(|p| p.name == name)?,
        })
    }
}

fn check_action(action: &Action, names: &[&str], at: &str) -> Result<()> {
    check_step(action, names, at, false)
}

/// Validate `action`; `in_sequence` when it is a step of a `sequence`.
fn check_step(action: &Action, names: &[&str], at: &str, in_sequence: bool) -> Result<()> {
    use crate::keys::{Chord, MediaKey, MouseButton};
    let chord = |chord: &str, what: &str| {
        Chord::parse(chord)
            .map(drop)
            .with_context(|| format!("{at}.{what}"))
    };
    match action {
        Action::Page(_) | Action::Hold(_) | Action::Switch(_) | Action::Sequence(_)
            if in_sequence =>
        {
            bail!("{at}: a sequence step cannot be page, hold, switch or another sequence")
        }
        Action::Delay(_) if !in_sequence => bail!("{at}: delay only works inside a sequence"),
        Action::Delay(ms) if *ms > MAX_DELAY_MS => {
            bail!("{at}.delay: {ms} ms is longer than {MAX_DELAY_MS} ms")
        }
        Action::Page(to) if to != "next" && to != "prev" && !names.contains(&to.as_str()) => {
            bail!("{at}: no page is named {to:?}")
        }
        Action::Run(command) if command.trim().is_empty() => bail!("{at}: empty command"),
        Action::Open(target) if target.trim().is_empty() => bail!("{at}: empty open target"),
        Action::Text(text) if text.is_empty() => bail!("{at}: empty text"),
        Action::Media(name) => MediaKey::parse(name)
            .map(drop)
            .with_context(|| format!("{at}.media")),
        Action::Mouse(name) => MouseButton::parse(name)
            .map(drop)
            .with_context(|| format!("{at}.mouse")),
        Action::System(name) if !SYSTEM_ACTIONS.contains(&name.as_str()) => {
            bail!("{at}.system: {name:?} is not one of {SYSTEM_ACTIONS:?}")
        }
        Action::Keys(c) => chord(c, "keys"),
        Action::Hold(c) => chord(c, "hold"),
        Action::Switch(c) => chord(c, "switch"),
        Action::Sequence(steps) if steps.is_empty() => bail!("{at}: empty sequence"),
        Action::Sequence(steps) => steps.iter().enumerate().try_for_each(|(i, step)| {
            check_step(step, names, &format!("{at}.sequence[{i}]"), true)
        }),
        _ => Ok(()),
    }
}

/// A built-in name when one exists; otherwise a file path that must exist and
/// have an image extension. Anything with a `/` or an extension is a path.
pub fn icon_source(icon: &str, base: &Path) -> Result<IconSource> {
    let looks_like_path = icon.contains('/') || Path::new(icon).extension().is_some();
    if !looks_like_path {
        if render::builtin_icon(icon).is_some() {
            return Ok(IconSource::Builtin(icon.to_string()));
        }
        bail!("no built-in icon is named {icon:?} (see `mx-keypad icons`)");
    }
    let path = expand(icon, base);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("svg" | "png" | "jpg" | "jpeg")) {
        bail!("{icon:?}: icons are .svg, .png or .jpg");
    }
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    Ok(IconSource::File(path))
}

fn expand(icon: &str, base: &Path) -> PathBuf {
    if let Some(rest) = icon.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    base.join(icon)
}

/// Where the config lives when `--config` is not given:
/// `$XDG_CONFIG_HOME/mx-keypad/config.json`, else `~/.config/mx-keypad/config.json`.
#[must_use]
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        });
    base.join("mx-keypad/config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(json: &str) -> Result<Setup> {
        Setup::resolve(serde_json::from_str(json)?, Path::new("."))
    }

    fn error(json: &str) -> String {
        format!("{:#}", setup(json).unwrap_err())
    }

    #[test]
    fn the_demo_config_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/demo.json");
        let setup = Setup::load(&path).unwrap();
        assert!(setup.pages.len() >= 2);
    }

    #[test]
    fn keys_fill_their_slots_and_inherit_defaults() {
        let setup = setup(
            r##"{
                "defaults": { "background": "#112233" },
                "pages": [{ "name": "a", "keys": {
                    "2": { "icon": "bell", "foreground": "#ff0000", "action": { "run": "true" } }
                } }]
            }"##,
        )
        .unwrap();
        let keys = &setup.pages[0].keys;
        assert_eq!(keys[0].0.icon, None);
        assert_eq!(keys[0].0.background, Rgb(0x11, 0x22, 0x33));
        assert_eq!(keys[1].0.icon, Some(IconSource::Builtin("bell".into())));
        assert_eq!(keys[1].0.foreground, Rgb(0xff, 0, 0));
        assert_eq!(keys[1].0.background, Rgb(0x11, 0x22, 0x33));
        assert_eq!(keys[1].1, Action::Run("true".into()));
        assert_eq!(setup.left, Action::Page("prev".into()));
    }

    #[test]
    fn the_new_actions_validate() {
        let ok = |action: &str| {
            setup(&format!(
                r#"{{ "pages": [{{ "name": "a", "keys": {{ "1": {{ "action": {action} }} }} }}] }}"#
            ))
        };
        for action in [
            r#"{ "open": "Safari" }"#,
            r#"{ "hold": "cmd+shift+m" }"#,
            r#"{ "switch": "cmd+tab" }"#,
            r#"{ "mouse": "back" }"#,
            r#"{ "system": "display-sleep" }"#,
            r#"{ "sequence": [{ "keys": "cmd+a" }, { "delay": 100 }, { "text": "hi" }] }"#,
        ] {
            assert!(ok(action).is_ok(), "{action}");
        }
        let err = |action: &str| format!("{:#}", ok(action).unwrap_err());
        assert!(err(r#"{ "delay": 5 }"#).contains("only works inside a sequence"));
        assert!(err(r#"{ "sequence": [{ "delay": 20000 }] }"#).contains("longer than"));
        assert!(err(r#"{ "sequence": [{ "page": "a" }] }"#).contains("cannot be page"));
        assert!(
            err(r#"{ "sequence": [{ "sequence": [{ "text": "x" }] }] }"#)
                .contains("another sequence")
        );
        assert!(err(r#"{ "sequence": [] }"#).contains("empty sequence"));
        assert!(err(r#"{ "system": "reboot" }"#).contains("not one of"));
        assert!(err(r#"{ "mouse": "thumb" }"#).contains(".mouse"));
        assert!(err(r#"{ "hold": "cmd+" }"#).contains(".hold"));
        assert!(err(r#"{ "sequence": [{ "keys": "cmd+zz" }] }"#).contains("sequence[0]"));
    }

    #[test]
    fn page_buttons_are_taken_over_only_when_used() {
        let one = setup(r#"{ "pages": [{ "name": "a" }] }"#).unwrap();
        assert!(!one.needs_page_buttons());
        let two = setup(r#"{ "pages": [{ "name": "a" }, { "name": "b" }] }"#).unwrap();
        assert!(two.needs_page_buttons());
        let bound = setup(
            r#"{ "pages": [{ "name": "a" }], "page_buttons": { "left": { "run": "true" } } }"#,
        )
        .unwrap();
        assert!(bound.needs_page_buttons());
    }

    #[test]
    fn page_actions_wrap_and_jump_by_name() {
        let setup =
            setup(r#"{ "pages": [{ "name": "a" }, { "name": "b" }, { "name": "c" }] }"#).unwrap();
        let page = |to: &str| Action::Page(to.into());
        assert_eq!(setup.target(2, &page("next")), Some(0));
        assert_eq!(setup.target(0, &page("prev")), Some(2));
        assert_eq!(setup.target(0, &page("b")), Some(1));
        assert_eq!(setup.target(0, &Action::Run("x".into())), None);
    }

    #[test]
    fn mistakes_name_where_they_are() {
        assert!(error(r#"{ "pages": [] }"#).contains("at least one page"));
        assert!(error(r#"{ "brightness": 0, "pages": [{ "name": "a" }] }"#).contains("brightness"));
        assert!(error(r#"{ "pages": [{ "name": "a" }, { "name": "a" }] }"#).contains("twice"));
        assert!(
            error(r#"{ "pages": [{ "name": "a", "keys": { "10": {} } }] }"#)
                .contains(r#"pages[0].keys."10""#)
        );
        assert!(
            error(r#"{ "pages": [{ "name": "a", "keys": { "1": { "icon": "nope" } } }] }"#)
                .contains("no built-in icon")
        );
        assert!(
            error(r#"{ "pages": [{ "name": "a", "keys": { "1": { "icon": "./x.png" } } }] }"#)
                .contains("does not exist")
        );
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": { "1": { "action": { "page": "z" } } } }] }"#
        )
        .contains("no page is named"));
        assert!(error(
            r#"{ "pages": [{ "name": "a" }], "page_buttons": { "left": { "page": "z" } } }"#
        )
        .contains("page_buttons.left"));
        assert!(
            error(r#"{ "pages": [{ "name": "a", "keys": { "1": { "colour": "black" } } }] }"#)
                .contains("unknown field")
        );
        assert!(setup(
            r##"{ "pages": [{ "name": "a", "keys": { "1": { "background": "#12" } } }] }"##
        )
        .is_err());
    }
}
