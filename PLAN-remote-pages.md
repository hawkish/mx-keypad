# The agent keeps the keypad: remote pages over a socket

## Context

tmux-legion currently takes the keypad from `mx-keypad run` with a claim
file (v0.5.0), then draws, reads presses, and runs a second config through
`mx_keypad::session::Session` (v0.6.0). That pushes all of the agent's work
into a process that wasn't built to do it:

- **Accessibility.** Keystrokes, text and media keys are posted from the
  tmux-legion process, which macOS hasn't trusted. Each `cargo build` makes a
  new ad-hoc signature, so even a grant doesn't last. The installed agent
  already has a stable, trusted binary. That's the reason it exists.
- **Two configs.** `~/.config/tmux-legion/keypad.json` duplicates the user's
  own mx-keypad pages.
- **Handoff churn.** Yield, wait, reopen and redraw on every sidebar start,
  plus `exclusive access and device already open` races in the log.

## Design

`mx-keypad run` never gives up the device. It listens on a Unix socket, and
other programs add *remote pages* to its page list. The agent draws them and
forwards presses on them back to the client. Every other page, including all
the user's own config pages, keeps running in the agent's `Session`.

```
tmux-legion ──socket──► mx-keypad run ──HID──► keypad
  page "agents": faces       draws, page buttons, every local action
  ◄── down/up "agents" k
```

### Socket

`$XDG_STATE_HOME/mx-keypad/agent.sock`, else
`~/.local/state/mx-keypad/agent.sock`, the same base as the claim. The agent
removes a stale socket file at start and its own on exit. The socket is
created with mode 0600: only the user can drive the keypad.

The socket lives for all of `run`, not one keypad connection. Clients stay
connected while the keypad is unplugged, and their pages are drawn again when
it comes back.

### Protocol (v1)

Newline-delimited JSON, one object per line. Messages are externally tagged
the same way config actions are (`{"page": {...}}`).

Client → agent:

| Message | Meaning |
|---|---|
| `{"hello": {"client": "tmux-legion", "protocol": 1}}` | First line. The agent replies `welcome` or `error` and closes. |
| `{"page": {"name": "agents", "keys": {"1": FACE, …}, "show": false}}` | Add a page, or replace it if this connection already owns it. Keys left out are blank. `show` turns to it. |
| `{"keys": {"page": "agents", "keys": {"3": FACE}}}` | Change some keys of an owned page. |
| `{"show": "agents"}` | Turn to the page. |
| `{"remove": "agents"}` | Remove an owned page. |

`FACE` uses the config's own face fields: `icon` (a built-in name or an
absolute or `~/` path), `label`, `background`, `foreground`, `glyph_scale`.
Colours that are left out use the defaults: black background, white
foreground.

Agent → client:

| Message | Meaning |
|---|---|
| `{"welcome": {"protocol": 1, "version": "0.7.0"}}` | Reply to `hello`. |
| `{"down": {"page": "agents", "key": 3}}` / `{"up": …}` | A key on the client's page. `up` goes to the page the key went down on, even if the page turned in between. |
| `{"error": {"message": "…"}}` | A message was refused (unknown key, a name already taken by another page, bad icon). The connection stays open. |

Page names share one namespace with config pages. A name that's already
taken is refused.

### Pages and the page buttons

- Remote pages come after the config's pages, in the order they were added.
  `next`/`prev` and the page buttons go through all of them.
- Whenever a remote page exists, the agent takes the page buttons over
  (`needs_page_buttons`).
- When a client disconnects, its pages are removed. If one of them was on
  screen, the agent goes back to the first page.
- A remote page has no shift, long press or blink. The client decides every
  face, and its blink is just a stream of `keys` updates. The keypad's shift
  stays latched across a visit to a remote page.
- A config reload keeps remote pages and stays on the current page by name.

### Library

`mx_keypad::remote` (public):

- the wire types (`Request`, `Reply`, `WireFace`) and `socket_path()`;
- `Server`: binds, accepts on a thread, and hands the main loop `Incoming`
  messages over a channel. Writes have a short timeout, and a client that
  stops reading is dropped;
- `Client`: what tmux-legion uses. `set_page(name, [WireFace; 9])` is safe
  to call every frame. It diffs against what was sent and sends only the
  changed keys. On disconnect it reconnects with backoff and resends its
  pages. `poll()` doesn't block and returns `(page, Event)`.

`Session` gains remote pages: `add_remote`, `set_remote_keys`,
`remove_remote`, `drop_owner`, and `take_remote_events` for the presses to
forward. `face`, `down`, `up` and page turning take both kinds of page into
account. Existing `Session` users that never add a remote page see no change.

### `run`

The serve loop drains the server each pass. While any client is connected it
polls HID with a 50 ms timeout instead of 200 ms, so face updates show up
promptly. After presses, client messages, or a blink, it redraws, which is
cheap thanks to the tile cache and `Keypad::draw` skipping unchanged keys.
While waiting for the keypad, it still services the socket.

The claim stays for v0.7 so an older tmux-legion keeps working. It will be
deprecated in the README and removed in a later release.

## Steps

### 1. mx-keypad v0.7.0

1. `src/remote.rs`: wire types, `socket_path`, `Server`, `Client`, plus
   round-trip and in-process socket tests.
2. `Session`: remote pages, with tests for page turning across both kinds,
   forwarding, up-follows-down, owner drop, and reload.
3. `main.rs`: own a `Server` across reconnects, apply `Incoming`, forward
   events, redraw, and use the shorter poll while a client is connected.
4. README: a "Remote pages" section, and a deprecation note on the claim.
   Bump to 0.7.0.

### 2. tmux-legion

1. Depend on mx-keypad v0.7.0 and replace `KeypadPainter` with a
   `remote::Client`. It sends the agents page from the frame's
   `KeypadFaces` and turns `down` events into focus presses.
2. Delete `utility.rs`, `~/.config/tmux-legion/keypad.json` handling, the
   claim, the HID open/retry code and the debug logging. The utility page is
   now just the user's mx-keypad pages.
3. `blank()` (sleep or night mode) removes the page, and `wake()` adds it
   back.

## Verification

- `cargo test`, `cargo clippy --all-targets` and `make test` in both repos.
- On hardware: with the sidebar running, page right goes from the config
  pages to "agents" and back. Esc, Mute and æ/ø/å work on the config pages
  while tmux-legion runs, and nothing new is granted Accessibility. Pressing
  an agent tile focuses its pane. Killing the sidebar removes the page within
  one tick. Restarting the agent makes the sidebar reconnect and its page
  come back.
- `mx-keypad.log` shows no more "yielding" or "exclusive access" lines.
