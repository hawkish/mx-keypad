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

`run` shows the first page, responds to presses, and checks for configuration changes. If a
changed file fails validation, it reports the error and keeps the previous configuration. If the
keypad isn't connected, or is unplugged while running, `run` waits and reconnects automatically
instead of exiting. If a session fails quickly after reconnecting, `run` backs off before trying
again, doubling the delay up to 32 seconds. A session that stays up for 30 seconds resets the
backoff.

`run` takes over the page buttons when the configuration has multiple pages or binds either button
to an action other than `page` or `none`. Otherwise, the buttons keep their firmware behavior.
`events` always takes them over. Both commands attempt to return diverted buttons to the firmware on
exit, including Ctrl-C and SIGTERM. When `run` ends a session, it also attempts to blank all nine
keys.

`run` caches rendered tiles and skips drawing keys whose images haven't changed. A refused image is
retried before reconnecting. On macOS, `run` empties the agent log at startup if it's over 1 MiB.

## Start at login on macOS

Install `run` as a per-user LaunchAgent. Pass the path to your configuration, or omit `--config`
to use the default path described below:

```sh
mx-keypad install --config "$HOME/.config/mx-keypad/config.json"
```

Install validates the configuration, then copies the running executable to
`~/Library/Application Support/mx-keypad/mx-keypad`, signs it if needed, and starts the agent
from that stable path. The agent starts at login and restarts if it exits, with a 10-second
throttle interval. Its label is `dk.mortenhogh.mx-keypad`, its property list is
`~/Library/LaunchAgents/dk.mortenhogh.mx-keypad.plist`, and its output goes to
`~/Library/Logs/mx-keypad.log`.

By default, install preserves a valid Developer ID signature. Otherwise, it signs the copy with the
first "Apple Development" identity in your keychain, falling back to the first "Developer ID
Application" identity, or an ad hoc signature if neither exists. Pass `--identity` with a SHA-1
hash or name from `security find-identity -v -p codesigning` to choose a different one. Reinstalling
with the same identity keeps the same signature, so macOS
remembers the agent's Accessibility permission across rebuilds; an ad hoc signature asks again
after every install. `uninstall` removes the installed copy along with the agent.

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
mx-keypad resolve 'cmd+['
```

`check` renders every key on every page without a device; `--out` saves them as
`<page>-<key>.jpg`. `draw` accepts a key number and a built-in icon name or image path; its
background and foreground default to black and white. `brightness` accepts 1–100. `events` prints
key and page-button press and release events, and `icons` lists the built-in names. `draw`,
`brightness`, and `events` need the keypad; `check`, `icons`, and `resolve` don't. `resolve` prints
the key code and modifiers for a shortcut on the current keyboard layout.

When `--config` is omitted, `run`, `check`, and `install` use
`$XDG_CONFIG_HOME/mx-keypad/config.json` if `XDG_CONFIG_HOME` is absolute. Otherwise, they use
`~/.config/mx-keypad/config.json`.

## Configure pages and keys

This example uses one page and nine keys. Save it as a JSON file and pass its path to `check` or
`run`:

```json
{
  "brightness": 70,
  "defaults": { "background": "#000000", "foreground": "#ffffff", "glyph_scale": 0.55, "long_press_ms": 500 },
  "pages": [
    {
      "name": "main",
      "keys": {
        "1": { "icon": "bell", "label": "Alert", "action": { "run": "printf 'pressed\\n' >> /tmp/mx-keypad.log" } },
        "2": { "icon": "copy", "label": "Copy", "action": { "keys": "cmd+c" } },
        "3": { "icon": "play", "label": "Play", "action": { "media": "play-pause" }, "long_press": { "media": "next" } },
        "4": { "icon": "globe", "label": "Web", "action": { "open": "https://example.com" } },
        "5": { "icon": "keyboard", "label": "Hold", "action": { "hold": "ctrl+space" } },
        "6": { "icon": "layers", "label": "Switch", "action": { "switch": "cmd+tab" } },
        "7": { "icon": "mouse-left", "label": "Click", "action": { "mouse": "left" } },
        "8": { "icon": "lock", "label": "Lock", "action": { "system": "lock" } },
        "9": { "icon": "list-checks", "label": "Type", "action": { "sequence": [ { "keys": "cmd+a" }, { "delay": 100 }, { "text": "Hello!" } ] } }
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
| `defaults.long_press_ms` | Long-press threshold in milliseconds for all keys. Default: `500`. |
| `pages` | Required array of at least one page. The first page is shown at startup. |
| `pages[].name` | Required, unique page name. Use it as a page action's target. |
| `pages[].keys` | Optional object mapping key numbers `"1"`–`"9"` to key settings, in row-major order from the top left. Omitted keys have the default background and no icon, label, or action. |
| `pages[].keys.<key>.icon` | Optional built-in name from `mx-keypad icons`, or a path to an `.svg`, `.png`, `.jpg`, or `.jpeg` file. Relative paths resolve from the config file's directory; `~/` resolves from `HOME`. SVG `currentColor` uses the foreground color. |
| `pages[].keys.<key>.label` | Optional text drawn near the bottom of the key. Labels use system fonts. |
| `pages[].keys.<key>.background` | Optional background color; overrides `defaults.background`. |
| `pages[].keys.<key>.foreground` | Optional icon and label color; overrides `defaults.foreground`. PNG and JPEG icons keep their own colors. |
| `pages[].keys.<key>.glyph_scale` | Optional icon size, from `0.05` to `1.0`; overrides `defaults.glyph_scale`. |
| `pages[].keys.<key>.action` | Optional action from the table below. Defaults to `"none"`. |
| `pages[].keys.<key>.long_press` | Optional action to perform instead of `action` when the key is held for at least `defaults.long_press_ms`. |
| `page_buttons.left` | Optional action for the left page button. Default: `{ "page": "prev" }`. |
| `page_buttons.right` | Optional action for the right page button. Default: `{ "page": "next" }`. |

For a key with `long_press`, a short tap runs `action` on release; holding it to the threshold
runs `long_press` once, instead. The key shows its normal face for both actions. A key with
`long_press` can't use `hold` or `switch` for either action. Keys without `long_press` act on
press.

### Actions

Use the same action formats for keys and page buttons. `delay` is only valid inside a sequence.

| Action | Example | Behavior |
| --- | --- | --- |
| None | `"none"` | Do nothing. |
| Run | `{ "run": "echo hello" }` | Start a nonempty shell command with `sh -c`. |
| Page | `{ "page": "next" }` | Go to `next`, `prev`, or a named page. |
| Keys | `{ "keys": "cmd+c" }` | Press and release a shortcut. |
| Text | `{ "text": "Hello!" }` | Type a nonempty Unicode string. |
| Media | `{ "media": "play-pause" }` | Send `mute`, `volume-up`, `volume-down`, `play-pause`/`play`, `next`, or `previous`/`prev`. |
| Open | `{ "open": "Safari" }` | Open an app by name, or a URL or path. |
| Hold | `{ "hold": "ctrl+space" }` | Hold the shortcut's key and modifiers until the keypad button is released. |
| Switch | `{ "switch": "cmd+tab" }` | Tap the key once and hold the modifiers until release, keeping the macOS app switcher open. |
| Mouse | `{ "mouse": "left" }` | Click `left`, `right`, `middle`, `back`, or `forward` at the pointer. |
| System | `{ "system": "lock" }` | Run `sleep`, `display-sleep`, or `lock`. |
| Sequence | `{ "sequence": [{ "keys": "cmd+a" }, { "delay": 100 }, { "text": "Hello!" }] }` | Run steps in order on a background thread. |
| Delay | `{ "delay": 100 }` | Wait up to 10,000 milliseconds; only valid inside a sequence. |

`next` and `prev` wrap around the page list. A named page target must exist; `next` and `prev` are
reserved as navigation targets. A sequence must contain at least one step and can't contain `page`,
`hold`, `switch`, or another `sequence`. Within a sequence, `run` and `open` wait for their
processes to exit before the next step. Outside a sequence, they don't block input. Only use
configurations and commands you trust. Unknown configuration fields are rejected.

`open` uses macOS `open -a` for app names and `open` for targets starting with `/`, `~`, `.`,
`mailto:`, or containing `://`. A leading `~/` expands to your home directory. `open`, `system`,
and the input-posting actions require macOS at runtime.

### Keystroke actions (macOS)

`keys`, `hold`, and `switch` use the same shortcut syntax. `text` posts Unicode independently of
the keyboard layout. Sending these actions requires macOS; other platforms can validate them but
can't send them.

A chord has zero or more modifiers and exactly one key, joined by `+`. Names are case-insensitive.
Modifiers are `cmd`/`command`/`super`, `ctrl`/`control`, `alt`/`opt`/`option`, and `shift`.

Keys include a single character, such as a letter, digit, punctuation, or `å`, and these names:

- Character aliases: `plus`, `equal`, `minus`, `quote`, `semicolon`, `backslash`, `comma`, `slash`,
  `period`, and `grave`. Use `plus` for `+`, because `+` separates chord parts.
- Positions: `f1`–`f12`, `return`/`enter`, `tab`, `space`, `delete`/`backspace`, `escape`/`esc`,
  `forwarddelete`, `home`, `end`, `pageup`, `pagedown`, `left`, `right`, `up`, and `down`

On macOS, character shortcuts resolve through the current keyboard layout and add the modifiers
needed to type that character. For example, on a Danish layout `cmd+[` resolves to cmd+option+8.
The main keyboard block takes precedence over the numeric keypad. If the layout can't type a
character, a US ANSI position is used where one exists; otherwise, the fallback is key code 0.
On a US layout, `plus` resolves to shift+`=`. Use `resolve` to check the result.

macOS requires Accessibility permission for the process responsible for mx-keypad. If the startup
configuration has an input-posting action, `run` requests the system prompt and prints a reminder
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

The Makefile wraps the everyday tasks. Run `make help` for the full list:

| Command | What it does |
|---|---|
| `make install` | Builds with Nix, checks your config, then installs and starts the LaunchAgent. Use it after every change to the code. |
| `make restart` | Restarts the running LaunchAgent, for example after granting Accessibility. |
| `make stop` | Stops the LaunchAgent until the next login or `make install`. |
| `make uninstall` | Stops the LaunchAgent and removes it. |
| `make status` | Shows whether the LaunchAgent is running. |
| `make log` | Follows the LaunchAgent's log. |
| `make check` | Validates your config and renders every tile. |
| `make test` | Runs the tests and Clippy. |

Set `CONFIG=path/to/config.json` to use a config other than `~/.config/mx-keypad/config.json`.
Config changes don't need a restart: the running agent reloads the file when it changes.

### Make a release

Releases are built on a Mac outside the Nix shell, so the binary only loads system libraries. You
need Xcode, Homebrew `rustup` with the `aarch64-apple-darwin` and `x86_64-apple-darwin` targets,
and a Developer ID Application identity in your keychain. Store your notarization credentials once:

```sh
xcrun notarytool store-credentials mx-keypad --apple-id <apple-id> --team-id <team-id>
```

Then set the version in `Cargo.toml`, commit, and run one of these:

```sh
scripts/release.sh dist       # universal binary, Developer ID signed, zipped into dist/
scripts/release.sh notarize   # dist, then notarize the zip with Apple
scripts/release.sh publish    # notarize, then tag v<version> and create the GitHub release
```

`mx-keypad install` keeps a release's Developer ID signature instead of re-signing it.

The protocol code is ported from OpenLogi. See [NOTICE](NOTICE) for attribution and licenses and
[PROTOCOL.md](PROTOCOL.md) for the device protocol. mx-keypad is licensed under MIT or Apache-2.0.
