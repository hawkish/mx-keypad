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
use serde::{Deserialize, Serialize};

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
    /// How long a key with a `long_press` action must be held for it.
    #[serde(default = "long_press_ms")]
    pub long_press_ms: u64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            background: black(),
            foreground: white(),
            glyph_scale: glyph_scale(),
            long_press_ms: long_press_ms(),
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
/// OpenLogi's long-press threshold.
fn long_press_ms() -> u64 {
    500
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

/// One key's face and action. Serializes too, so a remote-page client can
/// send keys in exactly this shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    /// Built-in icon name or image path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Text under the icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Overrides [`Defaults::background`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgb>,
    /// Overrides [`Defaults::foreground`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<Rgb>,
    /// Overrides [`Defaults::glyph_scale`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_scale: Option<f32>,
    /// What a press does.
    #[serde(default, skip_serializing_if = "Action::is_none")]
    pub action: Action,
    /// What holding the key past `defaults.long_press_ms` does instead. A
    /// key with one acts on release when tapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_press: Option<Action>,
    /// Make the key blink between its face and these colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blink: Option<Blink>,
    /// What the key shows and does while the keypad's shift is on; fields
    /// left out follow the key's own, shifted automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shifted: Option<Shifted>,
}

/// A key's overrides while shift is on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shifted {
    /// Built-in icon name or image path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Text under the icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Background colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<Rgb>,
    /// Icon and label colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<Rgb>,
    /// Icon size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_scale: Option<f32>,
    /// What a press does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// What a long press does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_press: Option<Action>,
}

/// A blinking key's alternate colours and pace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blink {
    /// Background of the alternate face.
    #[serde(default = "black")]
    pub background: Rgb,
    /// Icon and label colour of the alternate face; the key's own when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<Rgb>,
    /// How long each face shows.
    #[serde(default = "blink_ms")]
    pub interval_ms: u64,
}

/// Each face of a blinking key shows this long by default: 1 Hz.
fn blink_ms() -> u64 {
    500
}

/// The fastest and slowest blink allowed, in milliseconds.
pub const BLINK_RANGE_MS: std::ops::RangeInclusive<u64> = 200..=10_000;

/// What pressing a key does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Turn the keypad's shift on: `hold`, `toggle` or `once`.
    Shift(String),
}

/// What `action` does while the keypad's shift is on, when the key does not
/// say: text is typed in upper case, shortcuts get shift added, and a
/// sequence's text and shortcut steps are shifted. Anything else is the same.
#[must_use]
pub fn shifted_action(action: &Action) -> Action {
    let add_shift = |chord: &str| {
        if chord
            .split('+')
            .any(|part| part.trim().eq_ignore_ascii_case("shift"))
        {
            chord.to_string()
        } else {
            format!("shift+{chord}")
        }
    };
    match action {
        Action::Text(text) => Action::Text(text.to_uppercase()),
        Action::Keys(chord) => Action::Keys(add_shift(chord)),
        Action::Hold(chord) => Action::Hold(add_shift(chord)),
        Action::Switch(chord) => Action::Switch(add_shift(chord)),
        Action::Sequence(steps) => Action::Sequence(steps.iter().map(shifted_action).collect()),
        other => other.clone(),
    }
}

/// The `system` actions.
pub const SYSTEM_ACTIONS: [&str; 3] = ["sleep", "display-sleep", "lock"];

/// The longest `delay` a sequence may hold, in milliseconds.
pub const MAX_DELAY_MS: u64 = 10_000;

impl Action {
    /// Whether this is `none`, the action of a key that does nothing.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Action::None)
    }

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
    /// How long a press must last to count as a long press.
    pub long_press: std::time::Duration,
}

/// A page with all nine keys filled in.
#[derive(Debug, Clone)]
pub struct ResolvedPage {
    /// The page's name.
    pub name: String,
    /// Keys 1–9 at indexes 0–8.
    pub keys: [(Face, Action); 9],
    /// Their long-press actions, where they have one.
    pub long: [Option<Action>; 9],
    /// Blinking keys' colours and interval.
    pub blink: [Option<BlinkSpec>; 9],
    /// Keys 1–9 while shift is on.
    pub shifted_keys: [(Face, Action); 9],
    /// Their long-press actions while shift is on.
    pub shifted_long: [Option<Action>; 9],
}

/// A blinking key's alternate colours and pace, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct BlinkSpec {
    /// Background of the alternate face.
    pub background: Rgb,
    /// Foreground of the alternate face; the current face's when `None`.
    pub foreground: Option<Rgb>,
    /// How long each face shows.
    pub interval: std::time::Duration,
}

impl BlinkSpec {
    /// The alternate of `face`: the same icon and label in the blink colours.
    #[must_use]
    pub fn alternate(&self, face: &Face) -> Face {
        Face {
            background: self.background,
            foreground: self.foreground.unwrap_or(face.foreground),
            ..face.clone()
        }
    }
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
            || self.pages.iter().any(|page| {
                page.keys.iter().any(|(_, action)| action.sends_keys())
                    || page.long.iter().flatten().any(Action::sends_keys)
            })
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
            pages.push(resolve_page(
                page,
                &format!("pages[{p}]."),
                defaults,
                &names,
                base,
            )?);
        }
        Ok(Setup {
            brightness: config.brightness,
            pages,
            left: config.page_buttons.left,
            right: config.page_buttons.right,
            long_press: std::time::Duration::from_millis(defaults.long_press_ms),
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

/// Resolve a page a remote client sent: config keys with the default face
/// settings. Its icons must be built-in names or absolute or `~/` paths,
/// since a client's working directory means nothing to the agent, and its
/// `page` actions can only be `next` or `prev`.
pub fn resolve_remote_page(name: &str, keys: &BTreeMap<String, Key>) -> Result<ResolvedPage> {
    for (index, key) in keys {
        let icons = [
            key.icon.as_deref(),
            key.shifted.as_ref().and_then(|s| s.icon.as_deref()),
        ];
        for icon in icons.into_iter().flatten() {
            let looks_like_path = icon.contains('/') || Path::new(icon).extension().is_some();
            if looks_like_path && !Path::new(icon).is_absolute() && !icon.starts_with("~/") {
                bail!("keys.{index:?}.icon: {icon:?} must be absolute or start with ~/");
            }
        }
    }
    let page = Page {
        name: name.to_string(),
        keys: keys.clone(),
    };
    resolve_page(&page, "", &Defaults::default(), &[], Path::new("/"))
}

/// Resolve one page: every key's face, action, long press, blink and shifted
/// binding. `prefix` starts the error locations (`pages[0].`), `names` are
/// the pages a `page` action may name, and icon paths resolve against `base`.
fn resolve_page(
    page: &Page,
    prefix: &str,
    defaults: &Defaults,
    names: &[&str],
    base: &Path,
) -> Result<ResolvedPage> {
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
    let mut long: [Option<Action>; 9] = Default::default();
    let mut blink: [Option<BlinkSpec>; 9] = Default::default();
    let mut shifted_keys = keys.clone();
    let mut shifted_long: [Option<Action>; 9] = Default::default();
    for (index, key) in &page.keys {
        let at = format!("{prefix}keys.{index:?}");
        let slot = index
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .with_context(|| format!("{at}: keys are \"1\" to \"9\""))?;
        check_action(&key.action, names, &format!("{at}.action"))?;
        if matches!(key.action, Action::Shift(_)) && key.long_press.is_some() {
            bail!("{at}: a shift key cannot have long_press");
        }
        if let Some(long) = &key.long_press {
            if matches!(long, Action::Shift(_)) {
                bail!("{at}.long_press: shift only works as a key's action");
            }
            check_action(long, names, &format!("{at}.long_press"))?;
            // Held actions need the release edge, which a key with
            // two actions spends on telling them apart.
            for (what, action) in [("action", &key.action), ("long_press", long)] {
                if action.is_held() {
                    bail!("{at}.{what}: hold and switch cannot share a key with long_press");
                }
            }
        }
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
        long[slot - 1].clone_from(&key.long_press);
        if let Some(b) = &key.blink {
            if !BLINK_RANGE_MS.contains(&b.interval_ms) {
                bail!(
                    "{at}.blink.interval_ms: {} is outside {}–{}",
                    b.interval_ms,
                    BLINK_RANGE_MS.start(),
                    BLINK_RANGE_MS.end()
                );
            }
            blink[slot - 1] = Some(BlinkSpec {
                background: b.background,
                foreground: b.foreground,
                interval: std::time::Duration::from_millis(b.interval_ms),
            });
        }
        let (face, action) = shifted_binding(
            &keys[slot - 1],
            key.shifted.as_ref(),
            base,
            names,
            &format!("{at}.shifted"),
        )?;
        shifted_keys[slot - 1] = (face, action);
        shifted_long[slot - 1] = match key.shifted.as_ref().and_then(|s| s.long_press.clone()) {
            Some(long) => {
                if key.long_press.is_none() {
                    bail!("{at}.shifted.long_press: the key has no long_press");
                }
                check_action(&long, names, &format!("{at}.shifted.long_press"))?;
                Some(long)
            }
            None => key.long_press.as_ref().map(shifted_action),
        };
    }
    Ok(ResolvedPage {
        long,
        blink,
        shifted_keys,
        shifted_long,
        name: page.name.clone(),
        keys,
    })
}

/// A key's face and action while shift is on: its overrides, else its own
/// shifted automatically. Text keys show their label in upper case; a shift
/// key shows its colours swapped, so a latched shift is visible.
fn shifted_binding(
    (face, action): &(Face, Action),
    shifted: Option<&Shifted>,
    base: &Path,
    names: &[&str],
    at: &str,
) -> Result<(Face, Action)> {
    let over = shifted.cloned().unwrap_or_default();
    let shifted_action = match &over.action {
        Some(action) => {
            check_action(action, names, &format!("{at}.action"))?;
            action.clone()
        }
        None => shifted_action(action),
    };
    let mut face = face.clone();
    if matches!(action, Action::Shift(_)) {
        std::mem::swap(&mut face.background, &mut face.foreground);
    }
    if let Some(icon) = &over.icon {
        face.icon = Some(icon_source(icon, base).with_context(|| format!("{at}.icon"))?);
    }
    match (&over.label, action) {
        (Some(label), _) => face.label = Some(label.clone()),
        (None, Action::Text(_)) => face.label = face.label.map(|l| l.to_uppercase()),
        _ => {}
    }
    if let Some(background) = over.background {
        face.background = background;
    }
    if let Some(foreground) = over.foreground {
        face.foreground = foreground;
    }
    if let Some(glyph_scale) = over.glyph_scale {
        if !(0.05..=1.0).contains(&glyph_scale) {
            bail!("{at}.glyph_scale: {glyph_scale} is outside 0.05–1");
        }
        face.glyph_scale = glyph_scale;
    }
    Ok((face, shifted_action))
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
        Action::Page(_)
        | Action::Hold(_)
        | Action::Switch(_)
        | Action::Sequence(_)
        | Action::Shift(_)
            if in_sequence =>
        {
            bail!("{at}: a sequence step cannot be page, hold, switch, shift or another sequence")
        }
        Action::Shift(mode) => crate::shift::Mode::parse(mode)
            .map(drop)
            .with_context(|| format!("{at}.shift")),
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

/// The starter config, `examples/demo.json`, built in so a fresh install has
/// one whichever way it was built.
pub const STARTER: &str = include_str!("../examples/demo.json");

/// Write the starter config to `path` unless something is already there.
/// `Ok(true)` when it wrote it.
pub fn seed(path: &Path) -> Result<bool> {
    if path.symlink_metadata().is_ok() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(path, STARTER).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(true)
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
    fn seeding_writes_the_starter_once() {
        let dir = std::env::temp_dir().join(format!("mx-keypad-seed-{}", std::process::id()));
        let path = dir.join("mx-keypad/config.json");
        assert!(seed(&path).unwrap());
        assert!(Setup::load(&path).is_ok());
        std::fs::write(&path, "{}").unwrap();
        assert!(!seed(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        std::fs::remove_dir_all(&dir).ok();
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
    fn long_press_actions_resolve_and_reject_held_actions() {
        let setup = setup(
            r#"{ "defaults": { "long_press_ms": 800 }, "pages": [{ "name": "a", "keys": {
                "1": { "action": { "media": "play-pause" }, "long_press": { "media": "next" } }
            } }] }"#,
        )
        .unwrap();
        assert_eq!(setup.long_press, std::time::Duration::from_millis(800));
        assert_eq!(setup.pages[0].long[0], Some(Action::Media("next".into())));
        assert_eq!(setup.pages[0].long[1], None);
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
                "1": { "action": { "hold": "cmd+m" }, "long_press": { "text": "x" } } } }] }"#
        )
        .contains("cannot share a key with long_press"));
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
                "1": { "long_press": { "page": "nowhere" } } } }] }"#
        )
        .contains("long_press"));
    }

    #[test]
    fn blinking_keys_get_an_alternate_face() {
        let setup = setup(
            r##"{ "pages": [{ "name": "a", "keys": {
                "1": { "icon": "bell", "background": "#0000ff", "foreground": "#ffff00",
                       "blink": {} },
                "2": { "blink": { "background": "#ff0000", "foreground": "#000000",
                                  "interval_ms": 250 } }
            } }] }"##,
        )
        .unwrap();
        let spec = setup.pages[0].blink[0].clone().unwrap();
        let face = spec.alternate(&setup.pages[0].keys[0].0);
        assert_eq!(spec.interval, std::time::Duration::from_millis(500));
        assert_eq!(face.background, Rgb(0, 0, 0));
        assert_eq!(face.foreground, Rgb(0xff, 0xff, 0));
        assert_eq!(face.icon, setup.pages[0].keys[0].0.icon);
        let spec = setup.pages[0].blink[1].clone().unwrap();
        let face = spec.alternate(&setup.pages[0].keys[1].0);
        assert_eq!(spec.interval, std::time::Duration::from_millis(250));
        assert_eq!(face.foreground, Rgb(0, 0, 0));
        assert!(setup.pages[0].blink[2].is_none());
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": { "1": { "blink": { "interval_ms": 50 } } } }] }"#
        )
        .contains("blink.interval_ms"));
    }

    #[test]
    fn shifted_actions_follow_the_automatic_rules() {
        assert_eq!(
            shifted_action(&Action::Text("æ".into())),
            Action::Text("Æ".into())
        );
        assert_eq!(
            shifted_action(&Action::Text("ß".into())),
            Action::Text("SS".into())
        );
        assert_eq!(
            shifted_action(&Action::Keys("cmd+s".into())),
            Action::Keys("shift+cmd+s".into())
        );
        assert_eq!(
            shifted_action(&Action::Keys("Shift+cmd+s".into())),
            Action::Keys("Shift+cmd+s".into())
        );
        assert_eq!(
            shifted_action(&Action::Sequence(vec![
                Action::Text("ø".into()),
                Action::Delay(10),
                Action::Keys("tab".into()),
            ])),
            Action::Sequence(vec![
                Action::Text("Ø".into()),
                Action::Delay(10),
                Action::Keys("shift+tab".into()),
            ])
        );
        assert_eq!(
            shifted_action(&Action::Media("mute".into())),
            Action::Media("mute".into())
        );
    }

    #[test]
    fn shifted_bindings_resolve_with_overrides_and_upper_case_text_labels() {
        let setup = setup(
            r##"{ "pages": [{ "name": "a", "keys": {
                "1": { "label": "æ", "action": { "text": "æ" } },
                "2": { "label": "Save", "action": { "keys": "cmd+s" },
                       "shifted": { "label": "Save as", "action": { "keys": "cmd+shift+s" } } },
                "3": { "label": "Shift", "background": "#000000", "foreground": "#ffffff",
                       "action": { "shift": "toggle" } },
                "4": { "label": "Go", "action": { "run": "true" } }
            } }] }"##,
        )
        .unwrap();
        let page = &setup.pages[0];
        assert_eq!(page.shifted_keys[0].0.label.as_deref(), Some("Æ"));
        assert_eq!(page.shifted_keys[0].1, Action::Text("Æ".into()));
        assert_eq!(page.shifted_keys[1].0.label.as_deref(), Some("Save as"));
        assert_eq!(page.shifted_keys[1].1, Action::Keys("cmd+shift+s".into()));
        assert_eq!(page.shifted_keys[2].0.background, Rgb(0xff, 0xff, 0xff));
        assert_eq!(page.shifted_keys[2].0.foreground, Rgb(0, 0, 0));
        assert_eq!(page.shifted_keys[2].1, Action::Shift("toggle".into()));
        assert_eq!(page.shifted_keys[3].0.label.as_deref(), Some("Go"));
        assert_eq!(page.shifted_keys[3].1, Action::Run("true".into()));
        // Keys without a binding stay blank and inert when shifted.
        assert_eq!(page.shifted_keys[8].1, Action::None);
    }

    #[test]
    fn shift_is_only_a_keys_own_action() {
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
            "1": { "action": { "shift": "caps" } } } }] }"#
        )
        .contains(".shift"));
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
            "1": { "action": { "shift": "hold" }, "long_press": { "text": "x" } } } }] }"#
        )
        .contains("cannot have long_press"));
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
            "1": { "action": { "text": "x" }, "long_press": { "shift": "hold" } } } }] }"#
        )
        .contains("only works as a key's action"));
        assert!(error(
            r#"{ "pages": [{ "name": "a", "keys": {
            "1": { "action": { "sequence": [{ "shift": "once" }] } } } }] }"#
        )
        .contains("cannot be page, hold, switch, shift"));
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
