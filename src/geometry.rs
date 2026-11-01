//! Where the MX Creative Keypad's keys sit: the grid the vendor report numbers
//! and the LCD panel the keys are tiles of.
//!
//! Ported from OpenLogi's `openlogi-device-registry/src/keypad.rs`. The panel
//! is driven over a vendor report, so no protocol feature reports this
//! geometry; it is the product's identity fact.

/// USB vendor ID of every Logitech device.
pub const VENDOR_ID: u16 = 0x046d;
/// The MX Creative Keypad — wired USB-C only, HID++ 4.5.
pub const PRODUCT_ID: u16 = 0xc354;

/// A key's cell on the grid, zero-based from the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPosition {
    /// Row, counted down from the top.
    pub row: u16,
    /// Column, counted right from the left.
    pub col: u16,
}

/// A point on the LCD panel, in panel pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelPoint {
    /// Pixels from the panel's left edge.
    pub x: u16,
    /// Pixels from the panel's top edge.
    pub y: u16,
}

/// How many rows and columns of keys a grid has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyGrid {
    /// Rows of keys.
    pub rows: u16,
    /// Columns of keys.
    pub cols: u16,
}

impl KeyGrid {
    /// Whether `key` falls on this grid.
    #[must_use]
    pub const fn contains(self, key: KeyPosition) -> bool {
        key.row < self.rows && key.col < self.cols
    }

    /// The cell of the key at zero-based `index`, counting row-major — left
    /// to right, then top to bottom — the way the keypad numbers its keys.
    #[must_use]
    pub const fn cell(self, index: u16) -> KeyPosition {
        KeyPosition {
            row: index / self.cols,
            col: index % self.cols,
        }
    }
}

/// Where the square LCD keys sit on the panel, in panel pixels.
///
/// Keys are laid out on a regular grid: the key at `(row, col)` is the
/// `key_size`-square tile whose top-left corner is
/// `origin + (col, row) · pitch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyDisplay {
    /// Side length of one key's tile.
    pub key_size: u16,
    /// Top-left corner of the top-left key.
    pub origin: PanelPoint,
    /// Distance between the top-left corners of adjacent keys.
    pub pitch: u16,
    /// The panel's key grid.
    pub grid: KeyGrid,
}

impl KeyDisplay {
    /// The top-left corner of the key at `key`, or `None` off the grid.
    #[must_use]
    pub const fn key_origin(&self, key: KeyPosition) -> Option<PanelPoint> {
        if !self.grid.contains(key) {
            return None;
        }
        Some(PanelPoint {
            x: self.origin.x + key.col * self.pitch,
            y: self.origin.y + key.row * self.pitch,
        })
    }

    /// The top-left corner of the key the vendor report calls `index`
    /// (1-based, row-major), or `None` off the grid.
    #[must_use]
    pub const fn index_origin(&self, index: u8) -> Option<PanelPoint> {
        if index == 0 {
            return None;
        }
        self.key_origin(self.grid.cell(index as u16 - 1))
    }
}

/// The keypad's key grid: three rows of three, which its vendor report numbers
/// row-major from the top-left, 1 through 9. A mapping run of OpenLogi's
/// `mx_keypad_grid_probe` (2026-10) pressed the keys in physical row-major
/// order and read `1..=9`.
pub const GRID: KeyGrid = KeyGrid { rows: 3, cols: 3 };

/// The keypad's 480×480 panel: nine 118-pixel keys, 40 pixels apart. Verified
/// on hardware 2026-10 (OpenLogi's `mx_keypad_lcd_probe`): solid tiles written
/// at these positions filled each key edge to edge, in row-major order.
pub const DISPLAY: KeyDisplay = KeyDisplay {
    key_size: 118,
    origin: PanelPoint { x: 23, y: 6 },
    pitch: 158,
    grid: GRID,
};

#[cfg(test)]
mod tests {
    use super::*;

    const fn at(row: u16, col: u16) -> KeyPosition {
        KeyPosition { row, col }
    }

    const fn point(x: u16, y: u16) -> PanelPoint {
        PanelPoint { x, y }
    }

    #[test]
    fn keys_sit_on_the_verified_panel_grid() {
        assert_eq!(DISPLAY.key_origin(at(0, 0)), Some(point(23, 6)));
        assert_eq!(DISPLAY.key_origin(at(0, 2)), Some(point(339, 6)));
        assert_eq!(DISPLAY.key_origin(at(2, 1)), Some(point(181, 322)));
        // The last key still fits the 480-pixel panel.
        let PanelPoint { x, y } = DISPLAY.key_origin(at(2, 2)).expect("bottom-right key");
        assert!(x + DISPLAY.key_size <= 480 && y + DISPLAY.key_size <= 480);
    }

    #[test]
    fn wire_indexes_count_from_one() {
        assert_eq!(DISPLAY.index_origin(1), Some(point(23, 6)));
        assert_eq!(DISPLAY.index_origin(8), Some(point(181, 322)));
        assert!(DISPLAY.index_origin(0).is_none());
        assert!(DISPLAY.index_origin(10).is_none());
    }

    #[test]
    fn key_origin_is_none_off_the_grid() {
        let display = KeyDisplay {
            key_size: 10,
            origin: point(0, 0),
            pitch: 12,
            grid: KeyGrid { rows: 2, cols: 3 },
        };
        assert_eq!(display.key_origin(at(1, 2)), Some(point(24, 12)));
        assert!(display.key_origin(at(2, 0)).is_none());
        assert!(display.key_origin(at(0, 3)).is_none());
    }

    #[test]
    fn cells_count_row_major_from_the_top_left() {
        let grid = KeyGrid { rows: 2, cols: 3 };
        assert_eq!(grid.cell(0), at(0, 0));
        assert_eq!(grid.cell(2), at(0, 2));
        assert_eq!(grid.cell(3), at(1, 0));
        assert_eq!(grid.cell(5), at(1, 2));
        assert!(!grid.contains(grid.cell(6)));
    }
}
