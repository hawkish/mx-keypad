//! Keystrokes a key can send: a shortcut such as `cmd+shift+4`, a string
//! typed as text, or a media key.
//!
//! mx-keypad posts the events itself, so macOS asks Accessibility permission
//! of mx-keypad rather than of whatever terminal or tmux server launched it.
//!
//! A shortcut names its key either by position (`return`, `left`, `f5`, …),
//! which is the same on every layout, or by the character it types (`q`,
//! `[`, `å`). A character is looked up in the current keyboard layout when
//! the shortcut is sent, together with the modifiers that layout needs for
//! it: on a Danish layout `cmd+[` is sent as cmd+option+8, the keys that type
//! `[` there. Characters no key types fall back to their US-ANSI position.
//! Text is sent as Unicode and does not depend on the layout.

use std::collections::HashMap;

use anyhow::{bail, Result};

/// A shortcut: modifiers held while one key is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// Held modifiers.
    pub modifiers: Modifiers,
    /// The key pressed.
    pub key: Key,
}

/// The key of a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A key by its macOS virtual key code: one whose position is the same on
    /// every layout.
    Code(u16),
    /// The key that types this character on the current layout.
    Char(char),
}

/// Modifier keys, as flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Command.
    pub cmd: bool,
    /// Control.
    pub ctrl: bool,
    /// Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Modifiers {
    const SHIFT: Modifiers = Modifiers {
        cmd: false,
        ctrl: false,
        alt: false,
        shift: true,
    };
    const ALT: Modifiers = Modifiers {
        cmd: false,
        ctrl: false,
        alt: true,
        shift: false,
    };
    const SHIFT_ALT: Modifiers = Modifiers {
        cmd: false,
        ctrl: false,
        alt: true,
        shift: true,
    };

    /// Both sets of modifiers held.
    #[must_use]
    pub const fn with(self, other: Modifiers) -> Modifiers {
        Modifiers {
            cmd: self.cmd || other.cmd,
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
        }
    }
}

/// A shortcut resolved to what is actually pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// The macOS virtual key code.
    pub code: u16,
    /// Every modifier held: the shortcut's own and those the layout needs.
    pub modifiers: Modifiers,
}

impl Chord {
    /// Parse `"cmd+ctrl+q"`: modifiers and one key, joined by `+`, any case.
    /// `plus` names the `+` character, since `+` is the separator.
    pub fn parse(text: &str) -> Result<Chord> {
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            let part = part.to_lowercase();
            match part.as_str() {
                "cmd" | "command" | "super" => modifiers.cmd = true,
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "opt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "" => bail!("{text:?}: empty part between `+`"),
                name => {
                    if key.is_some() {
                        bail!("{text:?}: more than one non-modifier key");
                    }
                    key = Some(
                        parse_key(name)
                            .ok_or_else(|| anyhow::anyhow!("{text:?}: unknown key {name:?}"))?,
                    );
                }
            }
        }
        let Some(key) = key else {
            bail!("{text:?}: no key besides the modifiers");
        };
        Ok(Chord { modifiers, key })
    }

    /// What to press for this shortcut on the current keyboard layout. On
    /// macOS this asks the Text Input Sources API, which must be called on
    /// the main thread.
    #[must_use]
    pub fn resolve(&self) -> Resolved {
        let (code, needed) = match self.key {
            Key::Code(code) => (code, Modifiers::default()),
            Key::Char(c) => platform::layout_key(c)
                .or_else(|| ansi_code(c).map(|code| (code, Modifiers::default())))
                // `parse_key` only accepts characters with an ANSI fallback
                // or a single non-ASCII character; the latter with no key on
                // this layout is sent as key code 0, which types nothing
                // harmful and is reported by the app as unknown.
                .unwrap_or((0, Modifiers::default())),
        };
        Resolved {
            code,
            modifiers: self.modifiers.with(needed),
        }
    }
}

/// A key name: a position, a punctuation alias, or one character.
fn parse_key(name: &str) -> Option<Key> {
    const FKEYS: [u16; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
        return FKEYS.get(n.checked_sub(1)?).copied().map(Key::Code);
    }
    let position = match name {
        "return" | "enter" => Some(36),
        "tab" => Some(48),
        "space" => Some(49),
        "delete" | "backspace" => Some(51),
        "escape" | "esc" => Some(53),
        "forwarddelete" => Some(117),
        "home" => Some(115),
        "end" => Some(119),
        "pageup" => Some(116),
        "pagedown" => Some(121),
        "left" => Some(123),
        "right" => Some(124),
        "down" => Some(125),
        "up" => Some(126),
        _ => None,
    };
    if let Some(code) = position {
        return Some(Key::Code(code));
    }
    let alias = match name {
        "plus" => Some('+'),
        "equal" => Some('='),
        "minus" => Some('-'),
        "quote" => Some('\''),
        "semicolon" => Some(';'),
        "backslash" => Some('\\'),
        "comma" => Some(','),
        "slash" => Some('/'),
        "period" => Some('.'),
        "grave" => Some('`'),
        _ => None,
    };
    if let Some(c) = alias {
        return Some(Key::Char(c));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() && !c.is_whitespace() => Some(Key::Char(c)),
        _ => None,
    }
}

/// The US-ANSI key code that types `c` without modifiers.
fn ansi_code(c: char) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16,
        6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    if c.is_ascii_lowercase() {
        return Some(LETTERS[usize::from(c as u8 - b'a')]);
    }
    if c.is_ascii_digit() {
        return Some(DIGITS[usize::from(c as u8 - b'0')]);
    }
    Some(match c {
        '=' | '+' => 24,
        '-' => 27,
        ']' => 30,
        '[' => 33,
        '\'' => 39,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        '.' => 47,
        '`' => 50,
        _ => return None,
    })
}

/// Every character a layout types, with the key code and modifiers that
/// type it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) type LayoutTable = HashMap<char, (u16, Modifiers)>;

/// The numeric keypad's key codes: searched only after the main block, so a
/// digit or `+` resolves to the keys a shortcut means.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const KEYPAD: std::ops::RangeInclusive<u16> = 65..=92;

/// Every character a layout types, with the key code and modifiers that
/// type it. `translate(code, modifiers)` is the layout. The main block wins
/// over the numeric keypad; within each, the plainest way to type a
/// character wins: no modifiers, then shift, option, shift+option.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn layout_table(translate: impl Fn(u16, Modifiers) -> Option<char>) -> LayoutTable {
    let mut table = HashMap::new();
    let main = (0..128).filter(|code| !KEYPAD.contains(code));
    for codes in [main.collect::<Vec<_>>(), KEYPAD.collect()] {
        for modifiers in [
            Modifiers::default(),
            Modifiers::SHIFT,
            Modifiers::ALT,
            Modifiers::SHIFT_ALT,
        ] {
            for &code in &codes {
                if let Some(c) = translate(code, modifiers) {
                    table.entry(c).or_insert((code, modifiers));
                }
            }
        }
    }
    table
}

/// Whether this process may post keystrokes. With `prompt`, macOS shows its
/// Accessibility request when it may not (once per process, if ever).
#[must_use]
pub fn trusted(prompt: bool) -> bool {
    platform::trusted(prompt)
}

/// Press and release `chord`.
pub fn send_chord(chord: Chord) -> Result<()> {
    tap(chord.resolve())
}

/// Press and release a shortcut already resolved, on any thread.
pub fn tap(resolved: Resolved) -> Result<()> {
    platform::post_modifiers(resolved.modifiers, true)?;
    let tapped = platform::post_key(resolved.code, resolved.modifiers, true)
        .and_then(|()| platform::post_key(resolved.code, resolved.modifiers, false));
    platform::post_modifiers(resolved.modifiers, false)?;
    tapped
}

/// Press (`down`) or release a resolved shortcut's modifiers.
pub fn press_modifiers(modifiers: Modifiers, down: bool) -> Result<()> {
    platform::post_modifiers(modifiers, down)
}

/// Press (`down`) or release a resolved shortcut's key, with `modifiers`
/// marked as held.
pub fn press_key(resolved: Resolved, down: bool) -> Result<()> {
    platform::post_key(resolved.code, resolved.modifiers, down)
}

/// Type `text`.
pub fn type_text(text: &str) -> Result<()> {
    platform::type_text(text)
}

/// A mouse button to click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Primary click.
    Left,
    /// Secondary click.
    Right,
    /// Wheel click.
    Middle,
    /// The side button that goes back (button 4).
    Back,
    /// The side button that goes forward (button 5).
    Forward,
}

impl MouseButton {
    /// Parse `left`, `right`, `middle`, `back` or `forward`.
    pub fn parse(name: &str) -> Result<MouseButton> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "left" => MouseButton::Left,
            "right" => MouseButton::Right,
            "middle" => MouseButton::Middle,
            "back" => MouseButton::Back,
            "forward" => MouseButton::Forward,
            _ => bail!("{name:?}: mouse buttons are left, right, middle, back and forward"),
        })
    }
}

/// Click `button` where the pointer is.
pub fn click(button: MouseButton) -> Result<()> {
    platform::click(button)
}

/// A media key, as the keyboard's top row sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKey {
    /// Toggle mute.
    Mute,
    /// Volume up a step.
    VolumeUp,
    /// Volume down a step.
    VolumeDown,
    /// Play or pause.
    PlayPause,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
}

impl MediaKey {
    /// Parse a media key name: `mute`, `volume-up`, `volume-down`,
    /// `play-pause`, `next` or `previous`.
    pub fn parse(name: &str) -> Result<MediaKey> {
        Ok(match name.to_ascii_lowercase().as_str() {
            "mute" => MediaKey::Mute,
            "volume-up" => MediaKey::VolumeUp,
            "volume-down" => MediaKey::VolumeDown,
            "play-pause" | "play" => MediaKey::PlayPause,
            "next" => MediaKey::Next,
            "previous" | "prev" => MediaKey::Previous,
            _ => bail!(
                "{name:?}: media keys are mute, volume-up, volume-down, play-pause, next and previous"
            ),
        })
    }

    /// The `NX_KEYTYPE_*` code macOS gives the key.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    const fn nx_code(self) -> isize {
        match self {
            MediaKey::VolumeUp => 0,
            MediaKey::VolumeDown => 1,
            MediaKey::Mute => 7,
            MediaKey::PlayPause => 16,
            MediaKey::Next => 17,
            MediaKey::Previous => 18,
        }
    }
}

/// Press and release a media key. Unlike scripting the volume, this is the
/// key the keyboard sends, so it shows the system HUD and reaches every
/// output device the hardware key would.
pub fn send_media(key: MediaKey) -> Result<()> {
    platform::send_media(key)
}

#[cfg(target_os = "macos")]
mod platform {
    use anyhow::{anyhow, bail, Result};
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::Mutex;

    use super::{LayoutTable, Modifiers};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }

    pub(super) fn trusted(prompt: bool) -> bool {
        // SAFETY: a framework constant, valid for the life of the process.
        let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::from(prompt))]);
        // SAFETY: `options` is a valid CFDictionary for the call.
        unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| anyhow!("cannot create a keyboard event source"))
    }

    fn require_trust() -> Result<()> {
        if !trusted(false) {
            bail!(
                "macOS has not allowed mx-keypad to send keystrokes: turn it on under \
                 System Settings → Privacy & Security → Accessibility"
            );
        }
        Ok(())
    }

    fn flags(m: Modifiers) -> CGEventFlags {
        let mut flags = CGEventFlags::empty();
        if m.cmd {
            flags |= CGEventFlags::CGEventFlagCommand;
        }
        if m.ctrl {
            flags |= CGEventFlags::CGEventFlagControl;
        }
        if m.alt {
            flags |= CGEventFlags::CGEventFlagAlternate;
        }
        if m.shift {
            flags |= CGEventFlags::CGEventFlagShift;
        }
        flags
    }

    pub(super) fn post_key(code: u16, modifiers: Modifiers, down: bool) -> Result<()> {
        require_trust()?;
        let event = CGEvent::new_keyboard_event(source()?, code, down)
            .map_err(|()| anyhow!("cannot create a key event"))?;
        event.set_flags(flags(modifiers));
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    /// Press or release the modifier keys themselves, so apps that watch
    /// modifier state (push-to-talk, the app switcher) see them held.
    pub(super) fn post_modifiers(modifiers: Modifiers, down: bool) -> Result<()> {
        // Left command, shift, option, control.
        let keys = [
            (
                modifiers.cmd,
                55,
                Modifiers {
                    cmd: true,
                    ..Modifiers::default()
                },
            ),
            (
                modifiers.shift,
                56,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            ),
            (
                modifiers.alt,
                58,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
            ),
            (
                modifiers.ctrl,
                59,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            ),
        ];
        let mut held = if down {
            Modifiers::default()
        } else {
            modifiers
        };
        for (wanted, code, flag) in keys {
            if !wanted {
                continue;
            }
            if down {
                held = held.with(flag);
            } else {
                held = Modifiers {
                    cmd: held.cmd && !flag.cmd,
                    ctrl: held.ctrl && !flag.ctrl,
                    alt: held.alt && !flag.alt,
                    shift: held.shift && !flag.shift,
                };
            }
            require_trust()?;
            let event = CGEvent::new_keyboard_event(source()?, code, down)
                .map_err(|()| anyhow!("cannot create a key event"))?;
            event.set_type(core_graphics::event::CGEventType::FlagsChanged);
            event.set_flags(flags(held));
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: CFStringRef) -> *const c_void;
        static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
        static kTISPropertyInputSourceID: CFStringRef;
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            layout: *const c_void,
            code: u16,
            action: u16,
            modifier_state: u32,
            keyboard_type: u32,
            options: u32,
            dead_key_state: *mut u32,
            max_length: usize,
            actual_length: *mut usize,
            chars: *mut u16,
        ) -> i32;
    }

    extern "C" {
        fn CFRelease(cf: *const c_void);
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    }

    /// The layout table of the layout it was built for, by input source ID.
    static LAYOUT: Mutex<Option<(String, LayoutTable)>> = Mutex::new(None);

    /// The key and modifiers that type `c` on the current layout.
    pub(super) fn layout_key(c: char) -> Option<(u16, Modifiers)> {
        // SAFETY: TIS returns a +1 reference we release below; the layout
        // data is owned by the source and read only while it is alive.
        unsafe {
            let source = TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return None;
            }
            let id = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
            let id = if id.is_null() {
                String::new()
            } else {
                CFString::wrap_under_get_rule(id.cast()).to_string()
            };
            let mut cache = LAYOUT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cache.as_ref().map(|(cached, _)| cached) != Some(&id) {
                let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
                let table = if data.is_null() {
                    HashMap::new()
                } else {
                    let layout = CFDataGetBytePtr(data).cast::<c_void>();
                    let kind = u32::from(LMGetKbdType());
                    super::layout_table(|code, modifiers| translate(layout, kind, code, modifiers))
                };
                *cache = Some((id, table));
            }
            CFRelease(source);
            cache.as_ref().and_then(|(_, table)| table.get(&c).copied())
        }
    }

    /// The character `code` types with `modifiers` held, ignoring dead keys.
    unsafe fn translate(layout: *const c_void, kind: u32, code: u16, m: Modifiers) -> Option<char> {
        // Carbon modifier bits, shifted down a byte: shift 0x02, option 0x08.
        let state = (u32::from(m.shift) << 1) | (u32::from(m.alt) << 3);
        let mut dead = 0u32;
        let mut buf = [0u16; 4];
        let mut len = 0usize;
        // kUCKeyActionDisplay, kUCKeyTranslateNoDeadKeysMask.
        let status = UCKeyTranslate(
            layout,
            code,
            3,
            state,
            kind,
            1,
            &mut dead,
            buf.len(),
            &mut len,
            buf.as_mut_ptr(),
        );
        if status != 0 || len == 0 {
            return None;
        }
        let mut chars = char::decode_utf16(buf[..len].iter().copied());
        match (chars.next(), chars.next()) {
            (Some(Ok(c)), None) if !c.is_control() => Some(c),
            _ => None,
        }
    }

    pub(super) fn click(button: super::MouseButton) -> Result<()> {
        use super::MouseButton;
        use core_graphics::event::{CGEventType, CGMouseButton, EventField};

        require_trust()?;
        let at = CGEvent::new(source()?)
            .map_err(|()| anyhow!("cannot read the pointer position"))?
            .location();
        // Back and forward are "other" buttons 3 and 4, as a mouse's side
        // buttons report them; middle is other button 2.
        let (down, up, kind, number) = match button {
            MouseButton::Left => (
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGMouseButton::Left,
                0,
            ),
            MouseButton::Right => (
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGMouseButton::Right,
                1,
            ),
            MouseButton::Middle => (
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                2,
            ),
            MouseButton::Back => (
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                3,
            ),
            MouseButton::Forward => (
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                4,
            ),
        };
        for kind_of in [down, up] {
            let event = CGEvent::new_mouse_event(source()?, kind_of, at, kind)
                .map_err(|()| anyhow!("cannot create a mouse event"))?;
            event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, number);
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }

    /// `NSPoint`, for `otherEventWithType:…`'s location argument.
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }

    // SAFETY: the layout matches CGPoint's `{CGPoint=dd}` encoding.
    unsafe impl objc2::Encode for Point {
        const ENCODING: objc2::Encoding =
            objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    extern "C" {
        fn CGEventPost(tap: u32, event: *mut c_void);
    }

    pub(super) fn send_media(key: super::MediaKey) -> Result<()> {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};

        require_trust()?;
        // A media key is an NSSystemDefined event (type 14) of subtype 8
        // (NX_SUBTYPE_AUX_CONTROL_BUTTONS): data1 holds the key code in its
        // high half and the key state (0xa down, 0xb up) above the low byte.
        for state in [0xa_isize, 0xb] {
            objc2::rc::autoreleasepool(|_| {
                let data1 = (key.nx_code() << 16) | (state << 8);
                // SAFETY: a documented NSEvent class method; every argument
                // has the type its selector declares.
                let event: *mut AnyObject = unsafe {
                    msg_send![
                        class!(NSEvent),
                        otherEventWithType: 14_usize,
                        location: Point { x: 0.0, y: 0.0 },
                        modifierFlags: (state << 8) as usize,
                        timestamp: 0.0_f64,
                        windowNumber: 0_isize,
                        context: std::ptr::null_mut::<AnyObject>(),
                        subtype: 8_i16,
                        data1: data1,
                        data2: -1_isize
                    ]
                };
                if event.is_null() {
                    bail!("cannot create a media key event");
                }
                // SAFETY: `event` is a live NSEvent within this pool.
                let cg: *mut c_void = unsafe { msg_send![event, CGEvent] };
                if cg.is_null() {
                    bail!("cannot convert the media key event");
                }
                // SAFETY: `cg` is a valid CGEventRef owned by `event`;
                // 0 is kCGHIDEventTap.
                unsafe { CGEventPost(0, cg) };
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(super) fn type_text(text: &str) -> Result<()> {
        require_trust()?;
        // One character per event pair: apps that read the key code rather
        // than the attached string (terminals among them) still see each one.
        for c in text.chars() {
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            let s = String::from_utf16_lossy(units);
            for down in [true, false] {
                let event = CGEvent::new_keyboard_event(source()?, 0, down)
                    .map_err(|()| anyhow!("cannot create a key event"))?;
                event.set_string(&s);
                event.post(CGEventTapLocation::HID);
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{bail, Result};

    use super::Modifiers;

    pub(super) fn trusted(_prompt: bool) -> bool {
        false
    }

    pub(super) fn layout_key(_c: char) -> Option<(u16, Modifiers)> {
        None
    }

    pub(super) fn post_key(_code: u16, _modifiers: Modifiers, _down: bool) -> Result<()> {
        bail!("sending keystrokes is only supported on macOS")
    }

    pub(super) fn post_modifiers(_modifiers: Modifiers, _down: bool) -> Result<()> {
        bail!("sending keystrokes is only supported on macOS")
    }

    pub(super) fn click(_button: super::MouseButton) -> Result<()> {
        bail!("clicking is only supported on macOS")
    }

    pub(super) fn type_text(_text: &str) -> Result<()> {
        bail!("sending keystrokes is only supported on macOS")
    }

    pub(super) fn send_media(_key: super::MediaKey) -> Result<()> {
        bail!("sending keystrokes is only supported on macOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_modifiers_in_any_order_and_case() {
        let lock = Chord::parse("Ctrl+CMD+q").unwrap();
        assert_eq!(lock.key, Key::Char('q'));
        assert!(lock.modifiers.cmd && lock.modifiers.ctrl);
        assert!(!lock.modifiers.alt && !lock.modifiers.shift);
        assert_eq!(Chord::parse("cmd+[").unwrap().key, Key::Char('['));
        assert_eq!(Chord::parse("cmd+å").unwrap().key, Key::Char('å'));
        assert_eq!(Chord::parse("cmd+plus").unwrap().key, Key::Char('+'));
        assert_eq!(Chord::parse("f12").unwrap().key, Key::Code(111));
        assert_eq!(Chord::parse("return").unwrap().key, Key::Code(36));
    }

    #[test]
    fn every_letter_and_digit_has_a_distinct_ansi_code() {
        let mut codes: Vec<u16> = ('a'..='z')
            .chain('0'..='9')
            .map(|c| ansi_code(c).unwrap())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 36);
    }

    /// A slice of the Danish layout: `[` is option+8, `+` sits where US has
    /// `-`, and `å` where US has `[`.
    fn danish(code: u16, m: Modifiers) -> Option<char> {
        let plain = m == Modifiers::default();
        match code {
            12 if plain => Some('q'),
            28 if plain => Some('8'),
            28 if m == Modifiers::SHIFT => Some('('),
            28 if m == Modifiers::ALT => Some('['),
            27 if plain => Some('+'),
            33 if plain => Some('å'),
            // The numeric keypad's 8 must lose to the top row's.
            91 if plain => Some('8'),
            _ => None,
        }
    }

    #[test]
    fn a_layout_table_finds_the_plainest_way_to_type_each_character() {
        let table = layout_table(danish);
        assert_eq!(table[&'['], (28, Modifiers::ALT));
        assert_eq!(table[&'q'], (12, Modifiers::default()));
        assert_eq!(table[&'+'], (27, Modifiers::default()));
        assert_eq!(table[&'å'], (33, Modifiers::default()));
        assert_eq!(table[&'8'], (28, Modifiers::default()));
        assert!(!table.contains_key(&'@'));
    }

    /// A slice of the US layout: `+` is shift+`=` on the main block, and
    /// unshifted on the numeric keypad.
    fn us(code: u16, m: Modifiers) -> Option<char> {
        match (code, m) {
            (24, m) if m == Modifiers::default() => Some('='),
            (24, m) if m == Modifiers::SHIFT => Some('+'),
            (69, m) if m == Modifiers::default() => Some('+'),
            _ => None,
        }
    }

    #[test]
    fn the_main_block_wins_over_the_numeric_keypad() {
        assert_eq!(layout_table(us)[&'+'], (24, Modifiers::SHIFT));
    }

    #[test]
    fn layout_modifiers_add_to_the_shortcuts_own() {
        let cmd = Modifiers {
            cmd: true,
            ..Modifiers::default()
        };
        assert_eq!(
            cmd.with(Modifiers::ALT),
            Modifiers {
                cmd: true,
                alt: true,
                ..Modifiers::default()
            }
        );
    }

    #[test]
    fn mouse_buttons_parse_their_names() {
        assert_eq!(MouseButton::parse("Back").unwrap(), MouseButton::Back);
        assert!(MouseButton::parse("thumb").is_err());
    }

    #[test]
    fn media_keys_parse_their_names() {
        assert_eq!(MediaKey::parse("Mute").unwrap(), MediaKey::Mute);
        assert_eq!(MediaKey::parse("volume-up").unwrap().nx_code(), 0);
        assert_eq!(MediaKey::parse("prev").unwrap(), MediaKey::Previous);
        assert!(MediaKey::parse("louder").is_err());
    }

    #[test]
    fn bad_chords_say_why() {
        let err = |s: &str| Chord::parse(s).unwrap_err().to_string();
        assert!(err("cmd+shift").contains("no key"));
        assert!(err("cmd+a+b").contains("more than one"));
        assert!(err("cmd+banana").contains("unknown key"));
        assert!(err("cmd+tab tab").contains("unknown key"));
        assert!(err("cmd++").contains("empty part"));
        assert!(err("f13").contains("unknown key"));
    }
}
