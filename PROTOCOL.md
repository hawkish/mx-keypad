# MX Creative Keypad wire protocol

These are the facts `mx-keypad` relies on. They were measured on hardware with OpenLogi's probes
(`crates/openlogi-hid/examples/mx_keypad_{probe,grid_probe,lcd_probe}.rs`, 2026-10, `046d:c354`,
macOS) and cross-checked against
`node-logitech-mx-creative-console` (MIT).

## The node

The keypad only connects over wired USB-C and speaks HID++ 4.5. Every HID node of the device
mirrors the same stream. `mx-keypad` opens the one with usage page `0xff43`, usage `0x1a02`. It
carries both HID++ and the vendor channel, and needs no handshake or keepalive. Reports on it can
be **32 bytes**, so don't truncate reads at HID++'s 20.

## Vendor channel

The vendor channel isn't HID++, although it shares the node.

| Direction | Report ID | Bytes 1–2 | Byte 3 |
|---|---|---|---|
| device → host | `0x13` | `ff 02` | function |
| host → device | `0x14` | `ff 02` | function |

Function `00` is grid keys and function `2b` is an image write and its acknowledgement.

### Grid keys

```text
13 ff 02 00 [seq] 01 [id, id, …, 00]
```

The NUL-terminated list names every key held at that moment. `id` is 1–9, row-major from the
top-left, which a mapping run pressing the keys in physical order confirmed. A press names the key
and its release names none. A chord arrives as one frame that names every held key. `seq` is a
rolling counter (`c0`…`cf`) and carries no edge of its own, so you find edges by diffing the held
sets of successive frames.

The keys report this way without any setup, and they keep their firmware function as well.

### LCD image write

The panel is 480×480. Key `n` (0-based, row-major) is the 118×118 tile at
`(23 + col·158, 6 + row·158)`. The device places a JPEG; it doesn't scale it.

```text
first packet:  14 ff 02 2b [part] 01 00 01 00 [x:2] [y:2] [w:2] [h:2] 00 [len:2] [jpeg…]
continuation:  14 ff 02 2b [part] [jpeg…]
```

- Every packet is a 4095-byte report, zero-padded.
- Fields are big-endian `u16`. The first packet carries the JPEG from byte 20, continuations from
  byte 5.
- `part` is the packet's 1-based index `| 0x20`. Add `0x80` on the first packet and `0x40` on the
  last.
- A ~1–3 KiB baseline JPEG fits in one packet.

The device acknowledges each image with `13 ff 02 2b [part & ~0x20] 00 01`. A single-packet tile
is acknowledged with `13 ff 02 2b c1 00 01`. The first write of a session has also drawn one
`13 ff 02 10 c5 a0` notification, whose meaning is unknown.

## HID++ features used

The feature indexes are looked up at runtime through `IRoot` (`0x0000`, function 0). On the
recorded device, `0x1b04` was at index 11 and `0x8040` at index 15.

### `0x1b04` ReprogControlsV4: the page buttons

| Control | CID |
|---|---|
| Left page button | `0x01a1` |
| Right page button | `0x01a2` |

- **Divert:** function 3 (setCidReporting) with `[cid:2] 0b11` (divert plus divert-valid).
- **Hand back:** function 3 with `[cid:2] 0b10` (divert-valid, value 0).
- **Notification while diverted:** `11 ff [index] 00 [cid:2]×4`, naming up to four held controls.
  It has the same held-set shape as a grid frame.

OpenLogi's first probe restored the buttons with flags `0x01`. That doesn't set the valid bit.
OpenLogi's HID++ crate and `mx-keypad` use `0b10`.

### `0x8040` BrightnessControl

| Function | Request | Reply |
|---|---|---|
| 0 (getInfo) | | `[max:2] [steps_lo] [caps] [min:2] [steps_hi]` |
| 2 (setBrightness) | `[level:2]` | |

The hardware answered getInfo with `00 64 65 10 00 00`: max 100, 101 steps, min 0, and
capabilities `TRANSIENT`, so the device doesn't keep the level after it powers off. **Never write
0.** The reference driver says it resets the device, so `mx-keypad` clamps to at least 1.
