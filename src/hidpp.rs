//! Just enough HID++ 2.0 for the keypad: find a feature's index, divert the
//! page buttons on `0x1b04`, and read and set panel brightness on `0x8040`.
//!
//! The keypad answers on the same `ff43:1a02` node the vendor channel uses,
//! so replies arrive interleaved with grid frames and image acks. A request
//! hands back every report that is not its reply, so the caller can decode
//! presses that came in meanwhile.

use std::time::{Duration, Instant};

use thiserror::Error;

/// HID++ short report: 7 bytes.
const SHORT: u8 = 0x10;
/// HID++ long report: 20 bytes.
const LONG: u8 = 0x11;
/// Device index of a directly attached device.
const DEVICE: u8 = 0xff;
/// Software id marking our requests, so a reply can be told from an event
/// (events carry software id 0). OpenLogi's probes use the same.
const SWID: u8 = 0x0b;
/// Feature index of an HID++ 2.0 error reply.
const ERROR: u8 = 0xff;
/// How long a reply may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// `IRoot`, always at index 0.
const ROOT: u8 = 0x00;
/// Special keys and mouse buttons — diverts the page buttons.
pub(crate) const REPROG_CONTROLS_V4: u16 = 0x1b04;
/// Panel brightness.
pub(crate) const BRIGHTNESS_CONTROL: u16 = 0x8040;

/// `0x1b04` setCidReporting flags: divert, and the divert-valid bit.
const DIVERT: u8 = 0b11;
/// The divert-valid bit alone: hand the control back to the firmware.
const UNDIVERT: u8 = 0b10;

/// Why a request failed.
#[derive(Debug, Error)]
pub enum HidppError {
    /// The transport failed.
    #[error("HID transport: {0}")]
    Transport(String),
    /// No reply in time.
    #[error("no reply to feature {index:#04x} function {function} within 2 s")]
    Timeout {
        /// The feature index asked.
        index: u8,
        /// The function asked.
        function: u8,
    },
    /// The device answered with an HID++ 2.0 error.
    #[error("device refused feature {index:#04x} function {function}: error {code:#04x}")]
    Refused {
        /// The feature index asked.
        index: u8,
        /// The function asked.
        function: u8,
        /// The HID++ error code.
        code: u8,
    },
}

/// A raw HID report pipe. Implemented for `hidapi::HidDevice`, and scripted in
/// tests.
pub(crate) trait Link {
    /// Write one output report, report ID first.
    fn write(&self, report: &[u8]) -> Result<(), String>;
    /// Read one input report into `buf`, waiting up to `timeout`; `Ok(0)` when
    /// nothing came.
    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, String>;
}

impl Link for hidapi::HidDevice {
    fn write(&self, report: &[u8]) -> Result<(), String> {
        hidapi::HidDevice::write(self, report)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, String> {
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        self.read_timeout(buf, ms)
            .map_err(|error| error.to_string())
    }
}

/// Call `function` on the feature at `index` with `params` and return the
/// reply's 16-byte payload. Any other report read meanwhile is pushed onto
/// `stray`.
pub(crate) fn call(
    link: &impl Link,
    index: u8,
    function: u8,
    params: &[u8],
    stray: &mut Vec<Vec<u8>>,
) -> Result<[u8; 16], HidppError> {
    let fn_swid = (function << 4) | SWID;
    // Short when the parameters fit, as the probes sent them; long otherwise.
    let mut request = if params.len() <= 3 {
        vec![SHORT, DEVICE, index, fn_swid, 0, 0, 0]
    } else {
        let mut long = vec![0; 20];
        long[..4].copy_from_slice(&[LONG, DEVICE, index, fn_swid]);
        long
    };
    request[4..4 + params.len()].copy_from_slice(params);
    link.write(&request).map_err(HidppError::Transport)?;

    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut buf = [0u8; 64];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let n = link.read(&mut buf, left).map_err(HidppError::Transport)?;
        if n == 0 {
            continue;
        }
        let report = &buf[..n];
        match report {
            [SHORT | LONG, DEVICE, i, f, payload @ ..] if *i == index && *f == fn_swid => {
                let mut out = [0u8; 16];
                let len = payload.len().min(16);
                out[..len].copy_from_slice(&payload[..len]);
                return Ok(out);
            }
            [SHORT | LONG, DEVICE, ERROR, i, f, code, ..] if *i == index && *f == fn_swid => {
                return Err(HidppError::Refused {
                    index,
                    function,
                    code: *code,
                });
            }
            _ => stray.push(report.to_vec()),
        }
    }
    Err(HidppError::Timeout { index, function })
}

/// The index of `feature` in the device's feature table, or `None` when it
/// has none.
pub(crate) fn feature_index(
    link: &impl Link,
    feature: u16,
    stray: &mut Vec<Vec<u8>>,
) -> Result<Option<u8>, HidppError> {
    let [hi, lo] = feature.to_be_bytes();
    let reply = call(link, ROOT, 0, &[hi, lo], stray)?;
    Ok(Some(reply[0]).filter(|&index| index != 0))
}

/// Divert `cid` to us (`divert`), or hand it back to the firmware.
pub(crate) fn set_cid_reporting(
    link: &impl Link,
    reprog: u8,
    cid: u16,
    divert: bool,
    stray: &mut Vec<Vec<u8>>,
) -> Result<(), HidppError> {
    let [hi, lo] = cid.to_be_bytes();
    let flags = if divert { DIVERT } else { UNDIVERT };
    call(link, reprog, 3, &[hi, lo, flags, 0, 0], stray).map(drop)
}

/// The brightness range `0x8040` advertises: `(min, max)`.
pub(crate) fn brightness_range(
    link: &impl Link,
    brightness: u8,
    stray: &mut Vec<Vec<u8>>,
) -> Result<(u16, u16), HidppError> {
    let reply = call(link, brightness, 0, &[], stray)?;
    Ok((
        u16::from_be_bytes([reply[4], reply[5]]),
        u16::from_be_bytes([reply[0], reply[1]]),
    ))
}

/// Set the panel brightness. The caller clamps: OpenLogi's reference driver
/// (`node-logitech-mx-creative-console`) never writes 0, which it says resets
/// the device, although the keypad advertises a minimum of 0.
pub(crate) fn set_brightness(
    link: &impl Link,
    brightness: u8,
    level: u16,
    stray: &mut Vec<Vec<u8>>,
) -> Result<(), HidppError> {
    let [hi, lo] = level.to_be_bytes();
    call(link, brightness, 2, &[hi, lo], stray).map(drop)
}

#[cfg(test)]
pub(crate) mod scripted {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::time::Duration;

    use super::Link;

    /// A device that answers each write with whatever `respond` returns, and
    /// records what was written.
    pub(crate) struct Scripted<F: Fn(&[u8]) -> Vec<Vec<u8>>> {
        pub(crate) respond: F,
        pub(crate) written: RefCell<Vec<Vec<u8>>>,
        pub(crate) pending: RefCell<VecDeque<Vec<u8>>>,
    }

    impl<F: Fn(&[u8]) -> Vec<Vec<u8>>> Scripted<F> {
        pub(crate) fn new(respond: F) -> Self {
            Self {
                respond,
                written: RefCell::default(),
                pending: RefCell::default(),
            }
        }
    }

    impl<F: Fn(&[u8]) -> Vec<Vec<u8>>> Link for Scripted<F> {
        fn write(&self, report: &[u8]) -> Result<(), String> {
            self.written.borrow_mut().push(report.to_vec());
            self.pending.borrow_mut().extend((self.respond)(report));
            Ok(())
        }

        fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, String> {
            match self.pending.borrow_mut().pop_front() {
                Some(report) => {
                    buf[..report.len()].copy_from_slice(&report);
                    Ok(report.len())
                }
                None => {
                    std::thread::sleep(timeout.min(Duration::from_millis(5)));
                    Ok(0)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::Scripted;
    use super::*;

    /// A keypad whose feature table puts `0x1b04` at 11 and `0x8040` at 15,
    /// as the recorded fixture and the LCD probe found.
    fn keypad() -> Scripted<impl Fn(&[u8]) -> Vec<Vec<u8>>> {
        Scripted::new(|request: &[u8]| {
            let mut reply = vec![0u8; 20];
            reply[..4].copy_from_slice(&[LONG, DEVICE, request[2], request[3]]);
            match (request[2], request[3] >> 4, &request[4..6]) {
                (ROOT, 0, [0x1b, 0x04]) => reply[4] = 0x0b,
                (ROOT, 0, [0x80, 0x40]) => reply[4] = 0x0f,
                (ROOT, 0, _) => {}
                (0x0f, 0, _) => reply[4..10].copy_from_slice(&[0x00, 0x64, 0x65, 0x10, 0x00, 0x00]),
                (0x0b, 3, _) | (0x0f, 2, _) => reply[4..7].copy_from_slice(&request[4..7]),
                _ => {
                    reply = vec![SHORT, DEVICE, ERROR, request[2], request[3], 0x07, 0];
                }
            }
            // A grid press slips in ahead of every reply.
            vec![vec![0x13, 0xff, 0x02, 0x00, 0xc8, 0x01, 0x08], reply]
        })
    }

    #[test]
    fn feature_lookup_reads_the_root_reply_and_keeps_the_strays() {
        let link = keypad();
        let mut stray = Vec::new();
        assert_eq!(
            feature_index(&link, REPROG_CONTROLS_V4, &mut stray).unwrap(),
            Some(0x0b)
        );
        assert_eq!(
            feature_index(&link, BRIGHTNESS_CONTROL, &mut stray).unwrap(),
            Some(0x0f)
        );
        assert_eq!(feature_index(&link, 0x1234, &mut stray).unwrap(), None);
        assert_eq!(stray.len(), 3);
        assert_eq!(
            link.written.borrow()[0],
            [SHORT, DEVICE, ROOT, SWID, 0x1b, 0x04, 0]
        );
    }

    #[test]
    fn divert_and_undivert_write_the_valid_bit() {
        let link = keypad();
        let mut stray = Vec::new();
        set_cid_reporting(&link, 0x0b, 0x01a1, true, &mut stray).unwrap();
        set_cid_reporting(&link, 0x0b, 0x01a2, false, &mut stray).unwrap();
        let written = link.written.borrow();
        assert_eq!(
            written[0][..7],
            [LONG, DEVICE, 0x0b, 0x3b, 0x01, 0xa1, 0b11]
        );
        assert_eq!(
            written[1][..7],
            [LONG, DEVICE, 0x0b, 0x3b, 0x01, 0xa2, 0b10]
        );
    }

    #[test]
    fn brightness_info_matches_the_hardware_reading() {
        // The LCD probe read `00 64 65 10 00 00`: max 100, min 0.
        let link = keypad();
        let mut stray = Vec::new();
        assert_eq!(brightness_range(&link, 0x0f, &mut stray).unwrap(), (0, 100));
        set_brightness(&link, 0x0f, 70, &mut stray).unwrap();
        assert_eq!(
            link.written.borrow()[1],
            [SHORT, DEVICE, 0x0f, 0x2b, 0x00, 0x46, 0]
        );
    }

    #[test]
    fn an_error_reply_is_a_refusal() {
        let link = keypad();
        let error = call(&link, 0x0b, 9, &[], &mut Vec::new()).unwrap_err();
        assert!(matches!(error, HidppError::Refused { code: 0x07, .. }));
    }

    #[test]
    fn silence_times_out() {
        let link = Scripted::new(|_: &[u8]| Vec::new());
        let error = call(&link, 0x0b, 0, &[], &mut Vec::new()).unwrap_err();
        assert!(matches!(error, HidppError::Timeout { .. }));
    }
}
