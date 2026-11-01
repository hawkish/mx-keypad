//! Presses: the grid keys' vendor frames and the page buttons' diverted
//! HID++ notifications, diffed into balanced down/up edges.
//!
//! A grid event is a 32-byte report `13 ff 02 00 [seq] 01 [id, …]`: the
//! NUL-terminated id list names every key held at that moment, `id` being the
//! 1-based grid index. A press names the key, its release names none, and a
//! chord names every held key in one frame. `seq` is a rolling counter with no
//! edge of its own. Ported from OpenLogi's `openlogi-device/src/session/keyboard.rs`.
//!
//! The page buttons only report to us once diverted on feature `0x1b04`; the
//! device then sends `11 ff [index] 00 [cid, cid, cid, cid]` naming up to four
//! held controls, the same held-set shape as the grid frame.

use std::collections::BTreeSet;

use crate::vendor::{input_payload, VendorFunction};

/// Byte 5 of a grid frame: the id list follows.
const GRID_ID_LIST: u8 = 0x01;
/// The page button under the panel's left edge.
pub(crate) const LEFT_PAGING: u16 = 0x01a1;
/// The page button under the panel's right edge.
pub(crate) const RIGHT_PAGING: u16 = 0x01a2;
/// HID++ long report ID.
const HIDPP_LONG: u8 = 0x11;

/// A physical button on the keypad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Button {
    /// A grid key by its wire index, 1–9, row-major from the top-left.
    Key(u8),
    /// The left page button.
    PageLeft,
    /// The right page button.
    PageRight,
}

/// An edge on a button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The button went down.
    Down(Button),
    /// The button came up.
    Up(Button),
}

/// The ids a grid-key vendor frame names as held, or `None` when `bytes` is
/// anything else on the node — HID++ replies, image acks, other vendor
/// traffic. Both payload selectors are pinned (`00` function, `01` id list).
fn grid_key_frame(bytes: &[u8]) -> Option<impl Iterator<Item = u8> + '_> {
    match input_payload(bytes, VendorFunction::GridKeys)? {
        [_seq, GRID_ID_LIST, ids @ ..] if !ids.is_empty() => {
            Some(ids.iter().copied().take_while(|id| *id != 0))
        }
        _ => None,
    }
}

/// The page buttons a diverted-buttons notification on feature index
/// `reprog` names as held, or `None` for any other report.
fn page_frame(bytes: &[u8], reprog: u8) -> Option<BTreeSet<Button>> {
    match bytes {
        [HIDPP_LONG, 0xff, index, 0x00, cids @ ..] if *index == reprog && cids.len() >= 8 => Some(
            cids[..8]
                .chunks(2)
                .filter_map(|cid| match u16::from_be_bytes([cid[0], cid[1]]) {
                    LEFT_PAGING => Some(Button::PageLeft),
                    RIGHT_PAGING => Some(Button::PageRight),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Which buttons are held, as of the last frame of each kind.
#[derive(Debug, Default)]
pub struct Decoder {
    keys: BTreeSet<Button>,
    pages: BTreeSet<Button>,
    /// Feature index of `0x1b04` on this device, once the page buttons are
    /// diverted; their notifications are ignored until then.
    reprog: Option<u8>,
}

impl Decoder {
    /// Decode page-button notifications on feature index `reprog` from now on.
    pub(crate) fn follow_pages(&mut self, reprog: Option<u8>) {
        self.reprog = reprog;
        if reprog.is_none() {
            self.pages.clear();
        }
    }

    /// Diff one input report into edges: releases first, then presses.
    /// Repeated frames change nothing, and reports that are neither kind of
    /// frame yield nothing.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        if let Some(ids) = grid_key_frame(bytes) {
            let held = ids
                .filter(|id| (1..=9).contains(id))
                .map(Button::Key)
                .collect();
            return diff(&mut self.keys, held);
        }
        if let Some(held) = self.reprog.and_then(|reprog| page_frame(bytes, reprog)) {
            return diff(&mut self.pages, held);
        }
        Vec::new()
    }
}

fn diff(down: &mut BTreeSet<Button>, held: BTreeSet<Button>) -> Vec<Event> {
    let mut events: Vec<Event> = down.difference(&held).copied().map(Event::Up).collect();
    events.extend(held.difference(down).copied().map(Event::Down));
    *down = held;
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid PDU as the keypad sends it: `13 ff 02 00 [seq] 01 [id, …]`
    /// padded to the 32 bytes the node carries.
    fn grid_pdu(seq: u8, ids: &[u8]) -> Vec<u8> {
        let mut pdu = vec![0x13, 0xff, 0x02, 0x00, seq, 0x01];
        pdu.extend_from_slice(ids);
        pdu.resize(32, 0);
        pdu
    }

    /// A diverted-buttons notification on feature index `0x0b`.
    fn page_pdu(cids: &[u16]) -> Vec<u8> {
        let mut pdu = vec![0x11, 0xff, 0x0b, 0x00];
        for cid in cids {
            pdu.extend_from_slice(&cid.to_be_bytes());
        }
        pdu.resize(20, 0);
        pdu
    }

    fn frame_ids(bytes: &[u8]) -> Option<Vec<u8>> {
        grid_key_frame(bytes).map(Iterator::collect)
    }

    fn feed_all(decoder: &mut Decoder, frames: &[Vec<u8>]) -> Vec<Event> {
        frames
            .iter()
            .flat_map(|frame| decoder.feed(frame))
            .collect()
    }

    #[test]
    fn grid_frames_decode_the_held_ids() {
        // Captured on the keypad: key 8 pressed, then released.
        assert_eq!(frame_ids(&grid_pdu(0xc8, &[8])), Some(vec![8]));
        assert_eq!(frame_ids(&grid_pdu(0xc9, &[])), Some(vec![]));
        // An HID++ long report sharing the node, and a truncated frame.
        assert_eq!(frame_ids(&[0x11, 0xff, 0x04, 0x1a, 0x0b, 0xb8, 0, 0]), None);
        assert_eq!(frame_ids(&[0x13, 0xff, 0x02, 0x00, 0x01]), None);
    }

    #[test]
    fn grid_frames_reject_wrong_payload_selectors() {
        // The LCD image-write ack names what would be a valid key byte.
        let mut ack = vec![0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01];
        ack.resize(32, 0);
        assert_eq!(frame_ids(&ack), None);
        // A frame with the id-list marker replaced decodes to nothing.
        assert_eq!(frame_ids(&[0x13, 0xff, 0x02, 0x00, 0x01, 0x02, 0x05]), None);
    }

    /// The live sequence: the counter wraps from `cf` to `c0` mid-capture,
    /// and a press lands on even and odd values alike.
    #[test]
    fn a_press_and_its_empty_release_frame_make_one_edge_pair() {
        let mut decoder = Decoder::default();
        let frames = [
            grid_pdu(0xce, &[8]),
            grid_pdu(0xcf, &[]),
            grid_pdu(0xc0, &[9]),
            grid_pdu(0xc1, &[]),
        ];
        assert_eq!(
            feed_all(&mut decoder, &frames),
            [
                Event::Down(Button::Key(8)),
                Event::Up(Button::Key(8)),
                Event::Down(Button::Key(9)),
                Event::Up(Button::Key(9)),
            ]
        );
    }

    #[test]
    fn repeated_frames_and_off_grid_ids_add_no_edges() {
        let mut decoder = Decoder::default();
        let frames = [
            grid_pdu(0x01, &[2]),
            grid_pdu(0x02, &[2]),
            grid_pdu(0x03, &[2, 12]),
            grid_pdu(0x04, &[]),
        ];
        assert_eq!(
            feed_all(&mut decoder, &frames),
            [Event::Down(Button::Key(2)), Event::Up(Button::Key(2))]
        );
    }

    #[test]
    fn grid_chords_emit_one_balanced_edge_pair_per_key() {
        let mut decoder = Decoder::default();
        // Press both as a chord, release 3 while 8 stays held, then release 8.
        let frames = [
            grid_pdu(0x01, &[3, 8]),
            grid_pdu(0x02, &[8]),
            grid_pdu(0x03, &[]),
        ];
        assert_eq!(
            feed_all(&mut decoder, &frames),
            [
                Event::Down(Button::Key(3)),
                Event::Down(Button::Key(8)),
                Event::Up(Button::Key(3)),
                Event::Up(Button::Key(8)),
            ]
        );
    }

    #[test]
    fn page_buttons_report_only_once_followed() {
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&page_pdu(&[LEFT_PAGING])).is_empty());

        decoder.follow_pages(Some(0x0b));
        let frames = [
            page_pdu(&[LEFT_PAGING]),
            page_pdu(&[LEFT_PAGING, RIGHT_PAGING]),
            page_pdu(&[RIGHT_PAGING]),
            page_pdu(&[]),
        ];
        assert_eq!(
            feed_all(&mut decoder, &frames),
            [
                Event::Down(Button::PageLeft),
                Event::Down(Button::PageRight),
                Event::Up(Button::PageLeft),
                Event::Up(Button::PageRight),
            ]
        );
    }

    #[test]
    fn page_notifications_on_another_feature_or_function_are_ignored() {
        let mut decoder = Decoder::default();
        decoder.follow_pages(Some(0x0b));
        let mut other_index = page_pdu(&[LEFT_PAGING]);
        other_index[2] = 0x0c;
        let mut reply = page_pdu(&[LEFT_PAGING]);
        reply[3] = 0x3b;
        assert!(decoder.feed(&other_index).is_empty());
        assert!(decoder.feed(&reply).is_empty());
    }

    #[test]
    fn grid_and_page_state_are_independent() {
        let mut decoder = Decoder::default();
        decoder.follow_pages(Some(0x0b));
        let frames = [
            grid_pdu(0x01, &[5]),
            page_pdu(&[RIGHT_PAGING]),
            grid_pdu(0x02, &[]),
        ];
        assert_eq!(
            feed_all(&mut decoder, &frames),
            [
                Event::Down(Button::Key(5)),
                Event::Down(Button::PageRight),
                Event::Up(Button::Key(5)),
            ]
        );
    }
}
