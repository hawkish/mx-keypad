//! Images on the LCD keys — the keypad's vendor image write.
//!
//! The keys are tiles of one panel, and no HID++ feature drives it: an image
//! goes out as one or more 4095-byte output reports `0x14` on the vendor
//! channel (`ff 02`) that also carries the grid-key presses. The first packet
//! is
//!
//! ```text
//! 14 ff 02 2b [part] 01 00 01 00 [x] [y] [w] [h] 00 [len] [jpeg…]
//! ```
//!
//! with big-endian `u16` fields and the JPEG from byte 20; continuation
//! packets carry the rest from byte 5. `part` is the packet's 1-based index
//! `| 0x20`, `| 0x80` on the first packet and `| 0x40` on the last. The device
//! acknowledges the image with `13 ff 02 2b [part] [status…]`: the last
//! packet's part byte without its `0x20` marker, then `00 01` when it took the
//! image.
//!
//! Ported from OpenLogi's `openlogi-device/src/write/key_display.rs`; the
//! layout follows `node-logitech-mx-creative-console` (MIT).

use std::time::Duration;

use crate::geometry::PanelPoint;
use crate::vendor::{input_payload, output_header, VendorFunction};

/// Bytes 0–3 of every image packet.
const IMAGE_HEADER: [u8; 4] = output_header(VendorFunction::Image);
/// Every image packet is a full-width report.
pub(crate) const PACKET_LEN: usize = 4095;
const FIRST_HEADER_LEN: usize = 20;
/// The largest JPEG that fits in the first packet alone.
pub const SINGLE_PACKET_JPEG: usize = PACKET_LEN - FIRST_HEADER_LEN;
const NEXT_HEADER_LEN: usize = 5;
/// Marks every part byte; the acknowledgement echoes the part without it.
const PART_MARKER: u8 = 0x20;
/// The status bytes after the echoed part when the device took the image —
/// the only status the hardware has been seen to send.
const ACK_ACCEPTED: [u8; 2] = [0x00, 0x01];
/// How long the device may take to acknowledge an image after its last
/// packet. Acknowledgements arrive within milliseconds on hardware.
pub(crate) const ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// A baseline JPEG small enough for the image write's `u16` length field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyImageJpeg {
    bytes: Vec<u8>,
    /// `bytes.len()`, as the write declares it.
    len: u16,
}

impl KeyImageJpeg {
    /// Wrap encoded JPEG bytes, or `None` when they are empty or longer than
    /// the write can declare (`u16::MAX`).
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Option<Self> {
        let len = u16::try_from(bytes.len()).ok().filter(|&len| len > 0)?;
        Some(Self { bytes, len })
    }

    /// The encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The length field of the image write's first packet.
    fn declared_len(&self) -> u16 {
        self.len
    }
}

/// The device's answer in `report` to an image write: `Ok` when it took the
/// image, `Err` with the status it sent otherwise, and `None` when `report`
/// is not an image acknowledgement.
///
/// The part byte is only verified for single-packet writes; a split write
/// went unanswered under that check on hardware, so any image ack counts.
/// Writes go out one at a time, so the ack can only be this write's.
pub(crate) fn image_ack(report: &[u8]) -> Option<Result<(), [u8; 2]>> {
    match input_payload(report, VendorFunction::Image)? {
        [_part, high, low, ..] => {
            let status = [*high, *low];
            Some(if status == ACK_ACCEPTED {
                Ok(())
            } else {
                Err(status)
            })
        }
        _ => None,
    }
}

/// The part byte of packet `index` (1-based) of a write of `count` packets.
fn part_byte(index: u8, count: usize) -> u8 {
    let mut byte = index | PART_MARKER;
    if index == 1 {
        byte |= 0x80;
    }
    if usize::from(index) == count {
        byte |= 0x40;
    }
    byte
}

fn packet_header(part: u8) -> [u8; 5] {
    let [id, channel_hi, channel_lo, function] = IMAGE_HEADER;
    [id, channel_hi, channel_lo, function, part]
}

/// Split `image` into the vendor write's packets for a `size`-square tile at
/// `origin`. Always at least one packet.
pub(crate) fn image_packets(origin: PanelPoint, size: u16, image: &KeyImageJpeg) -> Vec<Vec<u8>> {
    let jpeg = image.as_bytes();
    let first_len = jpeg.len().min(PACKET_LEN - FIRST_HEADER_LEN);
    let count = 1 + (jpeg.len() - first_len).div_ceil(PACKET_LEN - NEXT_HEADER_LEN);

    let mut first = vec![0u8; PACKET_LEN];
    first[..5].copy_from_slice(&packet_header(part_byte(1, count)));
    first[5..7].copy_from_slice(&0x0100u16.to_be_bytes());
    first[7..9].copy_from_slice(&0x0100u16.to_be_bytes());
    first[9..11].copy_from_slice(&origin.x.to_be_bytes());
    first[11..13].copy_from_slice(&origin.y.to_be_bytes());
    first[13..15].copy_from_slice(&size.to_be_bytes());
    first[15..17].copy_from_slice(&size.to_be_bytes());
    first[18..20].copy_from_slice(&image.declared_len().to_be_bytes());
    first[FIRST_HEADER_LEN..FIRST_HEADER_LEN + first_len].copy_from_slice(&jpeg[..first_len]);

    let mut packets = vec![first];
    let rest = jpeg[first_len..].chunks(PACKET_LEN - NEXT_HEADER_LEN);
    for (index, chunk) in (2u8..).zip(rest) {
        let mut packet = vec![0u8; PACKET_LEN];
        packet[..NEXT_HEADER_LEN].copy_from_slice(&packet_header(part_byte(index, count)));
        packet[NEXT_HEADER_LEN..NEXT_HEADER_LEN + chunk.len()].copy_from_slice(chunk);
        packets.push(packet);
    }
    packets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::DISPLAY;

    fn jpeg(len: usize) -> KeyImageJpeg {
        // Distinct bytes, so a misplaced chunk cannot pass for a correct one.
        let bytes = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        KeyImageJpeg::new(bytes).unwrap()
    }

    /// The ack the hardware sent for a single-packet tile.
    fn hardware_ack() -> Vec<u8> {
        let mut ack = vec![0u8; 32];
        ack[..7].copy_from_slice(&[0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01]);
        ack
    }

    /// Byte-for-byte the first header `mx_keypad_lcd_probe` wrote for key 1
    /// with a 1024-byte JPEG — the write the panel drew correctly.
    #[test]
    fn a_single_packet_tile_matches_the_hardware_verified_header() {
        let origin = DISPLAY.index_origin(1).unwrap();
        let packets = image_packets(origin, DISPLAY.key_size, &jpeg(1024));

        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), PACKET_LEN);
        assert_eq!(
            packets[0][..FIRST_HEADER_LEN],
            [
                0x14, 0xff, 0x02, 0x2b, 0xe1, 0x01, 0x00, 0x01, 0x00, 0x00, 0x17, 0x00, 0x06, 0x00,
                0x76, 0x00, 0x76, 0x00, 0x04, 0x00,
            ]
        );
        assert_eq!(
            packets[0][FIRST_HEADER_LEN..FIRST_HEADER_LEN + 1024],
            *jpeg(1024).as_bytes()
        );
        assert!(packets[0][FIRST_HEADER_LEN + 1024..]
            .iter()
            .all(|b| *b == 0));
    }

    #[test]
    fn a_large_image_continues_in_marked_packets_that_carry_the_rest() {
        let first_room = PACKET_LEN - FIRST_HEADER_LEN;
        let next_room = PACKET_LEN - NEXT_HEADER_LEN;
        let len = first_room + next_room + 10;
        let image = jpeg(len);
        let packets = image_packets(PanelPoint { x: 181, y: 164 }, 118, &image);

        assert_eq!(packets.len(), 3);
        // First: index 1, first marker, not last; declares the whole length.
        assert_eq!(packets[0][4], 0xa1);
        assert_eq!(
            packets[0][18..20],
            u16::try_from(len).unwrap().to_be_bytes()
        );
        // Middle: index 2, neither marker. Last: index 3, last marker.
        assert_eq!(packets[1][..5], [0x14, 0xff, 0x02, 0x2b, 0x22]);
        assert_eq!(packets[2][..5], [0x14, 0xff, 0x02, 0x2b, 0x63]);
        let carried: Vec<u8> = packets[0][FIRST_HEADER_LEN..]
            .iter()
            .chain(&packets[1][NEXT_HEADER_LEN..])
            .chain(&packets[2][NEXT_HEADER_LEN..NEXT_HEADER_LEN + 10])
            .copied()
            .collect();
        assert_eq!(carried, image.as_bytes());
    }

    #[test]
    fn an_image_exactly_filling_the_first_packet_is_one_packet() {
        let packets = image_packets(
            PanelPoint { x: 0, y: 0 },
            118,
            &jpeg(PACKET_LEN - FIRST_HEADER_LEN),
        );
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0][4], 0xe1);
    }

    #[test]
    fn bottom_middle_lands_at_its_panel_position() {
        let origin = DISPLAY.index_origin(8).unwrap();
        let packets = image_packets(origin, DISPLAY.key_size, &jpeg(900));
        // Row 2, column 1: (23 + 158, 6 + 2·158).
        assert_eq!(packets[0][9..13], [0x00, 0xb5, 0x01, 0x42]);
    }

    #[test]
    fn key_image_jpeg_rejects_what_the_length_field_cannot_declare() {
        assert!(KeyImageJpeg::new(Vec::new()).is_none());
        assert!(KeyImageJpeg::new(vec![0; usize::from(u16::MAX) + 1]).is_none());
        assert!(KeyImageJpeg::new(vec![0; usize::from(u16::MAX)]).is_some());
    }

    #[test]
    fn only_a_vendor_image_report_counts_as_an_ack() {
        assert_eq!(image_ack(&hardware_ack()), Some(Ok(())));
        // Any part counts: a split write's ack was not seen to echo it.
        let mut other_part = hardware_ack();
        other_part[4] = 0x43;
        assert_eq!(image_ack(&other_part), Some(Ok(())));
        // A grid-key press shares the channel, not the function.
        assert_eq!(image_ack(&[0x13, 0xff, 0x02, 0x00, 0xc1, 0x00, 0x01]), None);
        // A HID++ long reply with the same leading bytes is another report.
        assert_eq!(image_ack(&[0x11, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01]), None);
        assert_eq!(image_ack(&[0x13, 0xff, 0x02, 0x2b, 0xc1]), None);
        assert_eq!(image_ack(&[0x13, 0xff]), None);
    }

    #[test]
    fn an_ack_with_any_other_status_is_a_refusal() {
        let mut ack = hardware_ack();
        ack[5..7].copy_from_slice(&[0x00, 0x00]);
        assert_eq!(image_ack(&ack), Some(Err([0x00, 0x00])));
    }
}
