//! An open MX Dialpad: its four buttons, the big dial and the small roller,
//! all diverted to us over HID++ so the system no longer sees them as mouse
//! buttons and scrolling.
//!
//! The Dialpad connects over Bluetooth Low Energy and shows up as one HID
//! device with a mouse collection and the HID++ collection `ff43:0202`,
//! which takes long reports only. macOS will not open a device with a mouse
//! collection exclusively, so it is opened shared, and only with Input
//! Monitoring permission.
//!
//! Undiverted, the buttons are mouse buttons 4–7 and the dial and roller are
//! the horizontal and vertical wheels. Diverted, the buttons report on
//! `0x1b04` like the keypad's page buttons, and each turn reports on
//! `0x4610` as `11 ff [index] 00 [roller] [delta] [ms timestamp, 4 bytes]`.
//! See `PROTOCOL.md`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use hidapi::{HidApi, HidDevice};

use crate::geometry::VENDOR_ID;
use crate::hidpp::{self, Link, MULTI_ROLLER, REPROG_CONTROLS_V4, WIRELESS_STATUS};
use crate::input::{diff, Button, Event};

/// The MX Dialpad over Bluetooth.
pub const PRODUCT_ID: u16 = 0xbc00;
/// The HID++ collection's usage page.
const USAGE_PAGE: u16 = 0xff43;
/// HID++ long report ID.
const HIDPP_LONG: u8 = 0x11;
/// The mouse report the Dialpad sends for controls that are not diverted.
const MOUSE_REPORT: u8 = 0x02;
/// The buttons' control ids, top-left to bottom-right.
const BUTTONS: [u16; 4] = [0x0053, 0x0056, 0x0059, 0x005a];
/// `0x4610`'s roller numbers.
const ROLLER: u8 = 0;
const DIAL: u8 = 1;
/// How long one poll of the watcher thread waits, which bounds how long
/// dropping a [`Watcher`] takes.
const WATCH_POLL: Duration = Duration::from_millis(200);
/// How often the watcher looks for a Dialpad that is not there.
const WATCH_RETRY: Duration = Duration::from_secs(2);

/// Taken around every open: hidapi's exclusive-open setting on macOS is
/// global, and the Dialpad must be opened shared while the keypad is opened
/// exclusively.
pub(crate) static OPEN_LOCK: Mutex<()> = Mutex::new(());

/// One of the Dialpad's turning controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wheel {
    /// The big dial; positive is clockwise.
    Dial,
    /// The small roller; positive is up.
    Roller,
}

/// Something the Dialpad did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A button edge, on [`Button::Dial`].
    Button(Event),
    /// A turn of `delta` steps: the dial makes 180 a revolution, the
    /// roller 40.
    Turn(Wheel, i32),
}

/// An open Dialpad, its controls diverted. Dropping it hands them back.
pub struct Dialpad {
    handle: LongOnly,
    reprog: u8,
    rollers: u8,
    wireless: Option<u8>,
    held: BTreeSet<Button>,
    /// Inputs decoded while waiting on a reply, not yet handed out.
    pending: Vec<Input>,
}

/// The Dialpad's node, which only takes long HID++ reports.
struct LongOnly(HidDevice);

impl Link for LongOnly {
    fn write(&self, report: &[u8]) -> Result<(), String> {
        self.0.write(report).map(drop).map_err(|e| e.to_string())
    }

    fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, String> {
        Link::read(&self.0, buf, timeout)
    }

    fn long_only(&self) -> bool {
        true
    }
}

impl Dialpad {
    /// Open the Dialpad and divert its controls, or `Ok(None)` when none is
    /// connected.
    pub fn open(api: &HidApi) -> Result<Option<Dialpad>> {
        let Some(info) = api.device_list().find(|d| {
            d.vendor_id() == VENDOR_ID
                && d.product_id() == PRODUCT_ID
                && d.usage_page() == USAGE_PAGE
        }) else {
            return Ok(None);
        };
        let handle = {
            let _lock = OPEN_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            #[cfg(target_os = "macos")]
            api.set_open_exclusive(false);
            let opened = api.open_path(info.path());
            #[cfg(target_os = "macos")]
            api.set_open_exclusive(true);
            opened.map_err(|error| {
                let error = anyhow!("{error}");
                if format!("{error}").contains("not permitted") {
                    error.context(
                        "macOS refused to open the MX Dialpad: allow mx-keypad, or the \
                         terminal running it, under System Settings → Privacy & Security → \
                         Input Monitoring",
                    )
                } else {
                    error.context("cannot open the MX Dialpad (is Logi Options+ holding it?)")
                }
            })?
        };
        let handle = LongOnly(handle);
        let mut stray = Vec::new();
        let mut index = |feature: u16| hidpp::feature_index(&handle, feature, &mut stray);
        let reprog =
            index(REPROG_CONTROLS_V4)?.context("this Dialpad has no reprogrammable controls")?;
        let rollers = index(MULTI_ROLLER)?.context("this Dialpad has no roller feature")?;
        let wireless = index(WIRELESS_STATUS)?;
        let mut dialpad = Dialpad {
            handle,
            reprog,
            rollers,
            wireless,
            held: BTreeSet::new(),
            pending: Vec::new(),
        };
        dialpad.absorb(stray);
        dialpad.divert(true)?;
        Ok(Some(dialpad))
    }

    /// Take every control over (`true`), or hand them back to the firmware.
    fn divert(&mut self, divert: bool) -> Result<()> {
        let mut stray = Vec::new();
        let result: Result<()> = (|| {
            for cid in BUTTONS {
                hidpp::set_cid_reporting(&self.handle, self.reprog, cid, divert, &mut stray)?;
            }
            for roller in [ROLLER, DIAL] {
                hidpp::set_roller_mode(&self.handle, self.rollers, roller, divert, &mut stray)?;
            }
            Ok(())
        })();
        self.absorb(stray);
        result
    }

    /// Wait up to `timeout` for input; empty when nothing happened.
    pub fn poll(&mut self, timeout: Duration) -> Result<Vec<Input>> {
        if !self.pending.is_empty() {
            return Ok(std::mem::take(&mut self.pending));
        }
        let mut buf = [0u8; 64];
        let mut wait = timeout;
        let mut inputs = Vec::new();
        let mut rediverted = false;
        loop {
            let n =
                Link::read(&self.handle, &mut buf, wait).map_err(|e| anyhow!("HID read: {e}"))?;
            if n == 0 {
                return Ok(inputs);
            }
            match self.decode(&buf[..n]) {
                Decoded::Inputs(more) => inputs.extend(more),
                // The Dialpad reconnected, or slept, and forgot the divert:
                // take its controls back, once per poll.
                Decoded::Forgotten if !rediverted => {
                    rediverted = true;
                    self.held.clear();
                    self.divert(true)?;
                }
                Decoded::Forgotten => {}
            }
            wait = Duration::ZERO;
        }
    }

    fn decode(&mut self, report: &[u8]) -> Decoded {
        let mut inputs = Vec::new();
        match decode(report, self.reprog, self.rollers, self.wireless) {
            Some(Frame::Held(held)) => {
                inputs.extend(diff(&mut self.held, held).into_iter().map(Input::Button));
            }
            Some(Frame::Turn(wheel, delta)) => inputs.push(Input::Turn(wheel, delta)),
            Some(Frame::Forgotten) => return Decoded::Forgotten,
            None => {}
        }
        Decoded::Inputs(inputs)
    }

    fn absorb(&mut self, stray: Vec<Vec<u8>>) {
        for report in stray {
            if let Decoded::Inputs(inputs) = self.decode(&report) {
                self.pending.extend(inputs);
            }
        }
    }
}

impl Drop for Dialpad {
    fn drop(&mut self) {
        let _ = self.divert(false);
    }
}

enum Decoded {
    Inputs(Vec<Input>),
    Forgotten,
}

/// What one report on the Dialpad's node says.
#[derive(Debug, PartialEq, Eq)]
enum Frame {
    /// The buttons held now.
    Held(BTreeSet<Button>),
    /// A turn.
    Turn(Wheel, i32),
    /// The controls are not diverted any more.
    Forgotten,
}

/// Decode `report`, given the indexes of `0x1b04`, `0x4610` and `0x1d4b`.
/// `None` for replies and anything else that carries no input.
fn decode(report: &[u8], reprog: u8, rollers: u8, wireless: Option<u8>) -> Option<Frame> {
    match report {
        [HIDPP_LONG, 0xff, index, 0x00, cids @ ..] if *index == reprog && cids.len() >= 8 => {
            Some(Frame::Held(
                cids[..8]
                    .chunks(2)
                    .filter_map(|cid| {
                        let cid = u16::from_be_bytes([cid[0], cid[1]]);
                        let n = BUTTONS.iter().position(|&b| b == cid)?;
                        Some(Button::Dial(u8::try_from(n + 1).ok()?))
                    })
                    .collect(),
            ))
        }
        [HIDPP_LONG, 0xff, index, 0x00, roller, delta, ..] if *index == rollers => {
            let wheel = match *roller {
                ROLLER => Wheel::Roller,
                DIAL => Wheel::Dial,
                _ => return None,
            };
            Some(Frame::Turn(wheel, i32::from(i8::from_ne_bytes([*delta]))))
        }
        // A wireless status broadcast: the Dialpad (re)connected.
        [HIDPP_LONG, 0xff, index, 0x00, ..] if Some(*index) == wireless => Some(Frame::Forgotten),
        // A button or wheel on the mouse report: something is undiverted.
        [MOUSE_REPORT, buttons, _, _, _, _, wheel, hwheel, ..]
            if *buttons != 0 || *wheel != 0 || *hwheel != 0 =>
        {
            Some(Frame::Forgotten)
        }
        _ => None,
    }
}

/// What the [`Watcher`] thread reports.
#[derive(Debug)]
pub enum Feed {
    /// The Dialpad is open and diverted.
    Connected,
    /// It could not be opened or went away, and why; the watcher keeps
    /// trying. Repeats of the same reason are not sent.
    Lost(String),
    /// It did something.
    Input(Input),
}

/// A thread that keeps the Dialpad open, reconnecting as it comes and goes,
/// and feeds its input to whoever holds this. Dropping it stops the thread
/// and hands the Dialpad's controls back.
pub struct Watcher {
    feed: Receiver<Feed>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start watching for a Dialpad.
    pub fn spawn() -> Result<Watcher> {
        let (send, feed) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("dialpad".into())
            .spawn(move || watch(&send, &flag))?;
        Ok(Watcher {
            feed,
            stop,
            thread: Some(thread),
        })
    }

    /// Everything the Dialpad did since the last call.
    pub fn drain(&self) -> Vec<Feed> {
        self.feed.try_iter().collect()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(send: &Sender<Feed>, stop: &AtomicBool) {
    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            let _ = send.send(Feed::Lost(format!("cannot start hidapi: {error}")));
            return;
        }
    };
    // The last reason sent, so a Dialpad that stays away is reported once.
    let mut last = None::<String>;
    let lost = |last: &mut Option<String>, reason: String| {
        if last.as_ref() != Some(&reason) {
            let _ = send.send(Feed::Lost(reason.clone()));
            *last = Some(reason);
        }
    };
    while !stop.load(Ordering::Relaxed) {
        match Dialpad::open(&api) {
            Ok(Some(mut dialpad)) => {
                last = None;
                let _ = send.send(Feed::Connected);
                let reason = loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match dialpad.poll(WATCH_POLL) {
                        Ok(inputs) => {
                            for input in inputs {
                                let _ = send.send(Feed::Input(input));
                            }
                        }
                        Err(error) => break format!("{error:#}"),
                    }
                };
                drop(dialpad);
                lost(&mut last, reason);
            }
            Ok(None) => lost(&mut last, "waiting for the MX Dialpad (Bluetooth)".into()),
            Err(error) => lost(&mut last, format!("{error:#}")),
        }
        let deadline = std::time::Instant::now() + WATCH_RETRY;
        while std::time::Instant::now() < deadline {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(WATCH_POLL);
        }
        let _ = api.refresh_devices();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Indexes as the Dialpad's feature table has them: 0x1b04 at 10,
    // 0x4610 at 13, 0x1d4b at 4.
    fn frame(report: &[u8]) -> Option<Frame> {
        decode(report, 10, 13, Some(4))
    }

    fn long(head: &[u8]) -> Vec<u8> {
        let mut report = vec![0; 20];
        report[..head.len()].copy_from_slice(head);
        report
    }

    #[test]
    fn turns_decode_as_the_probe_recorded_them() {
        // Dial clockwise, dial counter-clockwise, roller up, roller down.
        let dial_cw = long(&[0x11, 0xff, 0x0d, 0x00, 0x01, 0x03, 0x00, 0x0a, 0x92, 0x36]);
        let dial_ccw = long(&[0x11, 0xff, 0x0d, 0x00, 0x01, 0xfb, 0x00, 0x0a, 0x99, 0x02]);
        let roller_up = long(&[0x11, 0xff, 0x0d, 0x00, 0x00, 0x02, 0x00, 0x0a, 0x9e, 0x7a]);
        let roller_down = long(&[0x11, 0xff, 0x0d, 0x00, 0x00, 0xfe, 0x00, 0x0a, 0xa3, 0x66]);
        assert_eq!(frame(&dial_cw), Some(Frame::Turn(Wheel::Dial, 3)));
        assert_eq!(frame(&dial_ccw), Some(Frame::Turn(Wheel::Dial, -5)));
        assert_eq!(frame(&roller_up), Some(Frame::Turn(Wheel::Roller, 2)));
        assert_eq!(frame(&roller_down), Some(Frame::Turn(Wheel::Roller, -2)));
    }

    #[test]
    fn buttons_decode_to_their_position() {
        let held = |cids: &[u8]| {
            let mut head = vec![0x11, 0xff, 0x0a, 0x00];
            head.extend_from_slice(cids);
            frame(&long(&head))
        };
        assert_eq!(
            held(&[0x00, 0x53]),
            Some(Frame::Held([Button::Dial(1)].into()))
        );
        assert_eq!(
            held(&[0x00, 0x5a, 0x00, 0x56]),
            Some(Frame::Held([Button::Dial(2), Button::Dial(4)].into()))
        );
        assert_eq!(held(&[]), Some(Frame::Held(BTreeSet::new())));
    }

    #[test]
    fn undiverted_mouse_input_means_the_divert_was_forgotten() {
        assert_eq!(
            frame(&[0x02, 0x08, 0, 0, 0, 0, 0, 0]),
            Some(Frame::Forgotten)
        );
        assert_eq!(
            frame(&[0x02, 0, 0, 0, 0, 0, 0xff, 0]),
            Some(Frame::Forgotten)
        );
        assert_eq!(frame(&[0x02, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(
            frame(&long(&[0x11, 0xff, 0x04, 0x00, 0x01])),
            Some(Frame::Forgotten)
        );
    }

    #[test]
    fn replies_carry_no_input() {
        // A setRollerMode reply has software id 0x0b in its function byte.
        assert_eq!(frame(&long(&[0x11, 0xff, 0x0d, 0x3b, 0x01, 0x01])), None);
        assert_eq!(
            frame(&long(&[0x11, 0xff, 0x0a, 0x3b, 0x00, 0x53, 0x03])),
            None
        );
    }
}
