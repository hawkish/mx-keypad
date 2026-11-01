# mx-keypad

Control the Logitech MX Creative Keypad over USB with a JSON configuration. Set images and actions
for its nine 118 × 118 LCD keys, assign the two page buttons, and adjust panel brightness.

The Rust library exposes the same device operations without an async runtime. A planned use is to
show AI-agent status from tmux-legion; that integration isn't built yet.

## Install and check a configuration

From a checkout, install with Nix or Cargo:

```sh
nix profile install .
# Or, with Rust and Cargo installed:
cargo install --path .
```

Validate the included example and render its tiles without connecting a keypad:

```sh
mx-keypad check --config examples/demo.json
```

```text
examples/demo.json: 2 page(s), every tile renders
```

The example's actions use macOS commands. Change them before running the configuration on Linux.

## Connect the keypad

Connect the keypad over USB-C and quit Logi Options+ and OpenLogi before opening the device. On
macOS, your terminal may need **Input Monitoring** permission. On Linux, grant access to the
device's hidraw node with a udev rule, for example in `/etc/udev/rules.d/99-mx-keypad.rules`:

```text
KERNEL=="hidraw*", ATTRS{idVendor}=="046d", ATTRS{idProduct}=="c354", MODE="0660", TAG+="uaccess"
```

Reload your udev rules and reconnect the keypad. The rule uses `uaccess` to grant an active local
session access; your system's device access policy may require a different rule.

## Use the CLI

Run a configuration on the keypad. Press Ctrl-C to stop:

```sh
mx-keypad run --config examples/demo.json
```

`run` shows the first page, executes actions on press, and checks for configuration changes. If a
changed file fails validation, it reports the error and keeps the previous configuration. If the
keypad isn't connected, or is unplugged while running, `run` waits and reconnects automatically
instead of exiting. Both `run` and `events` take over the page buttons while active and attempt to
return them to the firmware on exit, including Ctrl-C and SIGTERM. When `run` ends a session, it
also attempts to blank all nine keys.

## Start at login on macOS

Install `run` as a per-user LaunchAgent. Pass the path to your configuration, or omit `--config`
to use the default path described below:

```sh
~/.nix-profile/bin/mx-keypad install --config "$HOME/.config/mx-keypad/config.json"
```

Replace the executable path if you installed with Cargo or use a different Nix profile. Install
validates the configuration, then starts the agent. The agent starts at login and restarts if it
exits, with a 10-second throttle interval. Its label is `dk.mortenhogh.mx-keypad`, its property
list is `~/Library/LaunchAgents/dk.mortenhogh.mx-keypad.plist`, and its output goes to
`~/Library/Logs/mx-keypad.log`.

The agent records the executable path used to start `install` and the resolved configuration path.
Use a stable path such as a Nix profile or `~/git/mx-keypad/result/bin/mx-keypad` after `nix build`.
Install warns if the executable path is in `/nix/store`, because garbage collection can remove that
path. Run `install` again after moving the configuration.

For keystroke actions, the LaunchAgent lets macOS ask Accessibility permission for mx-keypad
instead of a terminal or tmux server. See [Keystroke actions](#keystroke-actions-macos). To stop
and remove the agent, run:

```sh
mx-keypad uninstall
```

## Use the other commands

Use the other commands to check a configuration, draw a tile, or inspect input:

```sh
mx-keypad check --config examples/demo.json --out /tmp/tiles
mx-keypad draw 1 bell --bg '#cc0000' --fg '#ffffff' --label Alert
mx-keypad brightness 50
mx-keypad events
mx-keypad icons
```

`check` renders every key on every page without a device; `--out` saves them as
`<page>-<key>.jpg`. `draw` accepts a key number and a built-in icon name or image path; its
background and foreground default to black and white. `brightness` accepts 1–100. `events` prints
key and page-button press and release events, and `icons` lists the built-in names. `draw`,
`brightness`, and `events` need the keypad; `check` and `icons` don't.

When `--config` is omitted, `run`, `check`, and `install` use
`$XDG_CONFIG_HOME/mx-keypad/config.json` if `XDG_CONFIG_HOME` is absolute. Otherwise, they use
`~/.config/mx-keypad/config.json`.

## Configure pages and keys

This example uses one page and three keys. Save it as a JSON file and pass its path to `check` or
`run`:

```json
{
  "brightness": 70,
  "defaults": { "background": "#000000", "foreground": "#ffffff", "glyph_scale": 0.55 },
  "pages": [
    {
      "name": "main",
      "keys": {
        "1": { "icon": "bell", "label": "Alert", "action": { "run": "printf 'pressed\\n' >> /tmp/mx-keypad.log" } },
        "2": { "icon": "copy", "label": "Copy", "action": { "keys": "cmd+c" } },
        "3": { "icon": "keyboard", "label": "Hello", "action": { "text": "Hello!" } }
      }
    }
  ],
  "page_buttons": { "left": { "page": "prev" }, "right": { "page": "next" } }
}
```

| Field | Description |
| --- | --- |
| `brightness` | Optional integer from 1 to 100. If omitted, leave the current brightness unchanged. |
| `defaults.background` | Default key background as six hexadecimal digits, with or without `#`. Default: `#000000`. |
| `defaults.foreground` | Default icon and label color in the same format. Default: `#ffffff`. |
| `defaults.glyph_scale` | Default icon size as a fraction of the key side. Default: `0.55`; use `0.05`–`1.0`. |
| `pages` | Required array of at least one page. The first page is shown at startup. |
| `pages[].name` | Required, unique page name. Use it as a page action's target. |
| `pages[].keys` | Optional object mapping key numbers `"1"`–`"9"` to key settings, in row-major order from the top left. Omitted keys have the default background and no icon, label, or action. |
| `pages[].keys.<key>.icon` | Optional built-in name from `mx-keypad icons`, or a path to an `.svg`, `.png`, `.jpg`, or `.jpeg` file. Relative paths resolve from the config file's directory; `~/` resolves from `HOME`. SVG `currentColor` uses the foreground color. |
| `pages[].keys.<key>.label` | Optional text drawn near the bottom of the key. Labels use system fonts. |
| `pages[].keys.<key>.background` | Optional background color; overrides `defaults.background`. |
| `pages[].keys.<key>.foreground` | Optional icon and label color; overrides `defaults.foreground`. PNG and JPEG icons keep their own colors. |
| `pages[].keys.<key>.glyph_scale` | Optional icon size, from `0.05` to `1.0`; overrides `defaults.glyph_scale`. |
| `pages[].keys.<key>.action` | Optional action: `"none"`, `{ "run": "command" }`, `{ "page": "next" }`, `{ "page": "prev" }`, `{ "page": "name" }`, `{ "keys": "chord" }`, or `{ "text": "string" }`. Defaults to `"none"`. |
| `page_buttons.left` | Optional action for the left page button. Default: `{ "page": "prev" }`. |
| `page_buttons.right` | Optional action for the right page button. Default: `{ "page": "next" }`. |

`next` and `prev` wrap around the page list. A named page target must exist; `next` and `prev`
are reserved as navigation targets. Run commands and text actions must not be empty. Commands start
through `sh -c` without blocking input, so only use configurations and commands you trust. Unknown
configuration fields are rejected.

### Keystroke actions (macOS)

`{ "keys": "cmd+shift+4" }` presses a shortcut, and `{ "text": "hello" }` types a string by
posting keystrokes directly. Sending either action requires macOS. Other platforms can validate
these actions, but can't send them.

A chord has zero or more modifiers and exactly one key, joined by `+`. Names are case-insensitive.
Modifiers are `cmd`/`command`/`super`, `ctrl`/`control`, `alt`/`opt`/`option`, and `shift`.

Keys include letters, digits, `f1`–`f12`, and these names:

- Punctuation: `-`/`minus`, `=`/`equal`, `[`, `]`, `;`/`semicolon`, `'`/`quote`, `\`/`backslash`,
  `,`/`comma`, `.`/`period`, `/`/`slash`, and `` ` ``/`grave`
- Editing and navigation: `return`/`enter`, `tab`, `space`, `delete`/`backspace`, `escape`/`esc`,
  `forwarddelete`, `home`, `end`, `pageup`, `pagedown`, `left`, `right`, `up`, and `down`
- `plus`: the `=` key; on a US ANSI keyboard, add `shift` to produce `+`

Key codes use the US ANSI layout; text is sent as Unicode and doesn't depend on the keyboard
layout.

macOS requires Accessibility permission for the process responsible for mx-keypad. If the startup
configuration has a `keys` or `text` action, `run` requests the system prompt and prints a reminder
when it isn't trusted. The actions fail until permission is granted under **System Settings →
Privacy & Security → Accessibility**. When started from a terminal, grant permission to the
terminal app; inside tmux, grant it to the tmux server. Because tmux runs as a separate server,
granting permission to the terminal alone doesn't help. Use the LaunchAgent to have macOS request
permission for mx-keypad itself. Rebuilding the binary may cause macOS to ask again.

## Use the library

Add the local crate and `hidapi` to your project's `Cargo.toml`. Replace the path with the path to
your checkout:

```toml
[dependencies]
mx-keypad = { path = "/path/to/mx-keypad" }
hidapi = { version = "2", default-features = false }
```

Render the built-in bell icon and draw it on key 5:

```rust
use mx_keypad::geometry::DISPLAY;
use mx_keypad::render::{render_tile, Face, IconSource};
use mx_keypad::Keypad;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api = hidapi::HidApi::new()?;
    if let Some(mut keypad) = Keypad::open(&api)? {
        let face = Face {
            icon: Some(IconSource::Builtin("bell".into())),
            ..Face::default()
        };
        let tile = render_tile(&face, DISPLAY.key_size)?;
        keypad.draw(5, &tile)?;
    }
    Ok(())
}
```

`Keypad::open` returns `Ok(None)` when no matching device is found. `Keypad` also provides
`set_brightness`, `divert_page_buttons`, and `poll`; dropping it attempts to restore diverted page
buttons. The `mx_keypad::keys` module posts shortcuts and typed text on macOS, and reports whether
the process is trusted for Accessibility.

## Develop

Use the Nix development shell to run the tests and Clippy:

```sh
nix develop -c cargo test
nix develop -c cargo clippy --all-targets -- -D warnings
```

The protocol code is ported from OpenLogi. See [NOTICE](NOTICE) for attribution and licenses and
[PROTOCOL.md](PROTOCOL.md) for the device protocol. mx-keypad is licensed under MIT or Apache-2.0.
