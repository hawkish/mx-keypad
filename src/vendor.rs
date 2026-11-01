//! The keypad's vendor channel: the framing every report on it shares, owned
//! once for the grid-key decoder and the LCD image write.
//!
//! The channel rides the keypad's HID++ node but is not HID++: the device
//! sends input report `0x13` and accepts output report `0x14`, and bytes 1–2
//! of both name the channel (`ff 02`). Byte 3 selects the function — `00` for
//! grid-key frames, `2b` for an image write and its acknowledgement — and the
//! function's payload follows. Ported from OpenLogi's
//! `openlogi-device/src/keypad_vendor.rs`; hardware-verified there 2026-10.

/// Report ID the device sends vendor-channel traffic on.
const INPUT_REPORT_ID: u8 = 0x13;
/// Report ID of a vendor-channel write.
const OUTPUT_REPORT_ID: u8 = 0x14;
/// Bytes 1–2 of every vendor-channel report.
const CHANNEL: [u8; 2] = [0xff, 0x02];

/// A function on the vendor channel, byte 3 of its reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum VendorFunction {
    /// Grid-key frames: which keys are held now.
    GridKeys = 0x00,
    /// An LCD image write, and the device's acknowledgement of one.
    Image = 0x2b,
}

/// The payload after the function byte of `report`, when it is a
/// vendor-channel input report for `function`; `None` for anything else on
/// the node (HID++ replies, other functions, a truncated header).
pub(crate) fn input_payload(report: &[u8], function: VendorFunction) -> Option<&[u8]> {
    match report {
        [INPUT_REPORT_ID, c0, c1, byte, payload @ ..]
            if [*c0, *c1] == CHANNEL && *byte == function as u8 =>
        {
            Some(payload)
        }
        _ => None,
    }
}

/// The header every vendor-channel write for `function` starts with: report
/// ID, channel, function.
pub(crate) const fn output_header(function: VendorFunction) -> [u8; 4] {
    [OUTPUT_REPORT_ID, CHANNEL[0], CHANNEL[1], function as u8]
}

#[cfg(test)]
mod tests {
    use super::{input_payload, output_header, VendorFunction};

    #[test]
    fn input_payload_selects_the_function_on_the_vendor_channel() {
        // A grid frame and an image ack as captured on the keypad.
        let grid = [0x13, 0xff, 0x02, 0x00, 0xc8, 0x01, 0x08];
        let ack = [0x13, 0xff, 0x02, 0x2b, 0xc1, 0x00, 0x01];
        assert_eq!(
            input_payload(&grid, VendorFunction::GridKeys),
            Some(&grid[4..])
        );
        assert_eq!(input_payload(&ack, VendorFunction::Image), Some(&ack[4..]));
        // Each is foreign to the other function.
        assert_eq!(input_payload(&grid, VendorFunction::Image), None);
        assert_eq!(input_payload(&ack, VendorFunction::GridKeys), None);
    }

    #[test]
    fn input_payload_rejects_other_reports_on_the_node() {
        // An HID++ long report, the write's own report ID echoed back, another
        // vendor channel, and a header cut short.
        assert_eq!(
            input_payload(&[0x11, 0xff, 0x02, 0x2b, 0x00], VendorFunction::Image),
            None
        );
        assert_eq!(
            input_payload(&[0x14, 0xff, 0x02, 0x2b, 0x00], VendorFunction::Image),
            None
        );
        assert_eq!(
            input_payload(&[0x13, 0xff, 0x03, 0x2b, 0x00], VendorFunction::Image),
            None
        );
        assert_eq!(
            input_payload(&[0x13, 0xff, 0x02], VendorFunction::GridKeys),
            None
        );
        // A bare header is a frame with an empty payload, not a rejection.
        assert_eq!(
            input_payload(&[0x13, 0xff, 0x02, 0x00], VendorFunction::GridKeys),
            Some(&[][..])
        );
    }

    #[test]
    fn output_header_frames_a_write() {
        assert_eq!(
            output_header(VendorFunction::Image),
            [0x14, 0xff, 0x02, 0x2b]
        );
    }
}
