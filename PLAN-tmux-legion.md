# tmux-legion drives the MX Creative Keypad: agent status per key

## Context

This is the original goal behind mx-keypad: show AI-agent activity on the MX Creative Keypad, next
to tmux-legion's Keychron Q0 Max LEDs, on whichever device is connected.

You chose:
- **tmux-legion owns the keypad as a library user** while its sidebar is open.
- **Agent tiles:** status colour plus an agent icon.
- **Up to 9 agent slots.**
- **mx-keypad goes public**, so tmux-legion (public, with CI and a Nix flake) can depend on it.
- **Icons from a folder**, with a built-in fallback.

Today the mx-keypad LaunchAgent holds the keypad exclusively, since macOS hidapi opens it
exclusively. So the two programs need a hand-over: mx-keypad steps aside while tmux-legion's
sidebar runs, then takes the keypad back.

What I found in tmux-legion (`~/git/tmux-legion`):
- One `legion-leds` worker thread owns the only `HidApi`. It's fed the newest frame through a
  mailbox, `Leds::render(SlotColors)`, in `src/keyboard/mod.rs`.
- The sidebar loop builds frames with `slot_colors(entries, lit)`, blinks at 500 ms phases
  (`blink_lit`), and reconciles agents every 2 s (`src/sidebar/mod.rs`).
- Slots come from `state::assign_slots`. `state::SLOT_COUNT` is tied to the Keychron's 4 keys.
- Focus runs through `commands/focus.rs` and `App::focus_slot`.
- There's no backend abstraction, and nothing about the keypad exists yet.

## 1. mx-keypad v0.5.0: hand-over ("claim")

**New `src/claim.rs`**, shared by both programs:
- `Claim::acquire(owner) -> Result<Claim>` writes `pid` and `owner` to
  `$XDG_STATE_HOME/mx-keypad/claim`, falling back to `~/.local/state/mx-keypad/claim`. `Drop`
  removes the file only if it still names this process.
- `claim::foreign() -> Option<(u32 pid, String owner)>` returns another process's claim, and only
  while that process is alive (checked with `kill(pid, 0)`). A claim left by a killed sidebar
  therefore expires by itself.

**`mx-keypad run`:**
- Checks for a foreign claim every tick, and before every open.
- When a foreign claim appears, it logs "yielding to tmux-legion (pid …)" with a timestamp and
  releases held input. It then drops the `Keypad` without clearing it, because the claimant draws
  next. Dropping also hands the page buttons back.
- It waits until the claim is gone, then reopens and redraws, as on a reconnect. The tile and draw
  caches reset with the new connection.

**Release:** tests for the claim (own versus foreign, a dead pid, drop removes only its own) and the
README. Release v0.5.0, then make `hawkish/mx-keypad` public with
`gh repo edit --visibility public`.

## 2. tmux-legion: keypad backend

**Dependency:**
- `mx-keypad = { git = "https://github.com/hawkish/mx-keypad", tag = "v0.5.0" }`. hidapi stays
  one shared crate: both require ^2 with the same per-OS features.
- `flake.nix`: `cargoLock.outputHashes."mx-keypad-0.5.0"`. Linux CI compiles mx-keypad without its
  macOS keystroke code.

**Slots** (`src/state.rs`): a new `state::SLOT_COUNT = 9`, separate from `keyboard::SLOT_COUNT = 4`
(the Keychron keys).
- Assignment is unchanged: lowest free slot, sticky. So the first four agents still land on the
  Keychron, and the same agent has the same number on both devices.
- `keyboard::slot_colors` only covers slots 1–4.
- The sidebar's slot label (`src/sidebar/ui.rs`, now `SLOT_KEYS[slot-1]`) shows the numpad key for
  slots 1–4 and the keypad key number for 5–9. Indexing `SLOT_KEYS[slot-1]` directly would panic
  for slots past 4.
- The shell plugin's 4-binding cap and its test stay as they are.

**Frame** (`src/keyboard/mod.rs`): the mailbox carries `Frame { leds: SlotColors, keypad: [KeyFace; 9] }`
instead of `SlotColors`.
- `KeyFace` is `Empty` or `Agent { background: Rgb, icon: Icon, dimmed: bool }`, and implements
  `PartialEq` so the worker can diff frames.
- A new `keypad_faces(entries, lit, icons)` in `src/sidebar/mod.rs` builds it, alongside
  `slot_colors`:
  - The background is `palette::status_color` (red blocked, amber working, teal done, green idle,
    violet unknown).
  - Working and blocked blink: on the unlit half they show black, matching the Keychron blink.
  - An exited agent shows violet without blinking, as on the Keychron.
  - An empty slot is a black key.
  - The icon is white on dark colours and black on light ones, chosen by luminance.

**Icons:**
- A new tmux option, `@legion_keypad_icons`, names a folder. It's read once with the other options
  in `Config::from_options`.
- The agent's `name` picks the file (`claude.png`, `.svg` or `.jpg`). Lookups are cached per name.
- The fallback is mx-keypad's built-in `square-terminal`.
- A second option, `@legion_keypad off`, disables the keypad backend.

**Worker** (`src/keyboard/mod.rs` `paint_loop` and `Painter`):
- A `KeypadPainter` lives next to the Keychron painter. It uses the same `HidApi`
  (`Keypad::open(&api)`) on the same thread.
- **Claim:** `Leds::new` takes the claim with `Claim::acquire("tmux-legion")` when the backend is
  on. `shutdown` and Drop blank the keypad, close it and release the claim, so mx-keypad takes the
  keypad back.
- **Opening:** if the open fails because mx-keypad hasn't yielded yet, it retries after 1 s. Once
  connected, a failure follows the existing retry and warning model.
- **Drawing:** tiles are rendered with `mx_keypad::render::render_tile` and cached per `KeyFace`.
  `Keypad::draw` already skips unchanged keys, so a blink writes only the blinking keys.
- **Presses:** while the keypad is open, the worker waits with `wait_timeout(50 ms)` instead of
  `wait`, and polls the keypad between frames. Each `Event::Down(Key(n))` goes over an
  `mpsc::Sender<u8>` to the sidebar. That follows the existing `power::watch()` receiver pattern.
- **Sleep and wake:** sleep blanks the keypad alongside the Keychron, and wake repaints it.

**Sidebar loop:**
- `rx.try_recv()` each iteration, then `app.focus_slot(n)`. That's the existing lookup plus
  `tmux::select_pane`. Latency is at most one poll, 250 ms.
- The footer's red `⌨` also covers a keypad that has stopped answering, once one has been seen.

**`keyboard::cleanup()`** (toggle and close kill the sidebar pane) also releases a claim the dead
sidebar left behind. The pid check covers it anyway.

**Docs:** a new README section "MX Creative Keypad" covering the hand-over, slots 1–9, colours and
blink, icons and the options, plus rows in the options table.

## 3. Install and try

1. Release mx-keypad v0.5.0, make it public, and run `make install`, so the agent can yield.
2. Build tmux-legion and install it the way you normally do. Its flake is your plugin source, so I'll
   check how you install it before running anything.
3. Open the sidebar. The keypad should switch from your page to the agent tiles. Close it, and your
   Lock/Mute/æøå page should come back.

## Verification

- **mx-keypad:** claim unit tests; `make test`. On hardware:
  - A test process takes the claim, and the log shows "yielding".
  - Release it, and the page comes back.
  - Kill the claimant with `kill -9`, and mx-keypad takes the keypad back within a tick.
- **tmux-legion:** `cargo test` and `cargo clippy --all-targets -D warnings` in `nix develop`.
  - New tests, in the existing style: `keypad_faces`, `KeyFace` diffing in the keypad painter
    (without hardware, like `configured_painter`), and the slot-label fallback for slots 5–9.
  - An `#[ignore]` hardware smoke test, in the same style as the Keychron one.
  - `nix build`.
- **End to end:**
  - Start two or more agents. They get keys 1… in the status colours, and working or blocked ones
    blink.
  - Pressing a key focuses that agent's pane.
  - The Keychron keys 4,5,6,1 still match slots 1–4.
  - Closing the sidebar returns the keypad to mx-keypad.
