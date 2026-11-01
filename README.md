# mx-keypad

Drive the Logitech MX Creative Keypad from a JSON file: what each of the nine LCD keys shows,
what pressing it does, how many pages there are, and how bright the panel is. It needs no Logi
Options+ and no daemon beyond the `mx-keypad run` process itself.

`mx-keypad` is also a Rust library (`mx_keypad::Keypad`) that other tools can drive the keypad
through. It's built on blocking `hidapi` with no async runtime.

The protocol code is ported from [OpenLogi](https://github.com/aprilnea/openlogi). See
[PROTOCOL.md](PROTOCOL.md) for the wire format and [NOTICE](NOTICE) for credits.

## Install

```sh
nix profile install .   # or
cargo install --path .
```

Connect the keypad over USB. Quit Logi Options+ and OpenLogi first, since either one holds the
device. On macOS your terminal may need **Input Monitoring** permission. On Linux, `/dev/hidraw*`
needs a udev rule:

```
KERNEL=="hidraw*", ATTRS{idVendor}=="046d", ATTRS{idProduct}=="c354", MODE="0660", TAG+="uaccess"
```

## Usage

```sh
mx-keypad check --config examples/demo.json --out /tmp/tiles   # validate and render, no device needed
mx-keypad run --config examples/demo.json                       # show the setup and act on presses
mx-keypad draw 1 bell --bg '#cc0000' --label Alert              # draw one key
mx-keypad brightness 50
mx-keypad events                                                # print presses
mx-keypad icons                                                 # built-in icon names
```

`run` and `check` read `~/.config/mx-keypad/config.json` when you don't pass `--config`. `run`
reloads the file whenever it changes. If the new version doesn't validate, `run` keeps the setup
it already has and prints the error.

While `run` or `events` is going, the page buttons are taken over. They're handed back to the
firmware on exit, including on Ctrl-C and SIGTERM.

## Config

```json
{
  "brightness": 70,
  "defaults": { "background": "#000000", "foreground": "#ffffff", "glyph_scale": 0.55 },
  "pages": [
    {
      "name": "main",
      "keys": {
        "1": { "icon": "square-terminal", "label": "Shell", "action": { "run": "open -a Terminal" } },
        "2": { "icon": "./icons/claude.png", "background": "#cc785c", "action": { "run": "tmux select-window -t 2" } },
        "9": { "icon": "chevron-right", "action": { "page": "next" } }
      }
    }
  ],
  "page_buttons": { "left": { "page": "prev" }, "right": { "page": "next" } }
}
```

| Field | Meaning |
|---|---|
| `brightness` | Panel brightness, 1–100. Leave it out to keep the current level. |
| `defaults` | The `background`, `foreground` and `glyph_scale` every key inherits. |
| `pages[].name` | A unique name that `page` actions can jump to. The first page is shown at start. |
| `pages[].keys` | Keys `"1"`–`"9"`, row-major from the top left. A key you leave out is blank and does nothing. |
| `icon` | A built-in name (see `mx-keypad icons`), or a path to an `.svg`, `.png` or `.jpg` file relative to the config file. `~/` works too. SVGs that use `currentColor` take the key's `foreground`. |
| `label` | Text drawn under the icon. |
| `background`, `foreground` | `#rrggbb` |
| `glyph_scale` | How much of the key the icon spans, 0.05–1. |
| `action` | `{ "run": "<shell command>" }`, `{ "page": "next" \| "prev" \| "<name>" }`, or `{ "none": true }`. |
| `page_buttons` | The `left` and `right` page buttons' actions. They default to `prev` and `next`. |

A `run` command goes through `sh -c` and isn't waited on. To send keystrokes, use `osascript` or
`tmux send-keys`.

Mistakes are reported with where they are, for example
``pages[0].keys."10": keys are "1" to "9"``.

## Library

```rust
let api = hidapi::HidApi::new()?;
if let Some(mut keypad) = mx_keypad::Keypad::open(&api)? {
    let face = mx_keypad::render::Face { icon: Some(IconSource::Builtin("bell".into())), ..Default::default() };
    keypad.draw(5, &mx_keypad::render::render_tile(&face, 118)?)?;
    keypad.divert_page_buttons(true)?;
    for event in keypad.poll(Duration::from_millis(200))? { /* Event::Down(Button::Key(5)) … */ }
}
```

`Keypad::open` returns `Ok(None)` when no keypad is plugged in. It borrows your `HidApi` because
hidapi allows only one per process.

## Development

```sh
nix develop -c cargo test
nix develop -c cargo clippy --all-targets -- -D warnings
```
