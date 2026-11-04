# mx-keypad

Control the Logitech MX Creative Keypad over USB with a JSON configuration. Set images and actions
for its nine 118 × 118 LCD keys, assign the two page buttons, and adjust panel brightness. Bind
the [MX Dialpad](#add-an-mx-dialpad)'s buttons, dial, and roller in the same configuration.

The Rust library exposes the same device operations without an async runtime. Other programs can
add [remote pages](#remote-pages) while the agent keeps running your configured actions.

## Install and check a configuration

Releases are source only, so build mx-keypad with Nix or Cargo. Install a release tag:

```sh
nix profile install github:hawkish/mx-keypad/v0.10.1
# Or, with Rust and Cargo installed:
cargo install --git https://github.com/hawkish/mx-keypad --tag v0.10.1
```

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

## Hand the keypad to another program

The claim mechanism is deprecated in v0.7.0. Use [remote pages](#remote-pages) for new integrations;
claims remain available for older clients.

macOS `hidapi` opens the keypad exclusively, so `run` can share it with another program, such as
tmux-legion's sidebar, by taking turns instead of at the same time. The other program takes a
*claim*: a file at `$XDG_STATE_HOME/mx-keypad/claim`, or `~/.local/state/mx-keypad/claim` if
`XDG_STATE_HOME` isn't set, naming its pid and itself. `run` checks for a foreign claim before
every attempt to open the keypad, and once a tick while serving one.

When a foreign claim appears, `run` logs "yielding to \<owner\> (pid \<pid\>)" and drops the keypad
without clearing it, since the claimant draws next. It waits until the claim is gone, then reopens
and redraws, as it would after a reconnect. A claim left behind by a killed process doesn't count,
so it expires on its own the next time `run` checks.

`mx_keypad::claim::Claim::acquire(owner)` takes the claim for the calling process; dropping it
releases the claim, unless another process's claim has since replaced it.

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

### Troubleshoot: keypad connects but keys do nothing

If the keypad (or Dialpad) connects and shows the right page, but pressing a key has no effect,
check `~/Library/Logs/mx-keypad.log` for repeated permission errors:

- `macOS has not allowed mx-keypad to send keystrokes` — Accessibility permission is missing. Grant
  it under **System Settings → Privacy & Security → Accessibility**.
- `macOS refused to open the MX Dialpad` — Input Monitoring permission is missing. Grant it under
  **System Settings → Privacy & Security → Input Monitoring**.

Installing a new build (for example after adding a Dialpad binding) changes the binary, and macOS
can silently revoke both grants even though they stay checked in the list: toggle each one off and
back on (or remove and re-add mx-keypad with `-`/`+`) for both panes, then restart the agent with
`mx-keypad uninstall && mx-keypad install --config ...` or `make restart`, and confirm the log
shows `keypad connected` / `dialpad connected` with no further permission errors.

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
key and page-button press and release events, and the MX Dialpad's button presses and turns when one
is connected. `icons` lists the built-in names. `draw`,
`brightness`, and `events` need the keypad; `check`, `icons`, and `resolve` don't. `resolve` prints
the key code and modifiers for a shortcut on the current keyboard layout.

When `--config` is omitted, `run`, `check`, and `install` use
`$XDG_CONFIG_HOME/mx-keypad/config.json` if `XDG_CONFIG_HOME` is absolute. Otherwise, they use
`~/.config/mx-keypad/config.json`. If that file doesn't exist, `run` and `install` first write the
starter config built into the binary, [examples/demo.json](examples/demo.json), so a fresh install
works right away. They never overwrite an existing file, and they never write to a path you pass
with `--config`.

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
| `pages[].keys.<key>.label` | Optional text drawn near the bottom of the key. On a key without an icon, the label is the face: centred, sized by `glyph_scale`, and shrunk until it fits within 75 % of the key. `\n` starts a new line, as in `"nh\nswitch"`. Labels use system fonts. |
| `pages[].keys.<key>.background` | Optional background color; overrides `defaults.background`. |
| `pages[].keys.<key>.foreground` | Optional icon and label color; overrides `defaults.foreground`. PNG and JPEG icons keep their own colors. |
| `pages[].keys.<key>.glyph_scale` | Optional icon size, from `0.05` to `1.0`; overrides `defaults.glyph_scale`. |
| `pages[].keys.<key>.action` | Optional action from the table below. Defaults to `"none"`. |
| `pages[].keys.<key>.long_press` | Optional action to perform instead of `action` when the key is held for at least `defaults.long_press_ms`. |
| `pages[].keys.<key>.shifted` | Optional. What the key shows and does while shift is on. Takes `icon`, `label`, `background`, `foreground`, `glyph_scale`, `action` and `long_press`; each one you leave out follows the key's own, shifted automatically (see below). |
| `pages[].keys.<key>.blink` | Optional. Makes the key blink between its face and one with `background` (default `#000000`) and `foreground` (default: the key's own). Each face shows for `interval_ms`, from `200` to `10000`; default `500`, which is 1 Hz. |
| `page_buttons.left` | Optional action for the left page button. Default: `{ "page": "prev" }`. |
| `page_buttons.right` | Optional action for the right page button. Default: `{ "page": "next" }`. |

For a key with `long_press`, a short tap runs `action` on release; holding it to the threshold
runs `long_press` once, instead. The key shows its normal face for both actions. A key with
`long_press` can't use `hold` or `switch` for either action. Keys without `long_press` act on
press.

A blinking key is redrawn at every interval, since the keypad can't blink by itself. All blinking
keys count from the same moment, so keys with the same interval switch together, and changing
page doesn't restart them. For example, a blue key that flashes black once a second:

```json
"2": { "icon": "volume-x", "background": "#0000FF", "blink": {}, "action": { "media": "mute" } }
```

### Shift

A key with a `shift` action is the keypad's own shift. While shift is on, every other key uses its
shifted binding, and the keys redraw to show it. The shift key itself shows its colours swapped, so
you can see when shift is latched. Keys you don't give a `shifted` field are shifted automatically:

| Action | While shift is on |
| --- | --- |
| `text` | Typed in upper case: `æ` types `Æ`. The key's label is shown in upper case too. |
| `keys`, `hold`, `switch` | The same shortcut with `shift` added, unless it already has it. |
| `sequence` | Its `text` and `keys` steps are shifted. |
| Anything else | Unchanged. |

For example, `æ` and `Æ` on one key, a save key that becomes "Save as", and a shift key that
toggles:

```json
"4": { "label": "æ", "action": { "text": "æ" } },
"5": { "label": "Save", "action": { "keys": "cmd+s" },
       "shifted": { "label": "Save as", "action": { "keys": "cmd+shift+s" } } },
"8": { "icon": "chevrons-up", "label": "Shift", "action": { "shift": "toggle" } }
```

A shift key can't have `long_press`, and `shift` can't be a `long_press` or a sequence step. The
page buttons ignore shift. A `once` shift is spent by the next key press, whatever that key does.

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
| Shift | `{ "shift": "toggle" }` | Switch the other keys to their shifted bindings: `hold` while held, `toggle` on and off with each tap, or `once` for the next key press. |

`next` and `prev` wrap around the page list. A named page target must exist; `next` and `prev` are
reserved as navigation targets. A sequence must contain at least one step and can't contain `page`,
`hold`, `switch`, `shift`, or another `sequence`. Within a sequence, `run` and `open` wait for their
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

## Add an MX Dialpad

`run` can also act on the MX Dialpad's four buttons, its big dial, and its small roller. The
Dialpad has no screens, so it doesn't have pages of its own. Its bindings stay the same whichever
keypad page is showing, and they can turn the keypad's pages. Add a `dialpad` section to your
configuration, as in [examples/dialpad.json](examples/dialpad.json):

```json
"dialpad": {
  "buttons": {
    "1": { "keys": "cmd+z" },
    "2": { "keys": "cmd+shift+z" },
    "3": { "page": "prev" },
    "4": { "page": "next" }
  },
  "dial": { "left": { "media": "volume-down" }, "right": { "media": "volume-up" }, "step": 10 },
  "roller": { "up": { "keys": "cmd+=" }, "down": { "keys": "cmd+-" } }
}
```

| Field | Description |
| --- | --- |
| `dialpad.buttons` | Optional object mapping button numbers to actions: `"1"` top left, `"2"` top right, `"3"` bottom left, `"4"` bottom right. Omitted buttons do nothing. |
| `dialpad.dial.left` | Optional action for turning the dial counterclockwise. |
| `dialpad.dial.right` | Optional action for turning the dial clockwise. |
| `dialpad.dial.step` | How much turn performs the action once. The dial measures 180 steps per revolution. Default: `5`, which is 36 actions per revolution. |
| `dialpad.roller.up` | Optional action for rolling away from you. |
| `dialpad.roller.down` | Optional action for rolling toward you. |
| `dialpad.roller.step` | How many roller notches perform the action once. The roller has 40 per revolution. Default: `1`. |

Buttons take any action, as page buttons do, and ignore shift. A turn performs its action once per
`step`, so it can't use `hold`, `switch`, or `shift`. Turning back the other way starts counting
again, so reversing acts right away.

Pair the Dialpad over Bluetooth and quit Logi Options+, which also takes over its controls.
Connecting through a Logi Bolt receiver isn't supported. While `run` serves a `dialpad` section,
it takes over all of the Dialpad's controls, so they stop sending mouse buttons and scrolling. It
waits for the Dialpad and reconnects independently of the keypad, and works without a keypad
connected. When `run` stops, or you remove the `dialpad` section, it hands the controls back to
the firmware. Without a `dialpad` section, `run` doesn't open the Dialpad.

On macOS, opening the Dialpad needs **Input Monitoring** permission, because it's also a mouse.
Turn it on for mx-keypad under **System Settings** > **Privacy & Security** > **Input
Monitoring**. When you run mx-keypad from a terminal, the terminal needs the permission instead.
If the permission is missing, `run` logs "macOS refused to open the MX Dialpad". If the switch is
already on, a terminal that was updated since you granted it may no longer match the permission:
remove the terminal from the list, add it again, and restart it. Dialpad support has only been
tested on macOS.

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

### Running a config

`mx_keypad::session::Session` is the engine behind `mx-keypad run`: given a `config::Setup`, it
decides what each key shows (`Session::face`, blink- and shift-aware) and performs an action's side
effects when told a button went down or up (`Session::down`, `Session::up`), including held `hold`
and `switch` shortcuts, `sequence` steps on a background thread, and the keypad's own shift. A
`Session` never touches a `Keypad` or a terminal — reading presses, drawing tiles, diverting the
page buttons, and reloading the config from disk stay the caller's job, which is what keeps it
usable by a program that only wants to borrow the keypad for one of its own pages alongside
something else, such as tmux-legion's agent-status page:

```rust
use mx_keypad::config::Setup;
use mx_keypad::session::Session;
use mx_keypad::Button;

fn demo() -> anyhow::Result<()> {
    let setup = Setup::load("config.json".as_ref())?;
    let mut session = Session::new(setup);

    // Wire these to a Keypad's events and a redraw loop; see `mx-keypad
    // run`'s own `connect`/`serve` for the full shape, including
    // reconnecting and reloading the config file.
    session.down(Button::Key(1))?;
    let face_for_key_one = session.face(0, std::time::Instant::now());
    let _ = face_for_key_one;
    Ok(())
}
```

## Remote pages

Publish a page from another program while `mx-keypad run` owns the device. The page's keys use
the config's own key format, and the agent runs their actions itself, so the other program never
needs Accessibility. Keys without an action are sent back to that program as presses instead. Connect to `$XDG_STATE_HOME/mx-keypad/agent.sock`, or
`~/.local/state/mx-keypad/agent.sock` when `XDG_STATE_HOME` isn't an absolute path. The socket has
mode `0600` and stays available while the keypad is unplugged or a legacy claim is active.

Protocol v1 uses newline-delimited JSON. Send `hello` first and wait for `welcome`.

| Direction | Message | Meaning |
|---|---|---|
| Client → agent | `{"hello":{"client":"my-app","protocol":1}}` | Start the connection. An invalid handshake gets an error and closes. |
| Agent → client | `{"welcome":{"protocol":1,"version":"0.7.0"}}` | Handshake accepted. |
| Client → agent | `{"page":{"name":"agents","keys":{"1":{"label":"Ready"}},"show":false}}` | Add or replace an owned page. Omitted keys are blank; `show` defaults to false. |
| Client → agent | `{"keys":{"page":"agents","keys":{"3":{"icon":"bell"}}}}` | Replace the specified keys. |
| Client → agent | `{"show":"agents"}` | Turn to an owned page. |
| Client → agent | `{"remove":"agents"}` | Remove an owned page. |
| Client → agent | `{"dialpad":{"buttons":{"1":{"keys":"escape"}}}}` / `{"dialpad":null}` | Bind the MX Dialpad, in the config's `dialpad` format, or let it go. |
| Client → agent | `{"brightness":70}` / `{"brightness":null}` | Set the panel brightness, 1–100, in place of the config's `brightness`, or let it go. |
| Agent → client | `{"down":{"page":"agents","key":3}}` / `{"up":{"page":"agents","key":3}}` | Press or release of a grid key, numbered 1–9, that has no action or long press. |
| Agent → client | `{"error":{"message":"…"}}` | Refused request. After the handshake, the connection stays open. |

Each key is written exactly as in a config page: `icon`, `label`, `background`, `foreground`,
`glyph_scale`, `action`, `long_press`, `shifted`, and `blink`, with the default `defaults`. For
example, `{"label":"Esc","action":{"keys":"escape"}}` is a key the agent presses Escape for. Icons
must be built-in names or absolute or `~/` paths, and `page` actions can only be `next` or `prev`.
The agent checks a whole request before applying any of it.

While any client has a page, the clients' pages replace the config's: the keypad turns to the
first one, and page-turn actions go through the client pages only, in the order they were added.
The config's pages come back when the last client page goes. The agent takes over the page buttons
whenever a remote page exists. Names share one namespace:
a client can replace its own page but can't take another client's or a config page's name.
Disconnecting removes that client's pages. Removing the visible page shows the first client page left,
or the first config page when none is; removing another page keeps the current one. Config reloads preserve remote pages and the
current page by name. A reload that collides with a remote name keeps the previous config.

A release goes to the page that received its press, even after a page turn. Shift, long press and
blink work on remote pages as on config pages, and the keypad's shift is shared by every page.
A client that reconnects within a minute and adds its page again gets it back on screen, if that
page was showing and no page was turned in between.

A client's `dialpad` bindings replace the config's `dialpad` section until the client sends
`{"dialpad":null}` or disconnects; then the config's bindings, if any, apply again. If several
clients send bindings, the last one wins. As on client pages, `page` actions can only be `next` or
`prev`. The agent takes over the Dialpad while either the config or a client binds it. See
[Add an MX Dialpad](#add-an-mx-dialpad).

A client's `brightness` works the same way: it replaces the config's `brightness` until the client
sends `{"brightness":null}` or disconnects, and the last client to send one wins. Then the config's
brightness, if it sets one, applies again; otherwise the panel stays as the client left it.

With the `mx-keypad` dependency above, publish a page and handle its presses in Rust:

```rust
use mx_keypad::config::{Action, Key};
use mx_keypad::remote::Client;
use mx_keypad::{Button, Event};
use std::time::Duration;

fn main() {
    let mut client = Client::new("my-app");
    let mut keys: [Key; 9] = std::array::from_fn(|_| Key::default());
    keys[0].label = Some("Ready".into()); // no action: presses come back here
    keys[1].label = Some("Esc".into());
    keys[1].action = Action::Keys("escape".into()); // the agent presses it
    client.set_page("agents", keys);

    loop {
        for (page, event) in client.poll() {
            if let Event::Down(Button::Key(key)) = event {
                println!("{page}: key {key}");
            }
        }
        for error in client.take_errors() {
            eprintln!("{error}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
```

Call `poll()` each frame: it doesn't block, and connection handshakes run on a worker thread.
The client reconnects with a 1-second backoff, doubling to 30 seconds, and resends its desired
pages in insertion order, then its Dialpad bindings. Repeated `set_page` calls send only changed
keys. `set_dialpad(Some(bindings))` binds the Dialpad and `set_dialpad(None)` lets it go; repeating
the same bindings sends nothing. Use `show(name)` after
`connected()` becomes true to turn to a page, or `remove(name)` to stop publishing it.
Read connection and protocol errors with `take_errors()`.

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

CI publishes, so releasing is a commit and a push. A release is a tag and its notes, with no
binaries attached. `Cargo.toml` is the single source of truth for the version: `flake.nix` and the
release job both read it from there. The install commands under
[Install and check a configuration](#install-and-check-a-configuration) don't, so update their tag
by hand.

1. Set the new version in `Cargo.toml`, then run `cargo check` to refresh `Cargo.lock`.
2. Run `make notes` to scaffold `.github/release-notes/vX.Y.Z.md` from the commits since the last
   tag, then rewrite the bullets into prose.
3. Commit all of it as `📦 release: vX.Y.Z` and push.

The `release` job in [ci.yml](.github/workflows/ci.yml) takes it from there. It waits for the
build, tests, Clippy, and rustfmt to pass on Linux and macOS, then checks whether the commit subject
is a release for the version in `Cargo.toml`. On any other commit, it stops there. On a release
commit, it tags, pushes the tag, and creates the GitHub release from the notes file. It fails if
those notes are missing.

The protocol code is ported from OpenLogi. See [NOTICE](NOTICE) for attribution and licenses and
[PROTOCOL.md](PROTOCOL.md) for the device protocol. mx-keypad is licensed under MIT or Apache-2.0.
