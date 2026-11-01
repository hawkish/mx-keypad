//! Keystrokes a key can send: a shortcut such as `cmd+shift+4`, or a string
//! typed as text.
//!
//! mx-keypad posts the events itself, so macOS asks Accessibility permission
//! of mx-keypad rather than of whatever terminal or tmux server launched it.
//! Key codes are macOS virtual key codes for the ANSI layout; text is sent as
//! Unicode and does not depend on the layout.

use anyhow::{bail, Result};

/// A shortcut: modifiers held while one key is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// Held modifiers.
    pub modifiers: Modifiers,
    /// The macOS virtual key code pressed.
    pub key: u16,
}

/// Modifier keys, as flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

impl Chord {
    /// Parse `"cmd+ctrl+q"`: modifiers and one key, joined by `+`, any case.
    /// `plus` names the `=`/`+` key, since `+` is the separator; add `shift`
    /// to get `+`.
    pub fn parse(text: &str) -> Result<Chord> {
        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            let part = part.to_ascii_lowercase();
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
                        key_code(name)
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
}

/// The macOS virtual key code (ANSI layout) of a key name.
fn key_code(name: &str) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16,
        6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const FKEYS: [u16; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_lowercase() {
            return Some(LETTERS[usize::from(c as u8 - b'a')]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[usize::from(c as u8 - b'0')]);
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
        return FKEYS.get(n.checked_sub(1)?).copied();
    }
    Some(match name {
        "=" | "equal" => 24,
        "-" | "minus" => 27,
        "]" => 30,
        "[" => 33,
        "'" | "quote" => 39,
        ";" | "semicolon" => 41,
        "\\" | "backslash" => 42,
        "," | "comma" => 43,
        "/" | "slash" => 44,
        "." | "period" => 47,
        "`" | "grave" => 50,
        "plus" => 24,
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "forwarddelete" => 117,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    })
}

/// Whether this process may post keystrokes. With `prompt`, macOS shows its
/// Accessibility request when it may not (once per process, if ever).
#[must_use]
pub fn trusted(prompt: bool) -> bool {
    platform::trusted(prompt)
}

/// Press and release `chord`.
pub fn send_chord(chord: Chord) -> Result<()> {
    platform::send_chord(chord)
}

/// Type `text`.
pub fn type_text(text: &str) -> Result<()> {
    platform::type_text(text)
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

    use super::Chord;

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

    pub(super) fn send_chord(chord: Chord) -> Result<()> {
        require_trust()?;
        let mut flags = CGEventFlags::empty();
        let m = chord.modifiers;
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
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source()?, chord.key, down)
                .map_err(|()| anyhow!("cannot create a key event"))?;
            event.set_flags(flags);
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
        fn CGEventPost(tap: u32, event: *mut std::ffi::c_void);
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
                let cg: *mut std::ffi::c_void = unsafe { msg_send![event, CGEvent] };
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

    use super::Chord;

    pub(super) fn trusted(_prompt: bool) -> bool {
        false
    }

    pub(super) fn send_chord(_chord: Chord) -> Result<()> {
        bail!("sending keystrokes is only supported on macOS")
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
        assert_eq!(lock.key, 12);
        assert!(lock.modifiers.cmd && lock.modifiers.ctrl);
        assert!(!lock.modifiers.alt && !lock.modifiers.shift);
        assert_eq!(Chord::parse("cmd+[").unwrap().key, 33);
        assert_eq!(Chord::parse("shift + 4").unwrap().key, 21);
        assert_eq!(Chord::parse("f12").unwrap().key, 111);
        assert_eq!(
            Chord::parse("return").unwrap().modifiers,
            Modifiers::default()
        );
    }

    #[test]
    fn every_letter_and_digit_has_a_distinct_code() {
        let mut codes: Vec<u16> = ('a'..='z')
            .chain('0'..='9')
            .map(|c| key_code(&c.to_string()).unwrap())
            .collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 36);
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
        assert!(err("cmd++").contains("empty part"));
        assert!(err("f13").contains("unknown key"));
    }
}
