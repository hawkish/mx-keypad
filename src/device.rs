//! An open MX Creative Keypad: draw keys, set brightness, read presses.
//!
//! Everything goes through one HID node, `ff43:1a02`. OpenLogi's probes found
//! that every node of the device mirrors the same stream and that the vendor
//! channel and HID++ both work on this one, with no handshake or keepalive.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hidapi::{HidApi, HidDevice};

use crate::geometry::{PanelPoint, DISPLAY, PRODUCT_ID, VENDOR_ID};
use crate::hidpp::{self, Link, BRIGHTNESS_CONTROL, REPROG_CONTROLS_V4};
use crate::image::{ack_part, image_ack, image_packets, KeyImageJpeg, ACK_TIMEOUT};
use crate::input::{Decoder, Event, LEFT_PAGING, RIGHT_PAGING};
use crate::render;

/// The vendor node's usage page and usage.
const USAGE_PAGE: u16 = 0xff43;
const USAGE: u16 = 0x1a02;
/// Largest report the node sends: a grid frame is 32 bytes, HID++ 20.
const READ_LEN: usize = 64;

/// An open keypad.
pub struct Keypad {
    handle: HidDevice,
    decoder: Decoder,
    /// Edges decoded while waiting on a reply, not yet handed out.
    pending: Vec<Event>,
    /// Index of `0x1b04` while the page buttons are diverted.
    diverted: Option<u8>,
}

impl Keypad {
    /// Open the keypad, or `Ok(None)` when none is plugged in. Takes the
    /// caller's `HidApi`, since hidapi allows one per process.
    pub fn open(api: &HidApi) -> Result<Option<Keypad>> {
        let Some(info) = api.device_list().find(|d| {
            d.vendor_id() == VENDOR_ID
                && d.product_id() == PRODUCT_ID
                && d.usage_page() == USAGE_PAGE
                && d.usage() == USAGE
        }) else {
            return Ok(None);
        };
        let handle = api.open_path(info.path()).context(
            "cannot open the MX Creative Keypad (is Logi Options+ or OpenLogi holding it?)",
        )?;
        Ok(Some(Keypad {
            handle,
            decoder: Decoder::default(),
            pending: Vec::new(),
            diverted: None,
        }))
    }

    /// Draw `image` on key `index` (1–9, row-major from the top-left) and
    /// wait for the keypad to take it. `image` must be encoded at
    /// [`DISPLAY`]`.key_size` square: the device places it, it does not scale.
    pub fn draw(&mut self, index: u8, image: &KeyImageJpeg) -> Result<()> {
        let origin = DISPLAY
            .index_origin(index)
            .ok_or_else(|| anyhow!("key {index} is not on the keypad (1–9)"))?;
        let mut stray = Vec::new();
        let result = write_image(&self.handle, origin, image, &mut stray);
        self.absorb(stray);
        result
    }

    /// Blank all nine keys. The panel keeps whatever was drawn last, so a
    /// program that stops answering presses should clear it.
    pub fn clear(&mut self) -> Result<()> {
        let blank = render::render_tile(&render::Face::default(), DISPLAY.key_size)?;
        for index in 1..=DISPLAY.grid.rows * DISPLAY.grid.cols {
            self.draw(u8::try_from(index)?, &blank)?;
        }
        Ok(())
    }

    /// Set the panel brightness to `percent` of its range, 1–100. Never 0:
    /// the reference driver warns that it resets the device.
    pub fn set_brightness(&mut self, percent: u8) -> Result<()> {
        let mut stray = Vec::new();
        let result: Result<()> = (|| {
            let index = hidpp::feature_index(&self.handle, BRIGHTNESS_CONTROL, &mut stray)?
                .context("this keypad has no brightness control")?;
            let (min, max) = hidpp::brightness_range(&self.handle, index, &mut stray)?;
            let level = scaled_level(percent, min, max);
            hidpp::set_brightness(&self.handle, index, level, &mut stray)?;
            Ok(())
        })();
        self.absorb(stray);
        result
    }

    /// Take the page buttons over (`true`), so they report as
    /// [`Button::PageLeft`](crate::Button::PageLeft) and
    /// [`Button::PageRight`](crate::Button::PageRight), or hand them back to
    /// the firmware. Dropping the keypad hands them back.
    pub fn divert_page_buttons(&mut self, divert: bool) -> Result<()> {
        let mut stray = Vec::new();
        let result: Result<u8> = (|| {
            let index = hidpp::feature_index(&self.handle, REPROG_CONTROLS_V4, &mut stray)?
                .context("this keypad has no reprogrammable controls")?;
            for cid in [LEFT_PAGING, RIGHT_PAGING] {
                hidpp::set_cid_reporting(&self.handle, index, cid, divert, &mut stray)?;
            }
            Ok(index)
        })();
        self.absorb(stray);
        let index = result?;
        self.diverted = divert.then_some(index);
        self.decoder.follow_pages(self.diverted);
        Ok(())
    }

    /// Wait up to `timeout` for presses and return their edges; empty when
    /// nothing was pressed.
    pub fn poll(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if !self.pending.is_empty() {
            return Ok(std::mem::take(&mut self.pending));
        }
        let mut buf = [0u8; READ_LEN];
        let mut wait = timeout;
        let mut events = Vec::new();
        loop {
            let n =
                Link::read(&self.handle, &mut buf, wait).map_err(|e| anyhow!("HID read: {e}"))?;
            if n == 0 {
                return Ok(events);
            }
            events.extend(self.decoder.feed(&buf[..n]));
            // Drain whatever else is queued, without waiting again.
            wait = Duration::ZERO;
        }
    }

    fn absorb(&mut self, stray: Vec<Vec<u8>>) {
        for report in stray {
            let events = self.decoder.feed(&report);
            self.pending.extend(events);
        }
    }
}

impl Drop for Keypad {
    fn drop(&mut self) {
        if self.diverted.is_some() {
            let _ = self.divert_page_buttons(false);
        }
    }
}

/// `percent` of the advertised range, never below 1.
fn scaled_level(percent: u8, min: u16, max: u16) -> u16 {
    let percent = u32::from(percent.clamp(1, 100));
    let level = u32::from(min) + (u32::from(max.saturating_sub(min)) * percent).div_ceil(100);
    u16::try_from(level)
        .unwrap_or(max)
        .clamp(min.max(1), max.max(1))
}

/// Send `image`'s packets for the tile at `origin` and wait for the ack.
/// Reports that are not the ack go to `stray`.
fn write_image(
    link: &impl Link,
    origin: PanelPoint,
    image: &KeyImageJpeg,
    stray: &mut Vec<Vec<u8>>,
) -> Result<()> {
    let packets = image_packets(origin, DISPLAY.key_size, image);
    let part = ack_part(&packets);
    for packet in &packets {
        link.write(packet).map_err(|e| anyhow!("HID write: {e}"))?;
    }
    let deadline = std::time::Instant::now() + ACK_TIMEOUT;
    let mut buf = [0u8; READ_LEN];
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        let n = link
            .read(&mut buf, left)
            .map_err(|e| anyhow!("HID read: {e}"))?;
        if n == 0 {
            continue;
        }
        match image_ack(&buf[..n], part) {
            Some(Ok(())) => return Ok(()),
            Some(Err([high, low])) => {
                bail!("keypad refused the key image: status {high:02x} {low:02x}")
            }
            None => stray.push(buf[..n].to_vec()),
        }
    }
    bail!("keypad did not acknowledge the key image within 2 s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::scripted::Scripted;

    fn jpeg(len: usize) -> KeyImageJpeg {
        KeyImageJpeg::new(vec![0xab; len]).unwrap()
    }

    fn ack(status: [u8; 2]) -> Vec<u8> {
        vec![0x13, 0xff, 0x02, 0x2b, 0xc1, status[0], status[1]]
    }

    #[test]
    fn a_written_tile_completes_on_its_ack_and_keeps_presses() {
        let press = vec![0x13, 0xff, 0x02, 0x00, 0xc8, 0x01, 0x08];
        let link = Scripted::new(|_: &[u8]| vec![press.clone(), ack([0x00, 0x01])]);
        let mut stray = Vec::new();
        write_image(
            &link,
            DISPLAY.index_origin(8).unwrap(),
            &jpeg(900),
            &mut stray,
        )
        .unwrap();
        assert_eq!(stray.len(), 1);
        assert_eq!(stray[0], press);
        assert_eq!(link.written.borrow()[0][9..13], [0x00, 0xb5, 0x01, 0x42]);
    }

    #[test]
    fn a_refused_tile_fails() {
        let link = Scripted::new(|_: &[u8]| vec![ack([0x01, 0x00])]);
        let error = write_image(&link, DISPLAY.origin, &jpeg(900), &mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("01 00"));
    }

    #[test]
    fn brightness_never_scales_to_zero() {
        assert_eq!(scaled_level(0, 0, 100), 1);
        assert_eq!(scaled_level(1, 0, 100), 1);
        assert_eq!(scaled_level(50, 0, 100), 50);
        assert_eq!(scaled_level(100, 0, 100), 100);
        assert_eq!(scaled_level(200, 0, 100), 100);
        assert_eq!(scaled_level(50, 10, 20), 15);
    }
}
