# Tarp tmux Control-Mode Tabs, Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open one local tmux session's windows as Tarp tabs over a single `tmux -C` control connection, so Claude Code renders and runs correctly inside them.

**Architecture:** A new UI-free crate, `crates/tmux_control`, holds the protocol and the transport. The protocol side covers the `%output` octal unescape, the `ESC Ptmux;` unwrapper, the control-line parser, the reply queue, command builders and screen restore. The transport side is `Connection`, which runs one `tmux -C attach` child with a reader thread and a writer thread and fans pane output out to bounded per-pane channels. In the app, `terminal/tmux_control/` adds a per-tab backend modelled on `remote_tty` (a `TerminalManager` plus an event loop that parses off the UI thread). It also adds an app-wide `TmuxControlSessions` singleton that owns the connections. The terminal model gets a "raw mode" that skips Warp bootstrap. The workspace gets a Command Palette entry, "Attach tmux session", which opens the windows as tabs in one ordinary tab group named after the session.

**Tech Stack:** Rust 1.92 (edition 2024), std threads and `std::process`, `async-channel`, WarpUI models and views, tmux ≥ 3.4 (developed against 3.8), cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-10-21-tarp-tmux-control-mode-design.md` (phase 1 of "Suggested phasing"; protocol facts in "Verified facts this design rests on").

**Why a new crate:** Everything that can be checked without a window lives there, so it builds and tests in seconds with `cargo nextest run -p tmux_control` and doesn't pull in the `warp` app crate. It depends only on `async-channel`, `log`, `parking_lot` and `thiserror`. Phase 3 (ssh) adds a transport to it without touching the app. `crates/warp_terminal` was rejected because it is the VT/model crate, and process I/O doesn't belong there.

## Global Constraints

- Minimum tmux: **3.4**. Tarp probes `display-message -p '#{version}'` before anything else and refuses lower versions with: `tmux <found> is too old; Tarp needs tmux 3.4 or newer`.
- Transport: `<tmux_binary()> -C attach [-t <target>]` over plain pipes (no tty), started with `TMUX` and `TMUX_PANE` removed from its environment. Tests add `-L <socket> -f /dev/null` before `-C`.
- Writes are held until the first server `%begin … %end` block. Every command line ends with `\n`. A `#` in a format must sit inside single quotes. Pane-target arguments of `refresh-client -A` must be quoted (`'%3:pause'`): unquoted, tmux 3.8 answers `parse error: syntax error`.
- Replies pair to commands FIFO. A `%begin` whose flags bit 0 (`flags & 1`) is clear comes from the server and pops nothing. A block closes only on `%end`/`%error` lines with the same time, number and flags.
- Reply bodies are raw bytes. Only `%output`/`%extended-output` data is octal-escaped: bytes `< 0x20` and `\` become `\ooo`.
- Input: `send-keys -t %P -H <hex…>`, at most **1000** bytes per command. Resize: `refresh-client -C @W:<cols>x<rows>`. Flow control: `refresh-client -f pause-after=5`.
- Gate: `FeatureFlag::TmuxControlMode` **and** setting `integrations.tmux.control_mode` (default `false`). With the setting off, Harp's poll-based tmux sync (`tab_grouping.rs`) behaves exactly as today.
- Phase 1 scope: no hosts, no sidebar sync, no `@tarp_*` options, no `kill-window` on close, and no agent status (OSC 777 is unwrapped and passed to the parser only). Closing a tmux tab only stops showing that window; tmux keeps it.
- Repo conventions (AGENTS.md):
  - Unit tests go in `*_tests.rs` files, included with `#[cfg(test)] #[path = "…_tests.rs"] mod tests;`.
  - A `ctx` parameter comes last.
  - Matches are exhaustive, with no `_` arm on repo enums.
  - Comments explain why, never what.
  - A toggle setting also gets Command Palette enable/disable entries.
- Every test that runs real tmux uses a private server (`tmux -L tarp-test-<pid>-<name> -f /dev/null`) and returns early with `eprintln!("skipping: tmux not installed")` when `tmux -V` fails.

## Review Focus

1. **Pending output.** tmux may have read pane output into the screen but not yet written it as `%output` when a capture runs. Expected: no line is applied twice after a restore. Pinned by `restore_then_live_output_has_no_duplicate_lines` (Task 5), and the restore is bracketed by pause/continue in Task 3.
2. **Terminal queries in the byte stream.** A pane program queries the terminal (`CSI c`, `CSI 6n`, kitty keyboard query). tmux has already answered, so Tarp must not answer again by typing into the pane. Expected: no `send-keys` caused by output. Pinned by `output_queries_are_not_answered` (Task 8).
3. **Session missing or no server.** Attaching to a session that doesn't exist, or with no server running. Expected: a toast with tmux's reason (`can't find session: nope` / `no sessions`), no tab, no hang. Pinned by `attach_refused_reports_tmux_reason` (Task 5) and `attach_failure_shows_toast_and_opens_no_tab` (Task 10).
4. **Tabs outside the persisted state.** Quitting or closing the window while tmux tabs are open. Expected: no "running processes" warning for them, and they are not saved to sqlite to come back as dead local tabs. Pinned by `running_summary_skips_raw_sessions` (Task 7) and `snapshot_excludes_tmux_window_tabs` (Task 10).
5. **Re-attach after closing tabs.** Attaching a session whose tabs were partly closed by the user. Expected: the missing windows reopen and the existing ones are not duplicated. Pinned by `reattach_reopens_closed_windows_without_duplicates` (Task 10).

---

### Task 1: `tmux_control` crate, ids and output decoding

**Files:**
- Create: `crates/tmux_control/Cargo.toml`
- Create: `crates/tmux_control/src/lib.rs`
- Create: `crates/tmux_control/src/ids.rs`
- Create: `crates/tmux_control/src/output.rs`
- Test: `crates/tmux_control/src/ids_tests.rs`
- Test: `crates/tmux_control/src/output_tests.rs`
- Modify: `Cargo.toml` (`[workspace.dependencies]`, next to `warp_terminal` at line 102; `[profile.dev.package]` next to `warp_terminal.opt-level = 3` at line 452)

**Interfaces:**
- Produces:
  - `tmux_control::{PaneId, WindowId, SessionId}`: `#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct PaneId(pub u32);`, and the same shape for the other two.
    - `impl FromStr` requires the prefix: `%`, `@` or `$`.
    - `impl Display` writes the prefix.
    - Parse errors use `IdParseError` (thiserror).
  - `tmux_control::output::unescape_output(data: &[u8]) -> Vec<u8>`.
  - `tmux_control::output::PassthroughUnwrapper` with `new() -> Self` (also `Default`) and `push(&mut self, input: &[u8], out: &mut Vec<u8>)`.

- [ ] **Step 1: Create the crate skeleton**

  `Cargo.toml`:
  - Package fields as in `crates/warp_util/Cargo.toml`: `edition = "2024"`, `publish.workspace = true`, `license.workspace = true`.
  - Dependencies: `async-channel`, `log`, `parking_lot` and `thiserror`, all `.workspace = true`.
  - `[features] test-util = []`.

  `lib.rs` declares `pub mod ids; pub mod output;` and re-exports `pub use ids::{PaneId, SessionId, WindowId};`.

  Root `Cargo.toml` gets:
  - `tmux_control = { path = "crates/tmux_control" }`,
  - `tmux_control.opt-level = 3`, because it decodes every byte a pane prints.

- [ ] **Step 2: Write the failing tests**

```rust
// ids_tests.rs
#[test]
fn ids_parse_and_print_with_their_prefix() {
    assert_eq!("%12".parse::<PaneId>().unwrap(), PaneId(12));
    assert_eq!("@3".parse::<WindowId>().unwrap(), WindowId(3));
    assert_eq!("$0".parse::<SessionId>().unwrap(), SessionId(0));
    assert_eq!(PaneId(7).to_string(), "%7");
    assert_eq!(WindowId(7).to_string(), "@7");
    assert_eq!(SessionId(7).to_string(), "$7");
    assert!("12".parse::<PaneId>().is_err());
    assert!("@3".parse::<PaneId>().is_err());
    assert!("%x".parse::<PaneId>().is_err());
}

// output_tests.rs
#[test]
fn octal_escapes_decode_and_utf8_passes_through() {
    assert_eq!(unescape_output(b"h\xc3\xa9\\134\\015\\012"), b"h\xc3\xa9\\\r\n");
    assert_eq!(unescape_output(br"\033[1mB"), b"\x1b[1mB");
    assert_eq!(unescape_output(br"a\9"), br"a\9"); // not an escape: kept as is
}

const WRAPPED: &[u8] = b"\x1bPtmux;\x1b\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07\x1b\\";
const UNWRAPPED: &[u8] = b"\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07";

#[test]
fn tmux_passthrough_is_unwrapped() {
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(WRAPPED, &mut out);
    assert_eq!(out, UNWRAPPED);
}

#[test]
fn unwrapping_is_independent_of_chunk_boundaries() {
    for split in 0..=WRAPPED.len() {
        let (mut unwrapper, mut out) = (PassthroughUnwrapper::new(), Vec::new());
        unwrapper.push(&WRAPPED[..split], &mut out);
        unwrapper.push(&WRAPPED[split..], &mut out);
        assert_eq!(out, UNWRAPPED, "split at {split}");
    }
}

#[test]
fn other_escapes_and_dcs_pass_through() {
    let input = b"a\x1b[1mb\x1bP1$r0m\x1b\\c";
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(input, &mut out);
    assert_eq!(out, input);
}

#[test]
fn a_partial_prefix_is_held_until_decided() {
    let (mut unwrapper, mut out) = (PassthroughUnwrapper::new(), Vec::new());
    unwrapper.push(b"ab\x1bPtm", &mut out);
    assert_eq!(out, b"ab");
    unwrapper.push(b"ux;\x1b\x1bX\x1b\\", &mut out);
    assert_eq!(out, b"ab\x1bX");
}

#[test]
fn doubled_inner_terminator_is_undoubled() {
    let mut out = Vec::new();
    PassthroughUnwrapper::new().push(b"\x1bPtmux;\x1b\x1b]0;t\x1b\x1b\\\x1b\\", &mut out);
    assert_eq!(out, b"\x1b]0;t\x1b\\");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

  Run: `cargo nextest run -p tmux_control`
  Expected: compile errors: `cannot find function unescape_output`, `PassthroughUnwrapper` and `PaneId` not found.

- [ ] **Step 4: Implement `ids.rs` and `output.rs`**

  `unescape_output`:
  - `\` followed by three digits `0-7` becomes one byte.
  - Any other `\` is copied as is.

  `PassthroughUnwrapper` is a byte state machine (`PREFIX = b"\x1bPtmux;"`):
  - **Normal.** On `ESC`, hold it and go to `Prefix(1)`. Any other byte is emitted.
  - **Prefix(n).** If the byte equals `PREFIX[n]`, hold it. When `n + 1 == PREFIX.len()`, drop what is held and go to `Inside`. On a mismatch, emit what is held and reprocess the byte as Normal.
  - **Inside.** On `ESC`, go to `InsideEsc`. Any other byte is emitted.
  - **InsideEsc.** On `ESC`, emit one `ESC` and go back to `Inside`. On `\`, the wrapper ends: go to `Normal`. Any other byte: emit `ESC` and the byte, then go back to `Inside`.

- [ ] **Step 5: Run the tests to verify they pass**

  Run: `cargo nextest run -p tmux_control`
  Expected: `6 tests run: 6 passed`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/tmux_control
git commit -m "Add tmux_control crate: ids, %output unescape, passthrough unwrap"
```

---

### Task 2: Control-line parser and reply queue

**Files:**
- Create: `crates/tmux_control/src/parser.rs`
- Create: `crates/tmux_control/src/queue.rs`
- Create: `crates/tmux_control/src/testdata/session_events.txt`
- Create: `crates/tmux_control/src/testdata/extended_output.txt`
- Create: `crates/tmux_control/src/testdata/capture_reply.txt`
- Test: `crates/tmux_control/src/parser_tests.rs`
- Test: `crates/tmux_control/src/queue_tests.rs`

**Interfaces:**
- Consumes: `PaneId`, `WindowId`, `SessionId` and `output::unescape_output` (Task 1).
- Produces:
  - `pub enum ControlEvent { Notification(Notification), Reply(CommandReply) }`.
  - `#[derive(Clone, Debug, PartialEq)] pub struct CommandReply { pub time: u64, pub number: u64, pub from_client: bool, pub success: bool, pub body: Vec<Vec<u8>> }`.
  - `#[derive(Clone, Debug, PartialEq)] pub enum Notification` with these variants:
    - `Output { pane: PaneId, data: Vec<u8> }` (from both `%output` and `%extended-output`; data already unescaped),
    - `WindowAdd(WindowId)`, `WindowClose(WindowId)`, `WindowRenamed { window: WindowId, name: String }`,
    - `UnlinkedWindowAdd(WindowId)`, `UnlinkedWindowClose(WindowId)`, `UnlinkedWindowRenamed { window: WindowId, name: String }`,
    - `LayoutChange { window: WindowId, layout: String }`,
    - `WindowPaneChanged { window: WindowId, pane: PaneId }`,
    - `SessionChanged { session: SessionId, name: String }`, `SessionRenamed { session: SessionId, name: String }`, `SessionsChanged`,
    - `Pause(PaneId)`, `Continue(PaneId)`,
    - `Exit { reason: Option<String> }`,
    - `Other(String)`.
  - `pub struct ControlParser` with:
    - `new() -> Self`,
    - `push_line(&mut self, line: &[u8]) -> Option<ControlEvent>` (the line has no trailing `\n`),
    - `is_ready(&self) -> bool`.
  - `pub(crate) struct CommandQueue<T>` with:
    - `new() -> Self`,
    - `push(&mut self, tag: T)`,
    - `complete(&mut self, reply: &CommandReply) -> Option<T>`,
    - `drain(&mut self) -> impl Iterator<Item = T>`.

- [ ] **Step 1: Add the recorded transcripts**

  These came from tmux 3.8 on a private server. Write them byte-exact, each line ending in `\n`. `é` is UTF-8 and `\134` etc. are literal backslash-digit text.

  `session_events.txt`:
  ```
  %begin 1792572895 315 0
  %end 1792572895 315 0
  %session-changed $0 demo
  %begin 1792572895 317 1
  %end 1792572895 317 1
  %layout-change @0 285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1] 285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1] *
  %output %0 hé\134\015\012
  %window-renamed @0 zsh
  %begin 1792572896 318 1
  %end 1792572896 318 1
  %window-pane-changed @0 %1
  %begin 1792572896 319 1
  %end 1792572896 319 1
  %unlinked-window-add @1
  %sessions-changed
  %unlinked-window-renamed @1 tmux
  %window-renamed @0 sleep
  %begin 1792572896 320 1
  %end 1792572896 320 1
  %session-renamed $0 demo2
  %begin 1792572896 321 1
  parse error: unknown command: bogus-command
  %error 1792572896 321 1
  %unlinked-window-renamed @1 zsh
  %begin 1792572897 322 1
  %end 1792572897 322 1
  %exit
  ```

  `extended_output.txt`:
  ```
  %begin 1792572885 315 0
  %end 1792572885 315 0
  %session-changed $0 demo
  %begin 1792572886 317 1
  %end 1792572886 317 1
  %begin 1792572886 318 1
  %end 1792572886 318 1
  %window-add @1
  %extended-output %0 1 : hé\134\015\012\033Ptmux;\033\033]777;notify;warp://cli-agent;{"x":1}\007\033\134
  %window-renamed @0 zsh
  %window-close @1
  %exit
  ```

  `capture_reply.txt`:
  ```
  %begin 1792572957 315 0
  %end 1792572957 315 0
  %session-changed $0 demo
  %begin 1792572957 317 1
  ab
  cd

  %end 1792572957 317 1
  %begin 1792572957 318 1

  %end 1792572957 318 1
  %begin 1792572957 319 1
  2 1 0
  %end 1792572957 319 1
  %window-renamed @0 sleep
  %begin 1792572958 320 1
  %end 1792572958 320 1
  %layout-change @0 e9dd,30x4,0,0,0 e9dd,30x4,0,0,0 *
  %begin 1792572958 321 1
  30 4
  %end 1792572958 321 1
  %exit
  ```

- [ ] **Step 2: Write the failing tests**

  `parser_tests.rs` uses two helpers:
  - `fn parse(fixture: &[u8]) -> Vec<ControlEvent>` splits on `\n`, feeds `push_line` and collects the `Some`s.
  - `fn ok(time, number, from_client, body: &[&[u8]]) -> ControlEvent` builds a successful `ControlEvent::Reply`.

```rust
#[test]
fn session_events_transcript() {
    use Notification::*;
    let n = ControlEvent::Notification;
    assert_eq!(parse(include_bytes!("testdata/session_events.txt")), vec![
        ok(1792572895, 315, false, &[]),
        n(SessionChanged { session: SessionId(0), name: "demo".into() }),
        ok(1792572895, 317, true, &[]),
        n(LayoutChange { window: WindowId(0), layout: "285d,40x5,0,0[40x2,0,0,0,40x2,0,3,1]".into() }),
        n(Output { pane: PaneId(0), data: b"h\xc3\xa9\\\r\n".to_vec() }),
        n(WindowRenamed { window: WindowId(0), name: "zsh".into() }),
        ok(1792572896, 318, true, &[]),
        n(WindowPaneChanged { window: WindowId(0), pane: PaneId(1) }),
        ok(1792572896, 319, true, &[]),
        n(UnlinkedWindowAdd(WindowId(1))),
        n(SessionsChanged),
        n(UnlinkedWindowRenamed { window: WindowId(1), name: "tmux".into() }),
        n(WindowRenamed { window: WindowId(0), name: "sleep".into() }),
        ok(1792572896, 320, true, &[]),
        n(SessionRenamed { session: SessionId(0), name: "demo2".into() }),
        ControlEvent::Reply(CommandReply { time: 1792572896, number: 321, from_client: true,
            success: false, body: vec![b"parse error: unknown command: bogus-command".to_vec()] }),
        n(UnlinkedWindowRenamed { window: WindowId(1), name: "zsh".into() }),
        ok(1792572897, 322, true, &[]),
        n(Exit { reason: None }),
    ]);
}

#[test]
fn extended_output_transcript() {
    // Expected events, in order:
    //   ok(315, server), SessionChanged{$0,"demo"}, ok(317), ok(318), WindowAdd(@1),
    //   Output { pane: %0, data: b"h\xc3\xa9\\\r\n\x1bPtmux;\x1b\x1b]777;notify;warp://cli-agent;{\"x\":1}\x07\x1b\\" },
    //   WindowRenamed{@0,"zsh"}, WindowClose(@1), Exit{None}
}

#[test]
fn capture_reply_transcript() {
    // Reply bodies are raw and keep empty lines:
    //   317 → [b"ab", b"cd", b""],
    //   318 → [b""],
    //   319 → [b"2 1 0"],
    //   321 → [b"30 4"].
    // Notifications in between: WindowRenamed{@0,"sleep"}, LayoutChange{@0,"e9dd,30x4,0,0,0"}, then Exit{None}.
}

#[test]
fn body_lines_that_look_like_notifications_stay_body() {
    let mut parser = ControlParser::new();
    for line in [&b"%begin 1 5 1"[..], b"%1", b"%output %1 x", b"%end 1 4 1"] {
        assert_eq!(parser.push_line(line), None);
    }
    assert_eq!(parser.push_line(b"%end 1 5 1"), Some(ControlEvent::Reply(CommandReply {
        time: 1, number: 5, from_client: true, success: true,
        body: vec![b"%1".to_vec(), b"%output %1 x".to_vec(), b"%end 1 4 1".to_vec()],
    })));
}

#[test]
fn small_notifications() {
    let mut p = ControlParser::new();
    let n = |e| Some(ControlEvent::Notification(e));
    assert_eq!(p.push_line(b"%exit detached"), n(Notification::Exit { reason: Some("detached".into()) }));
    assert_eq!(p.push_line(b"%pause %3"), n(Notification::Pause(PaneId(3))));
    assert_eq!(p.push_line(b"%continue %3"), n(Notification::Continue(PaneId(3))));
    assert_eq!(p.push_line(b"%window-renamed @2 two words"),
        n(Notification::WindowRenamed { window: WindowId(2), name: "two words".into() }));
    assert_eq!(p.push_line(b"%window-add oops"), n(Notification::Other("%window-add oops".into())));
    assert_eq!(p.push_line(b"%client-detached /dev/x"), n(Notification::Other("%client-detached /dev/x".into())));
}

#[test]
fn ready_only_after_the_first_block_closes() {
    let mut p = ControlParser::new();
    assert!(!p.is_ready());
    p.push_line(b"%begin 1 315 0");
    assert!(!p.is_ready());
    p.push_line(b"%end 1 315 0");
    assert!(p.is_ready());
}
```

```rust
// queue_tests.rs, with a helper `reply(from_client: bool) -> CommandReply`
#[test]
fn replies_pop_in_order_and_server_blocks_pop_nothing() {
    let mut q = CommandQueue::new();
    q.push("a");
    q.push("b");
    assert_eq!(q.complete(&reply(false)), None);
    assert_eq!(q.complete(&reply(true)), Some("a"));
    assert_eq!(q.complete(&reply(true)), Some("b"));
    assert_eq!(q.complete(&reply(true)), None);
}
```

- [ ] **Step 3: Run the tests to verify they fail**

  Run: `cargo nextest run -p tmux_control parser queue`
  Expected: compile errors: `ControlParser`, `CommandQueue` not found.

- [ ] **Step 4: Implement `parser.rs` and `queue.rs`**

  - `%begin <time> <number> <flags>` opens a block, with `from_client = flags & 1 != 0`.
  - Inside a block, only the matching `%end`/`%error` line closes it (same three numbers). Every other line is pushed to the body raw.
  - Outside a block, notifications are parsed:
    - names are taken lossily as UTF-8 and run to the end of the line;
    - `%layout-change` keeps the first layout field only;
    - `%extended-output %P <age> … : <data>` takes the data after the first `" : "`;
    - a known name with malformed arguments, or an unknown name, becomes `Other(line)`.
  - `CommandQueue::complete` returns `None` for `!reply.from_client`, and otherwise `pop_front`.
  - Add `mod parser; mod queue;` to `lib.rs`, with `pub` on `parser`.

- [ ] **Step 5: Run the tests to verify they pass**

  Run: `cargo nextest run -p tmux_control`
  Expected: all 14 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tmux_control
git commit -m "Parse tmux control-mode lines and pair replies to commands"
```

---

### Task 3: Command builders and pane restore

**Files:**
- Create: `crates/tmux_control/src/commands.rs`
- Create: `crates/tmux_control/src/restore.rs`
- Test: `crates/tmux_control/src/commands_tests.rs`
- Test: `crates/tmux_control/src/restore_tests.rs`

**Interfaces:**
- Consumes: the ids (Task 1).
- Produces. In `commands`, each builder returns one command line without the trailing `\n`:
  - `quote(arg: &str) -> String`,
  - `MIN_VERSION: (u32, u32) = (3, 4)`,
  - `version_query() -> String`,
  - `parse_version(version: &str) -> Option<(u32, u32)>`,
  - `SEND_KEYS_CHUNK: usize = 1000`,
  - `send_keys(pane: PaneId, bytes: &[u8]) -> Vec<String>`,
  - `resize_window(window: WindowId, columns: usize, rows: usize) -> String`,
  - `PAUSE_AFTER_SECS: u32 = 5`,
  - `pause_after(seconds: u32) -> String`,
  - `pause_pane(pane: PaneId) -> String`,
  - `continue_pane(pane: PaneId) -> String`,
  - `detach_client() -> String`,
  - `list_windows() -> String`,
  - `describe_window(window: WindowId) -> String`,
  - `#[derive(Clone, Debug, PartialEq)] pub struct WindowInfo { pub window: WindowId, pub pane: PaneId, pub name: String }`,
  - `parse_window_line(line: &[u8]) -> Option<WindowInfo>`,
  - `restore_pane(pane: PaneId) -> String`,
  - `RESTORE_REPLIES: usize = 6`.
- Produces. In `restore`:
  - `STATE_FORMAT: &str`,
  - `#[derive(Clone, Debug, Default, PartialEq)] pub struct PaneState` with fields, in format order:
    - `cursor_x: usize`, `cursor_y: usize`,
    - `alternate_on: bool`, `alternate_saved_x: usize`, `alternate_saved_y: usize`,
    - `history_size: usize`, `pane_height: usize`,
    - `bracketed_paste: bool`, `keypad_cursor: bool`, `keypad: bool`,
    - `mouse_standard: bool`, `mouse_button: bool`, `mouse_any: bool`, `mouse_sgr: bool`,
    - `cursor_visible: bool`,
    - `scroll_region_upper: usize`, `scroll_region_lower: usize`;
  - `PaneState::parse(line: &[u8]) -> Option<PaneState>`,
  - `pub struct PaneCapture { pub state: PaneState, pub history: Vec<Vec<u8>>, pub visible: Vec<Vec<u8>>, pub saved_normal: Vec<Vec<u8>> }`,
  - `restore_bytes(capture: &PaneCapture) -> Vec<u8>`.

**Verified tmux 3.8 behavior these values encode:**
- In alternate mode, a plain capture is the normal history followed by the alt screen, and `-a` is the saved normal screen with no history.
- `capture-pane -S - -E -1` with no history returns the first visible row, not nothing. That is why `history` is used only when `history_size > 0`.

- [ ] **Step 1: Write the failing tests**

```rust
// commands_tests.rs
#[test]
fn quoting() {
    assert_eq!(quote("a b"), "'a b'");
    assert_eq!(quote("it's"), r"'it'\''s'");
}

#[test]
fn versions() {
    assert_eq!(parse_version("3.8"), Some((3, 8)));
    assert_eq!(parse_version("3.4a"), Some((3, 4)));
    assert_eq!(parse_version("3.10"), Some((3, 10)));
    assert_eq!(parse_version("next-3.6"), Some((3, 6)));
    assert_eq!(parse_version("openbsd-7.5"), None);
    assert_eq!(parse_version("master"), None);
    assert_eq!(version_query(), "display-message -p '#{version}'");
}

#[test]
fn send_keys_hex_in_chunks_of_1000() {
    assert_eq!(send_keys(PaneId(3), b"ab\x1b"), vec!["send-keys -t %3 -H 61 62 1b"]);
    let lines = send_keys(PaneId(3), &[b'a'; 2500]);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2].matches(" 61").count(), 500);
    assert!(send_keys(PaneId(3), b"").is_empty());
}

#[test]
fn fixed_commands() {
    assert_eq!(resize_window(WindowId(2), 120, 40), "refresh-client -C @2:120x40");
    assert_eq!(pause_after(PAUSE_AFTER_SECS), "refresh-client -f pause-after=5");
    assert_eq!(pause_pane(PaneId(3)), "refresh-client -A '%3:pause'");
    assert_eq!(continue_pane(PaneId(3)), "refresh-client -A '%3:continue'");
    assert_eq!(detach_client(), "detach-client");
    assert_eq!(list_windows(), "list-windows -F '#{window_id} #{pane_id} #{window_name}'");
    assert_eq!(describe_window(WindowId(4)),
        "display-message -p -t @4 '#{window_id} #{pane_id} #{window_name}'");
    assert_eq!(restore_pane(PaneId(3)), format!(
        "refresh-client -A '%3:pause' ; display-message -p -t %3 '{STATE_FORMAT}' ; \
         capture-pane -peqJ -t %3 -S - -E -1 ; capture-pane -peq -t %3 ; \
         capture-pane -peq -a -t %3 ; refresh-client -A '%3:continue'"));
}

#[test]
fn window_lines() {
    assert_eq!(parse_window_line(b"@0 %0 sleep 1"),
        Some(WindowInfo { window: WindowId(0), pane: PaneId(0), name: "sleep 1".into() }));
    assert_eq!(parse_window_line(b"@0"), None);
}
```

```rust
// restore_tests.rs
const NORMAL_STATE: &[u8] = b"2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2";

#[test]
fn state_format_and_parse() {
    assert_eq!(STATE_FORMAT, "#{cursor_x} #{cursor_y} #{alternate_on} #{alternate_saved_x} \
        #{alternate_saved_y} #{history_size} #{pane_height} #{bracket_paste_flag} \
        #{keypad_cursor_flag} #{keypad_flag} #{mouse_standard_flag} #{mouse_button_flag} \
        #{mouse_any_flag} #{mouse_sgr_flag} #{cursor_flag} #{scroll_region_upper} \
        #{scroll_region_lower}");
    let state = PaneState::parse(NORMAL_STATE).unwrap();
    assert_eq!((state.cursor_x, state.cursor_y, state.pane_height, state.cursor_visible), (2, 1, 3, true));
    // An unknown format (older tmux) expands to empty and reads as 0/false.
    assert!(!PaneState::parse(b"2 1 0 0 0 0 3  0 0 0 0 0 0 1 0 2").unwrap().bracketed_paste);
    assert_eq!(PaneState::parse(b"2 1 0"), None);
}

#[test]
fn normal_screen_restore() {
    let capture = PaneCapture {
        state: PaneState::parse(NORMAL_STATE).unwrap(),
        history: vec![b"x".to_vec()], // ignored: history_size is 0
        visible: vec![b"ab".to_vec(), b"cd".to_vec(), b"".to_vec()],
        saved_normal: vec![],
    };
    assert_eq!(restore_bytes(&capture), b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H");
}

#[test]
fn alternate_screen_restore() {
    let capture = PaneCapture {
        state: PaneState::parse(b"3 0 1 2 2 1 3 0 0 0 0 0 0 0 1 0 2").unwrap(),
        history: vec![b"n1".to_vec()],
        visible: vec![b"ALT".to_vec(), b"".to_vec(), b"".to_vec()],
        saved_normal: vec![b"n2".to_vec(), b"n3".to_vec(), b"n4".to_vec()],
    };
    assert_eq!(restore_bytes(&capture),
        b"\x1b[H\x1b[2J\x1b[3Jn1\r\nn2\r\nn3\r\nn4\x1b[3;3H\x1b[?1049h\x1b[HALT\r\n\r\n\x1b[1;4H");
}

#[test]
fn modes_and_scroll_region_are_restored() {
    let capture = PaneCapture {
        state: PaneState::parse(b"0 0 0 0 0 0 3 1 0 0 0 0 1 1 0 1 2").unwrap(),
        history: vec![], visible: vec![b"".to_vec(); 3], saved_normal: vec![],
    };
    assert_eq!(restore_bytes(&capture),
        b"\x1b[H\x1b[2J\x1b[3J\r\n\r\n\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;3r\x1b[1;1H");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p tmux_control commands restore`
  Expected: compile errors: unresolved items in `commands` and `restore`.

- [ ] **Step 3: Implement `commands.rs` and `restore.rs`**

  `restore_bytes` emits, in this order:
  1. `ESC[H ESC[2J ESC[3J`.
  2. The normal screen:
     - when `history_size > 0`, the `history` lines;
     - then the screen: `saved_normal` if `alternate_on`, else `visible`;
     - all of them joined with `\r\n` and no trailing separator.
  3. If `alternate_on`:
     - `CSI (alternate_saved_y+1);(alternate_saved_x+1) H`,
     - `ESC[?1049h ESC[H`,
     - the `visible` lines joined with `\r\n`.
  4. Modes, each only when set, in this order:
     - `ESC[?2004h` (bracketed paste),
     - `ESC[?1h` (keypad cursor),
     - `ESC=` (keypad),
     - `ESC[?1000h`, `ESC[?1002h`, `ESC[?1003h`, `ESC[?1006h` (mouse standard, button, any, sgr),
     - `ESC[?25l` when the cursor is hidden.
  5. `CSI (upper+1);(lower+1) r` only when the scroll region isn't `0..pane_height-1`.
  6. `CSI (cursor_y+1);(cursor_x+1) H`.

  Add `pub mod commands; pub mod restore;` to `lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p tmux_control`
  Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tmux_control
git commit -m "Build tmux commands and rebuild a pane's screen from captures"
```

---

### Task 4: `Connection` core over any reader and writer

**Files:**
- Create: `crates/tmux_control/src/connection.rs`
- Create: `crates/tmux_control/src/testing.rs` (`#[cfg(any(test, feature = "test-util"))]`)
- Test: `crates/tmux_control/src/connection_tests.rs`

**Interfaces:**
- Consumes:
  - `ControlParser`, `Notification` and `CommandReply` (Task 2),
  - `CommandQueue` (Task 2),
  - every builder in `commands` (Task 3),
  - `PaneCapture`, `PaneState::parse` and `restore_bytes` (Task 3),
  - `PassthroughUnwrapper` is not used here; it runs per tab in Task 8.
- Produces (`tmux_control::connection`):
  - `pub struct ConnectionConfig { pub pane_channel_capacity: usize }`, with `Default` set to 256.
  - `#[derive(Clone)] pub struct Connection`.
  - `pub enum ConnectionEvent { Ready { version: String, session: SessionId, session_name: String }, Notification(Notification), Failed(ConnectionError), Closed { reason: Option<String> } }`. `Notification` here never carries `Output`, `Pause` or `Continue`; those are routed to panes.
  - `#[derive(Clone, Debug, thiserror::Error)] pub enum ConnectionError`:
    - `#[error("tmux {found} is too old; Tarp needs tmux 3.4 or newer")] VersionTooOld { found: String }`,
    - `#[error("tmux refused to attach: {0}")] AttachRefused(String)`,
    - `#[error("could not start tmux: {0}")] Spawn(String)`.
  - `#[derive(Debug, PartialEq)] pub enum PaneEvent { Output(Vec<u8>), Restored(Vec<u8>), Paused }`.
  - `pub type CommandResult = Result<Vec<Vec<u8>>, CommandError>`.
  - `#[derive(Clone, Debug, PartialEq, thiserror::Error)] pub enum CommandError { #[error("{0}")] Tmux(String), #[error("tmux connection closed")] Disconnected }`.
  - Methods on `Connection`:
    - `from_io(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, config: ConnectionConfig) -> (Connection, async_channel::Receiver<ConnectionEvent>)`
    - `command(&self, line: String) -> async_channel::Receiver<CommandResult>`
    - `subscribe_pane(&self, pane: PaneId) -> async_channel::Receiver<PaneEvent>`
    - `unsubscribe_pane(&self, pane: PaneId)`
    - `restore_pane(&self, pane: PaneId)`
    - `send_input(&self, pane: PaneId, bytes: &[u8])`
    - `resize_window(&self, window: WindowId, columns: usize, rows: usize)`
    - `resume_if_paused(&self, pane: PaneId) -> bool`
    - `detach(&self)`
  - `tmux_control::testing`:
    - `FakeTmux::start(config: ConnectionConfig) -> (FakeTmux, Connection, async_channel::Receiver<ConnectionEvent>)`, built on `std::io::pipe()`,
    - `FakeTmux::send(&mut self, line: &str)`,
    - `FakeTmux::read_command(&mut self) -> String` (panics after 5 s),
    - `FakeTmux::try_read_command(&mut self, wait: Duration) -> Option<String>`,
    - `FakeTmux::reply(&mut self, body: &[&str])` and `FakeTmux::reply_error(&mut self, body: &[&str])` (client-flagged blocks with increasing numbers),
    - `FakeTmux::handshake(&mut self, session: &str, version: &str)`: sends the server block and `%session-changed $0 <session>`, answers the version query and, when the version is accepted, the pause-after command,
    - `recv_timeout<T>(rx: &async_channel::Receiver<T>, wait: Duration) -> Option<T>`.

- [ ] **Step 1: Write the failing tests** (`connection_tests.rs`; `W = Duration::from_millis(200)`)

```rust
#[test]
fn nothing_is_written_before_the_first_block_and_app_commands_wait_for_the_probe() {
    let (mut tmux, connection, events) = FakeTmux::start(ConnectionConfig::default());
    let listed = connection.command(commands::list_windows());
    assert_eq!(tmux.try_read_command(W), None);
    tmux.send("%begin 1 315 0");
    tmux.send("%end 1 315 0");
    tmux.send("%session-changed $0 demo");
    assert_eq!(tmux.read_command(), "display-message -p '#{version}'");
    tmux.reply(&["3.8"]);
    assert_eq!(tmux.read_command(), "refresh-client -f pause-after=5");
    tmux.reply(&[]);
    assert_eq!(tmux.read_command(), commands::list_windows());
    tmux.reply(&["@0 %0 zsh"]);
    assert_eq!(recv_timeout(&listed, W), Some(Ok(vec![b"@0 %0 zsh".to_vec()])));
    assert!(matches!(recv_timeout(&events, W), Some(ConnectionEvent::Ready { version, session: SessionId(0), session_name })
        if version == "3.8" && session_name == "demo"));
}

#[test]
fn refuses_tmux_older_than_3_4() {
    // handshake("demo", "3.3a"):
    //   event Failed(VersionTooOld { found: "3.3a" }),
    //   next command "detach-client",
    //   a command queued earlier is never written (try_read_command(W) == None after that).
}

#[test]
fn unparseable_version_is_accepted() {
    // handshake("demo", "openbsd-7.5") → Ready { version: "openbsd-7.5", .. }
}

#[test]
fn attach_refused_when_the_first_block_is_an_error() {
    // send "%begin 1 315 0", "can't find session: nope", "%error 1 315 0", "%exit", then close the pipe:
    //   events: Failed(AttachRefused("can't find session: nope")), then Closed { reason: None };
    //   nothing is written.
}

#[test]
fn replies_resolve_in_order_and_errors_carry_the_tmux_message() {
    // After the handshake:
    //   a = command("a"), b = command("bogus-command");
    //   the fake sends a server block "%begin 5 400 0"/"%end 5 400 0", then reply(&["one"]), then
    //   reply_error(&["parse error: unknown command: bogus-command"]).
    //   a → Ok([b"one"]);
    //   b → Err(CommandError::Tmux("parse error: unknown command: bogus-command")).
}

#[test]
fn pane_output_is_unescaped_and_routed_to_its_subscriber() {
    // After the handshake, subscribe %3.
    //   send "%output %3 a\\033[1mb" → rx gets Output(b"a\x1b[1mb");
    //   send "%output %4 x" (no subscriber) → dropped, nothing breaks.
}

#[test]
fn restore_is_one_line_and_delivers_restored_before_later_output() {
    // After the handshake: rx = subscribe_pane(%0); restore_pane(%0).
    //   read_command() == commands::restore_pane(PaneId(0)).
    //   The fake sends:
    //     "%output %0 stale"                                     (dropped)
    //     reply(&[])                                             (pause)
    //     "%pause %0"                                            (ignored while restoring)
    //     reply(&["2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2"])          (state)
    //     reply(&["ignored"])                                    (history)
    //     reply(&["ab", "cd", ""])                               (visible)
    //     reply(&[])                                             (saved normal)
    //     reply(&[])                                             (continue)
    //     "%continue %0"
    //     "%output %0 live"
    //   rx yields exactly:
    //     Restored(b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H"),
    //     Output(b"live"),
    //   and then nothing within W.
}

#[test]
fn input_becomes_send_keys_chunks() {
    // send_input(%3, &[b'a'; 1500]) → two commands, the first with 1000 " 61", the second with 500.
}

#[test]
fn resize_is_refresh_client() {
    // resize_window(@2, 120, 40) → "refresh-client -C @2:120x40"
}

#[test]
fn a_full_pane_channel_pauses_the_pane_until_drained() {
    // config.pane_channel_capacity = 2; after the handshake, subscribe %1.
    //   Three "%output %1 x" lines → read_command() == "refresh-client -A '%1:pause'".
    //   rx holds exactly 2 Output(b"x").
    //   resume_if_paused(%1) == true and read_command() == restore_pane(%1).
    //   A second resume_if_paused(%1) == false.
}

#[test]
fn tmux_pause_wakes_the_pane() {
    // subscribe %1; send "%pause %1" → rx gets Paused; resume_if_paused(%1) == true.
}

#[test]
fn eof_fails_pending_commands_and_reports_the_exit_reason() {
    // After the handshake: r = command("x"); send "%exit detached"; close the pipe.
    //   r → Err(CommandError::Disconnected);
    //   events → Closed { reason: Some("detached") };
    //   pane receivers are closed.
}

#[test]
fn unsubscribe_closes_the_pane_receiver() {
    // subscribe %1, unsubscribe %1 → rx.recv_blocking() is Err.
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p tmux_control connection`
  Expected: compile errors: `Connection`, `FakeTmux` not found.

- [ ] **Step 3: Implement `connection.rs` and `testing.rs`**

  Shared state is `Arc<parking_lot::Mutex<Shared>>`, with `Shared { queue: CommandQueue<Tag>, panes: HashMap<PaneId, PaneRoute>, … }`.

  The private `Tag` enum has these variants:
  - `Ignore`,
  - `Reply(async_channel::Sender<CommandResult>)`,
  - `Version`,
  - `Restore { pane, part }`, where `part` is `Pause`, `State`, `History`, `Visible`, `SavedNormal` or `Continue`.

  The writer thread reads a `std::sync::mpsc` of `WriterMsg`:
  - `Probe(line, tag)` is written immediately once ready.
  - `Line { line, tags }` is held until `Release`.
  - `Release { first: Vec<(String, Tag)> }`.
  - `Shutdown`.

  The tags of a line are pushed under the same lock that writes it, so the queue order is always the write order.

  The reader thread runs `BufReader::read_until(b'\n')` and calls `ControlParser::push_line`:
  - **First block `%error`.** Emit `Failed(AttachRefused(body joined with "\n"))`.
  - **First block `%end`.** Send `Probe(version_query, Version)`.
  - **Version reply.** If the version is too old: `Failed(VersionTooOld)` and probe `detach-client`. Otherwise:
    - send `Release { first: [pause_after(PAUSE_AFTER_SECS), Ignore] }`;
    - emit `Ready` once the `%session-changed` session is known (it arrives before the version reply).
  - **Output for a `Live` pane.** `try_send`. On `Full`, set `paused` and send `pause_pane` (`Ignore`). While `paused`, drop output.
  - **Output for a `Restoring` pane.** Drop it. The pause in the restore line means everything before it is in the capture and nothing new is sent until `continue`.
  - **`Pause(P)` while Live.** Set `paused` and `try_send(Paused)`. `Continue` is ignored.
  - **Restore parts.** Fill a `PaneCapture`. On `Continue` (the 6th reply), `try_send(Restored(restore_bytes))` and go back to `Live`. An error reply in any part aborts back to `Live` with a `log::warn!`.
  - **Other notifications.** Forward as `ConnectionEvent::Notification`.
  - **EOF.**
    - Every queued `Reply` tag gets `Err(Disconnected)`.
    - Every pane sender is dropped.
    - Emit `Closed { reason }`. The reason is the `%exit` reason, or else the trimmed text read from the optional stderr reader (Task 5), or `None`.

  Methods:
  - `restore_pane` marks the pane `Restoring` and then queues `restore_pane(pane)` with the six `Restore` tags.
  - `resume_if_paused` clears `paused` and calls `restore_pane`.
  - `detach` queues `detach_client()`.

  `lib.rs` gets `pub mod connection;` and `#[cfg(any(test, feature = "test-util"))] pub mod testing;`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p tmux_control`
  Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tmux_control
git commit -m "Add tmux control Connection: gated writer, reply queue, per-pane demux, restore"
```

---

### Task 5: Spawn `tmux -C` and test against a real private server

**Files:**
- Create: `crates/tmux_control/src/binary.rs`
- Modify: `crates/tmux_control/src/connection.rs` (add `SpawnConfig`, `Connection::spawn`)
- Test: `crates/tmux_control/src/spawn_tests.rs`
- Modify: `app/src/workspace/view/tab_grouping.rs:1996-2001` (delete the private `tmux_binary`; `use tmux_control::tmux_binary;` inside `mod tmux_agents`)
- Modify: `app/Cargo.toml` (`tmux_control.workspace = true` under `[target.'cfg(not(target_family = "wasm"))'.dependencies]` at line 304; `tmux_control = { workspace = true, features = ["test-util"] }` under the matching dev-dependencies at line 447)

**Interfaces:**
- Consumes: `Connection::from_io` internals (Task 4).
- Produces:
  - `tmux_control::tmux_binary() -> &'static str`, moved verbatim: `/opt/homebrew/bin/tmux`, then `/usr/local/bin/tmux`, then `tmux`.
  - `pub struct SpawnConfig { pub binary: String, pub socket_name: Option<String>, pub target: Option<String>, pub connection: ConnectionConfig }`.
  - `Connection::spawn(config: SpawnConfig) -> Result<(Connection, async_channel::Receiver<ConnectionEvent>), ConnectionError>`.

- [ ] **Step 1: Write the failing tests**

  `spawn_tests.rs` has a helper, `struct PrivateServer { socket: String }`:
  - `fn start(name: &str, width: u16, height: u16, command: &str) -> Option<PrivateServer>` runs `tmux -L tarp-test-<pid>-<name> -f /dev/null new-session -d -s demo -x W -y H <command>`. It returns `None` (after printing `skipping: tmux not installed`) when `tmux_binary() -V` fails.
  - `Drop` runs `kill-server`.
  - `fn attach(&self) -> (Connection, Receiver<ConnectionEvent>)` spawns with `target: Some("demo".into())` and waits for `Ready`.

```rust
#[test]
fn attaches_and_follows_window_add_rename_and_close() {
    let Some(server) = PrivateServer::start("windows", 40, 5, "sleep 30") else { return };
    let (connection, events) = server.attach();
    let windows = recv_timeout(&connection.command(commands::list_windows()), SECS).unwrap().unwrap();
    assert_eq!(windows.len(), 1);
    assert!(windows[0].starts_with(b"@0 %0 "));
    recv_timeout(&connection.command("new-window -d -n second 'sleep 30'".into()), SECS);
    // events within SECS include Notification(WindowAdd(WindowId(1)))
    recv_timeout(&connection.command("rename-window -t @1 third".into()), SECS);
    // ... WindowRenamed { window: WindowId(1), name: "third" }
    recv_timeout(&connection.command("kill-window -t @1".into()), SECS);
    // ... WindowClose(WindowId(1))
}

#[test]
fn restore_reproduces_screen_and_cursor() {
    let Some(server) = PrivateServer::start("restore", 20, 3, "printf 'ab\\ncd'; sleep 30") else { return };
    let (connection, _events) = server.attach();
    let rx = connection.subscribe_pane(PaneId(0));
    connection.resize_window(WindowId(0), 20, 3);
    connection.restore_pane(PaneId(0));
    assert_eq!(recv_timeout(&rx, SECS),
        Some(PaneEvent::Restored(b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H".to_vec())));
}

#[test]
fn input_reaches_the_pane() {
    // The server runs "cat". Subscribe to %0, then send_input(%0, b"hi\r").
    // Output events within SECS, concatenated, contain b"hi".
}

#[test]
fn attach_refused_reports_tmux_reason() {
    // The server is started, but spawn uses target "nope".
    // events: Failed(AttachRefused("can't find session: nope")).
}

#[test]
fn restore_then_live_output_has_no_duplicate_lines() {
    // The server runs: "i=0; while [ $i -lt 3000 ]; do echo L$i; i=$((i+1)); done; sleep 30" at 40x5.
    // Attach immediately: subscribe %0, resize 40x5, restore %0.
    // Collect Restored and Output bytes for 3 s and split on '\n' after removing '\r' and escapes.
    // The L<n> numbers seen are strictly increasing (no number appears twice).
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p tmux_control spawn`
  Expected: compile errors: `SpawnConfig`, `Connection::spawn` and `tmux_binary` not found.

- [ ] **Step 3: Implement `spawn` and `binary.rs`; switch `tab_grouping.rs` to the moved function**

  `spawn` builds `Command::new(binary)`:
  - arguments `[-L socket] -C attach [-t target]`,
  - `env_remove("TMUX")` and `env_remove("TMUX_PANE")`,
  - stdin, stdout and stderr all piped.

  It hands stdout and stdin to the Task 4 core, along with stderr as the optional reason source. A reaper thread calls `child.wait()`. Spawn I/O errors map to `ConnectionError::Spawn(err.to_string())`.

- [ ] **Step 4: Run the tests to verify they pass, and check that the old sync still builds**

  Run: `cargo nextest run -p tmux_control && cargo check -p warp`
  Expected: all pass (the tmux tests are skipped where tmux is missing), and `warp` compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/tmux_control app/Cargo.toml app/src/workspace/view/tab_grouping.rs Cargo.lock
git commit -m "Spawn tmux -C attach over pipes; share tmux_binary with the old sync"
```

---

### Task 6: Feature flag, setting and Command Palette toggle

**Files:**
- Modify: `crates/warp_features/src/lib.rs` (enum `FeatureFlag` at line 8: add `TmuxControlMode` after `PinnedTabs`, line ~893; add to `DOGFOOD_FLAGS` at line 1031)
- Modify: `app/Cargo.toml` (`tmux_control_mode = []` next to `pluggable_notifications = []` at line 818; add `"tmux_control_mode"` to `default` after `"pinned_tabs"`, line 672)
- Modify: `app/src/features.rs:460` (`#[cfg(feature = "tmux_control_mode")] FeatureFlag::TmuxControlMode,` after `GroupedTabs`)
- Modify: `app/src/workspace/tab_settings.rs:634` (new setting after `tmux_session_script`)
- Modify: `app/src/settings_view/mod.rs:493` (`flags::TMUX_CONTROL_MODE_CONTEXT_FLAG`)
- Modify: `app/src/workspace/action.rs` (`WorkspaceAction::ToggleTmuxControlMode`; add to the `false` arm of `should_save_app_state_on_action`)
- Modify: `app/src/workspace/mod.rs` (register the toggle pair after the tab-pinning bindings, ~line 1090)
- Modify: `app/src/workspace/view.rs`:
  - `add_toggle_setting_context_flags` (~23917),
  - the `TabSettingsChangedEvent` match (~4013),
  - the action match in `handle_action` (24775).
- Test: `app/src/workspace/view_tests.rs`

**Interfaces:**
- Produces:
  - `FeatureFlag::TmuxControlMode`.
  - `TabSettings::tmux_control_mode` (type `TmuxControlMode`, `bool`, default `false`):
    - `toml_path: "integrations.tmux.control_mode"`,
    - `storage_key: "IntegrationsTmuxControlMode"`,
    - `sync_to_cloud: SyncToCloud::Never`,
    - `surface: GUI`,
    - description: `"Whether tmux sessions open as tabs over tmux control mode (one tab per tmux window)."`.
  - `flags::TMUX_CONTROL_MODE_CONTEXT_FLAG: &str = "Tmux_Control_Mode"`.
  - `WorkspaceAction::ToggleTmuxControlMode`.
  - `fn tmux_control_mode_enabled(ctx: &AppContext) -> bool` in `app/src/terminal/tmux_control/mod.rs` (created here, module declared in `app/src/terminal/mod.rs` under `#[cfg(not(target_family = "wasm"))]`). It returns `FeatureFlag::TmuxControlMode.is_enabled() && *TabSettings::as_ref(ctx).tmux_control_mode.value()`.

The flag goes in both `DOGFOOD_FLAGS` (per the spec) and a default cargo feature. Tarp's own builds are `warp-oss`, not dogfood, so without the cargo feature the code would be unreachable for the owner. The setting stays the owner's switch.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(not(target_family = "wasm"))]
#[test]
fn test_toggle_tmux_control_mode_flips_the_setting_and_context_flag() {
    let _flag = FeatureFlag::TmuxControlMode.override_enabled(true);
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let workspace = mock_workspace(&mut app);
        workspace.update(&mut app, |workspace, ctx| {
            assert!(!tmux_control_mode_enabled(ctx));
            workspace.handle_action(&WorkspaceAction::ToggleTmuxControlMode, ctx);
            assert!(tmux_control_mode_enabled(ctx));
            assert!(workspace.keymap_context(ctx).set.contains(flags::TMUX_CONTROL_MODE_CONTEXT_FLAG));
        });
    });
}
```

- [ ] **Step 2: Run the test to verify it fails**

  Run: `cargo nextest run -p warp test_toggle_tmux_control_mode`
  Expected: compile error: no variant `TmuxControlMode`.

- [ ] **Step 3: Implement the changes**

  - Register the pair in `workspace/mod.rs`, which puts "Enable tmux control mode" and "Disable tmux control mode" in the palette:

    ```rust
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(vec![
        ToggleSettingActionPair::new("tmux control mode", WorkspaceAction::ToggleTmuxControlMode,
            &id!("Workspace"), flags::TMUX_CONTROL_MODE_CONTEXT_FLAG)
            .with_enabled(|| FeatureFlag::TmuxControlMode.is_enabled()),
    ], app);
    ```

  - In `add_toggle_setting_context_flags`, insert the flag when `*TabSettings::as_ref(app).tmux_control_mode.value()`.
  - The handler flips the value the same way `ToggleVerticalTabsShowDetailsOnHover` does (view.rs:25873).
  - The `TmuxControlMode { .. }` settings event joins the existing `TmuxSyncEnabled | TmuxSessionScript => {}` arm.

- [ ] **Step 4: Run the test to verify it passes**

  Run: `cargo nextest run -p warp test_toggle_tmux_control_mode`
  Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/warp_features app/Cargo.toml app/src/features.rs app/src/workspace app/src/settings_view/mod.rs app/src/terminal/mod.rs app/src/terminal/tmux_control/mod.rs
git commit -m "Add TmuxControlMode flag, integrations.tmux.control_mode setting and palette toggle"
```

---

### Task 7: Raw mode for terminal models that never bootstrap

**Files:**
- Modify: `app/src/terminal/model/terminal_model.rs`:
  - new field next to `ignore_bootstrapping_messages` (line 437) and its initializer (line 1103),
  - methods next to `ignore_bootstrapping_messages()` (line 1502),
  - early returns in `precmd_with_completion_metadata` (3036) and `preexec` (3075).
- Modify: `app/src/terminal/model/blocks.rs` (new `enter_raw_mode` next to `set_bootstrapped`, line 3099)
- Modify: `app/src/terminal/model/test_utils.rs` (`TerminalModel::new_raw_for_test`)
- Modify: `app/src/terminal/view.rs`:
  - `is_long_running` (9599),
  - `should_write_typed_chars_to_pty` (9854),
  - `write_to_pty_for_syncing_long_running_commands` (10040),
  - `is_input_box_visible` (8835),
  - `on_bootstrap_failed_timer_complete` (16917).
- Modify: `app/src/session_management.rs:101` and `app/src/pane_group/pane/terminal_pane.rs:225` (`is_raw`)
- Test: `app/src/terminal/model/terminal_model_tests.rs`, `app/src/terminal/view_tests.rs`, `app/src/session_management_tests.rs` (create it with the `#[path]` include if absent)

**Interfaces:**
- Produces:
  - `TerminalModel::enter_raw_mode(&mut self)` and `TerminalModel::is_raw(&self) -> bool`.
  - `BlockList::enter_raw_mode(&mut self)` (`pub(super)`).
  - `SessionNavigationData::new(…, is_read_only: bool, is_raw: bool, window_id, …)` and `SessionNavigationData::is_raw(&self) -> bool`.
- `BlockList::enter_raw_mode` does this:
  1. `active_block_mut().finish(0)` on the WarpInput block.
  2. `self.bootstrap_stage = BootstrapStage::PostBootstrapPrecmd`.
  3. `create_new_block(BlockId::new(), BootstrapStage::PostBootstrapPrecmd, None, None)`.
  4. `start_active_block()`.
  5. `apply_preexec_to_active(PreexecValue::default())`.
  6. `active_block_mut().set_should_hide_command_grid(true)`.
- `TerminalModel::enter_raw_mode` calls `ignore_bootstrapping_messages()`, sets `is_raw` and calls `block_list.enter_raw_mode()`.

- [ ] **Step 1: Write the failing tests**

```rust
// terminal_model_tests.rs
#[test]
fn raw_mode_shows_output_in_an_executing_post_bootstrap_block() {
    let mut model = TerminalModel::new_raw_for_test();
    Processor::default().parse_bytes(&mut model, b"hello\r\n", &mut std::io::sink());
    let block = model.block_list().active_block();
    assert!(model.is_raw());
    assert!(model.block_list().is_bootstrapping_precmd_done());
    assert_eq!(block.bootstrap_stage(), BootstrapStage::PostBootstrapPrecmd);
    assert_eq!(block.state(), BlockState::Executing);
    assert_eq!(block.output_to_string(), "hello");
}

#[test]
fn raw_mode_ignores_shell_integration_hooks() {
    let mut model = TerminalModel::new_raw_for_test();
    let before = model.block_list().active_block().id().clone();
    model.precmd_with_completion_metadata(PrecmdValue::default());
    model.preexec(PreexecValue::default());
    assert_eq!(model.block_list().active_block().id(), &before);
}

// view_tests.rs
#[test]
fn raw_mode_routes_keys_to_the_pty_and_hides_the_input_box() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        terminal.update(&mut app, |view, ctx| {
            view.model.lock().enter_raw_mode();
            assert!(view.is_long_running());
            assert!(view.should_write_typed_chars_to_pty(ctx));
            let model = view.model.lock();
            assert!(!view.is_input_box_visible(&model, ctx));
        });
    });
}

// session_management_tests.rs
#[test]
fn running_summary_skips_raw_sessions() {
    // Two SessionNavigationData with CommandContext::RunningCommand: one is_raw = true, one false.
    // RunningSessionSummary::new(&sessions).long_running_cmds.len() == 1
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p warp raw_mode running_summary_skips_raw_sessions`
  Expected: compile errors: no method `enter_raw_mode` / `new_raw_for_test` / `is_raw`.

- [ ] **Step 3: Implement**

  - `new_raw_for_test` builds with `TerminalModel::new(...)`, using the same arguments as `new_for_test` minus the fake bootstrap, then calls `enter_raw_mode()`.
  - In the view, OR `model.is_raw()` into `is_long_running`, `should_write_typed_chars_to_pty` and `write_to_pty_for_syncing_long_running_commands`.
  - `is_input_box_visible` returns `false` first when raw.
  - `on_bootstrap_failed_timer_complete` returns early when raw.
  - `RunningSessionSummary::new` also filters `!session.is_raw()`.
  - `terminal_pane.rs` passes `view.model.lock().is_raw()`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p warp raw_mode running_summary_skips_raw_sessions`
  Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add app/src/terminal app/src/session_management.rs app/src/session_management_tests.rs app/src/pane_group/pane/terminal_pane.rs
git commit -m "Add raw mode: a terminal model that skips bootstrap and passes input through"
```

---

### Task 8: tmux tab backend (`TerminalManager` and event loop)

**Files:**
- Create: `app/src/terminal/tmux_control/terminal_manager.rs`
- Create: `app/src/terminal/tmux_control/event_loop.rs`
- Modify: `app/src/terminal/tmux_control/mod.rs` (`mod terminal_manager; mod event_loop; mod sessions;` and re-exports)
- Modify: `app/src/pane_group/mod.rs`:
  - new `new_for_tmux_window`, modelled on `new_for_shared_session_viewer` (3566),
  - new `set_custom_title` next to `set_title` (5253).
- Test: `app/src/terminal/tmux_control/event_loop_tests.rs`

**Interfaces:**
- Consumes:
  - from Task 4: `Connection`, `PaneEvent`, and `subscribe_pane` / `resize_window` / `restore_pane` / `send_input` / `resume_if_paused` / `unsubscribe_pane`;
  - from Task 1: `PassthroughUnwrapper`;
  - from Task 7: `TerminalModel::enter_raw_mode`;
  - `TmuxControlSessions::release_window` (Task 9; until Task 9 lands, call a `todo`-free no-op stub with the same signature in `sessions.rs`).
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct TmuxSessionKey { pub session: SessionId }`, in `sessions.rs`. Phase 3 adds a host field.
  - `#[derive(Clone)] pub struct TmuxPaneBinding { pub connection: Connection, pub key: TmuxSessionKey, pub window: WindowId, pub pane: PaneId }`.
  - `TmuxTerminalManager::create_model(binding: TmuxPaneBinding, resources: TerminalViewResources, initial_size: Vector2F, model_event_sender: Option<SyncSender<ModelEvent>>, window_id: WindowId, ctx: &mut AppContext) -> (ViewHandle<TerminalView>, ModelHandle<Box<dyn TerminalManager>>)`.
  - `PaneGroup::new_for_tmux_window(binding: TmuxPaneBinding, tips_completed: ModelHandle<TipsCompleted>, user_default_shell_unsupported_banner_model_handle: ModelHandle<BannerState>, server_api: Arc<ServerApi>, model_event_sender: Option<SyncSender<ModelEvent>>, ctx: &mut ViewContext<Self>) -> Self`.
  - `PaneGroup::set_custom_title(&mut self, title: Option<String>, ctx: &mut ViewContext<Self>)`. Unlike `set_title`, it does not refocus.
  - `pub(super) fn run_output(model: Arc<FairMutex<TerminalModel>>, listener: ChannelEventListener, binding: TmuxPaneBinding, events: async_channel::Receiver<PaneEvent>) -> impl Future<Output = ()>`.
  - `pub(super) fn run_input(binding: TmuxPaneBinding, messages: async_channel::Receiver<Message>) -> impl Future<Output = ()>`.

- [ ] **Step 1: Write the failing tests** (`event_loop_tests.rs`, driving the free functions with `FakeTmux` and `futures::executor::block_on`)

```rust
#[test]
fn restored_and_live_output_reach_the_model_unwrapped() {
    // model = TerminalModel::new_raw_for_test(); events = bounded(8);
    // send Restored(b"\x1b[H\x1b[2J\x1b[3Jab"), Output(b"\x1bPtmux;\x1b\x1b]0;t\x07\x1b\\cd"), then close.
    // After block_on(run_output(..)): the active block's output_to_string() == "abcd",
    // and terminal_title() == Some("t").
}

#[test]
fn output_queries_are_not_answered() {
    // Output(b"\x1b[c\x1b[6n") goes through run_output.
    // FakeTmux::try_read_command(W) == None: tmux already answered the pane.
}

#[test]
fn input_and_resize_become_tmux_commands() {
    // run_input with Message::Input(b"hi".into()) then Message::Resize(size with columns 100, rows 30), then Shutdown.
    //   read_command() == "send-keys -t %0 -H 68 69"
    //   read_command() == "refresh-client -C @0:100x30"
    // After Shutdown the pane receiver from subscribe_pane is closed.
}

#[test]
fn a_drained_paused_pane_is_resumed() {
    // subscribe with capacity 1 and overflow it (see the Task 4 test); run_output drains.
    // read_command() == commands::restore_pane(PaneId(0)).
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p warp tmux_control::event_loop`
  Expected: compile errors: `run_output` / `run_input` not found.

- [ ] **Step 3: Implement**

  `create_model` follows `remote_tty::TerminalManager::create_model` (`app/src/terminal/remote_tty/terminal_manager.rs:51-160`) line for line, except:
  - It calls `model.enter_raw_mode()` right after `create_terminal_model`.
  - It does not write env vars or the zsh init script.
  - The event loop is a `TmuxEventLoop` model whose constructor runs, in order:
    1. `subscribe_pane`,
    2. `resize_window(window, size_info.columns, size_info.rows)`,
    3. `restore_pane`,
    4. then it spawns `run_output` and `run_input` on `ctx.background_executor()`, `.detach()`.

  `run_output` loop:
  - `Output` or `Restored`: `unwrapper.push`, lock the model, `Processor::parse_bytes(&mut *model, &buf, &mut io::sink())`, unlock, `send_wakeup_event()`.
  - `Paused`: nothing.
  - After each event, `if events.is_empty() { binding.connection.resume_if_paused(binding.pane); }`.

  The `io::sink()` gets the one "why" comment here: tmux is the pane's terminal and has already answered its queries.

  `run_input` maps messages:
  - `Input` → `send_input`.
  - `Resize` → `resize_window`.
  - `Shutdown` → `unsubscribe_pane` and stop.
  - `ChildExited` → nothing.

  `TerminalManager::on_view_detached` matches all three cases:
  - `DetachType::Closed`: `unsubscribe_pane`, then `TmuxControlSessions::handle(app).update(app, |sessions, ctx| sessions.release_window(binding.key, binding.window, ctx))`.
  - `DetachType::HiddenForClose | DetachType::Moved`: nothing.

  `PaneGroup::new_for_tmux_window` copies `new_for_shared_session_viewer`, replacing `create_shared_session_viewer` with `TmuxTerminalManager::create_model(binding, resources, view_bounds.size(), model_event_sender_clone, ctx.window_id(), ctx)`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p warp tmux_control::event_loop`
  Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add app/src/terminal/tmux_control app/src/pane_group/mod.rs
git commit -m "Add tmux window tab backend modelled on remote_tty"
```

---

### Task 9: `TmuxControlSessions`, the app-wide connection owner

**Files:**
- Modify: `app/src/terminal/tmux_control/sessions.rs` (replaces the Task 8 stub)
- Modify: `app/src/lib.rs:2318` (`#[cfg(not(target_family = "wasm"))] ctx.add_singleton_model(|_| TmuxControlSessions::new());` after `CLIAgentSessionsModel`)
- Modify: `app/src/workspace/view_tests.rs:102` (`initialize_app_with_team_client` registers the same singleton)
- Test: `app/src/terminal/tmux_control/sessions_tests.rs`

**Interfaces:**
- Consumes:
  - from Task 5: `Connection::spawn`, `SpawnConfig` and `tmux_binary`;
  - from Task 3: `commands::{list_windows, describe_window, parse_window_line, WindowInfo}`;
  - `ConnectionEvent` (Task 4).
- Produces:
  - `pub struct AttachTarget { pub socket_name: Option<String>, pub session: Option<String> }`.
  - `pub enum TmuxControlEvent`:
    - `Attached { key: TmuxSessionKey, owner_window: WindowId, session_name: String, connection: Connection, windows: Vec<WindowInfo> }`,
    - `WindowAdded { key: TmuxSessionKey, owner_window: WindowId, connection: Connection, window: WindowInfo }`,
    - `WindowClosed { key: TmuxSessionKey, window: WindowId }`,
    - `WindowRenamed { key: TmuxSessionKey, window: WindowId, name: String }`,
    - `SessionRenamed { key: TmuxSessionKey, name: String }`,
    - `Detached { key: TmuxSessionKey, reason: Option<String> }`,
    - `AttachFailed { owner_window: WindowId, message: String }`.
  - `impl Entity for TmuxControlSessions { type Event = TmuxControlEvent; }` and `SingletonEntity`.
  - Methods on `TmuxControlSessions`:
    - `new() -> Self`
    - `attach(&mut self, target: AttachTarget, owner_window: WindowId, ctx: &mut ModelContext<Self>)`
    - `release_window(&mut self, key: TmuxSessionKey, window: WindowId, ctx: &mut ModelContext<Self>)`
    - `fn attach_with_connection(&mut self, connection: Connection, events: async_channel::Receiver<ConnectionEvent>, owner_window: WindowId, ctx: &mut ModelContext<Self>)`, private and used by `attach` and the tests.

- [ ] **Step 1: Write the failing tests** (`sessions_tests.rs`, `App::test`, `FakeTmux`, recording emitted events through `ctx.subscribe_to_model`, waiting with `assert_eventually`)

```rust
#[test]
fn attach_lists_windows_then_follows_add_rename_close_and_exit() {
    // After attach_with_connection and FakeTmux.handshake("demo", "3.8"):
    //   read_command() == list_windows(); reply(&["@0 %0 zsh", "@1 %1 claude"])
    //   → Attached { key: TmuxSessionKey { session: SessionId(0) }, session_name: "demo",
    //       windows: [WindowInfo{@0,%0,"zsh"}, WindowInfo{@1,%1,"claude"}], .. }
    //   send "%window-add @2"; read_command() == describe_window(WindowId(2)); reply(&["@2 %2 build"])
    //   → WindowAdded { window: WindowInfo{@2,%2,"build"}, .. }
    //   send "%window-renamed @1 review" → WindowRenamed { window: @1, name: "review" }
    //   send "%session-renamed $0 work"  → SessionRenamed { name: "work" }
    //   send "%window-close @0"          → WindowClosed { window: @0 }
    //   send "%exit", close the pipe     → Detached { reason: None }
}

#[test]
fn version_refusal_becomes_attach_failed() {
    // handshake("demo", "3.3") → AttachFailed { message: "tmux 3.3 is too old; Tarp needs tmux 3.4 or newer" }
}

#[test]
fn releasing_the_last_window_detaches() {
    // After Attached with windows @0 and @1:
    //   release_window(key, @0) writes nothing (try_read_command(W) == None);
    //   release_window(key, @1) → read_command() == "detach-client".
}

#[test]
fn attaching_an_attached_session_relists_on_the_existing_connection() {
    // A second connection's handshake reports $0 as well:
    //   the second connection gets "detach-client";
    //   the first gets list_windows() again;
    //   a new Attached is emitted with the first connection.
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p warp tmux_control::sessions`
  Expected: compile errors: `TmuxControlEvent`, `attach_with_connection` not found.

- [ ] **Step 3: Implement**

  `attach`:
  - Call `Connection::spawn(SpawnConfig { binary: tmux_binary().into(), socket_name, target: session, connection: ConnectionConfig::default() })`.
  - An `Err` emits `AttachFailed { message: err.to_string() }`.
  - On success, call `attach_with_connection`, which consumes the events with `ctx.spawn_stream_local(events, …)`.

  Pending connections are keyed by a local `u64` until `Ready`. On `Ready { session, .. }`:
  - **Key already attached.** `detach()` the new connection and re-list on the existing one.
  - **New key.** `connection.command(list_windows())`. Await it with `ctx.spawn`, then insert `AttachedSession { connection, owner_window, name, open_windows: HashSet<WindowId> }` and emit `Attached`.

  Notification mapping matches every `Notification` variant:
  - `WindowAdd` → `describe_window`, then `WindowAdded`.
  - `WindowClose`, `WindowRenamed` and `SessionRenamed` → their events.
  - Everything else is ignored in phase 1.

  Connection outcomes:
  - `Failed` → `AttachFailed`.
  - `Closed` on an attached key → remove it and emit `Detached`.

  `release_window` removes the window from `open_windows` and calls `detach()` when the set is empty.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p warp tmux_control::sessions`
  Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add app/src/terminal/tmux_control app/src/lib.rs app/src/workspace/view_tests.rs
git commit -m "Add TmuxControlSessions: one control connection per tmux session, app-wide"
```

---

### Task 10: Workspace entry point and window tabs

**Files:**
- Create: `app/src/workspace/view/tmux_control_tabs.rs` (declared in `app/src/workspace/view.rs` next to `mod tab_grouping;` at line 20, under `#[cfg(not(target_family = "wasm"))]`)
- Modify: `app/src/workspace/view.rs`:
  - field `tmux_window_tabs: HashMap<(TmuxSessionKey, WindowId), EntityId>`,
  - subscribe to `TmuxControlSessions` in `Workspace::new`, next to the `CLIAgentSessionsModel` subscription at 3340,
  - the `AttachTmuxSession` arm in `handle_action`,
  - in `snapshot` (12191), filter `!self.is_tmux_window_tab(pane_group_view.id())` before `.map`, next to the existing read-only filter.
- Modify: `app/src/workspace/action.rs` (`WorkspaceAction::AttachTmuxSession`; add it to the `false` arm of `should_save_app_state_on_action`)
- Modify: `app/src/workspace/mod.rs` (palette binding)
- Modify: `app/src/workspace/view/tab_grouping.rs`:
  - `put_tab_in_named_group` (1414) becomes `pub(super)`,
  - `mod tmux_agents` (1672) becomes `pub(in crate::workspace::view) mod tmux_agents`.
- Test: `app/src/workspace/view_tests.rs`

**Interfaces:**
- Consumes:
  - `TmuxControlSessions`, `TmuxControlEvent` and `AttachTarget` (Task 9),
  - `PaneGroup::new_for_tmux_window` and `set_custom_title` (Task 8),
  - `TmuxPaneBinding` (Task 8),
  - `tmux_control_mode_enabled` (Task 6),
  - `tmux_agents::tmux_client_sessions()` and `vertical_tabs::tab_terminal_shell_pids` (existing).
- Produces:
  - `WorkspaceAction::AttachTmuxSession`.
  - `Workspace::attach_tmux_session(&mut self, ctx: &mut ViewContext<Self>)`.
  - `pub(super) fn handle_tmux_control_event(&mut self, event: &TmuxControlEvent, ctx: &mut ViewContext<Self>)`.
  - `pub(super) fn is_tmux_window_tab(&self, pane_group_id: EntityId) -> bool`.

**Entry point decision.** The palette entry is "Attach tmux session" (`workspace:attach_tmux_session`):
- `.with_enabled(|| FeatureFlag::TmuxControlMode.is_enabled())`,
- context `id!("Workspace") & id!(flags::TMUX_CONTROL_MODE_CONTEXT_FLAG)`,
- group `Navigation`.

It attaches the session of the active tab when that tab's shell is a tmux client (target `"$<id>"` from `tmux_client_sessions()`, looked up in `tokio::task::spawn_blocking` as `poll_tmux_agents` does). Otherwise it uses no `-t`, and tmux picks its most recently used session. There is no session picker in phase 1.

- [ ] **Step 1: Write the failing tests** (`view_tests.rs`)

  These use `FakeTmux` connections and call `handle_tmux_control_event` directly, with `GroupedTabs` and `TmuxControlMode` overridden on. The helper `attached(windows: &[(u32, u32, &str)]) -> TmuxControlEvent` builds `Attached` for key `$0`, session `"demo"`, and the workspace's own `window_id`.

```rust
#[test]
fn test_tmux_attached_session_opens_window_tabs_in_one_group() {
    // A workspace with 1 tab receives attached(&[(0, 0, "zsh"), (1, 1, "claude")]):
    //   tabs.len() == 3;
    //   tabs 1 and 2 share a group whose name == Some("demo");
    //   display_title of tabs 1 and 2 == "zsh", "claude";
    //   active_tab_index == 1.
}

#[test]
fn test_tmux_window_events_follow_tmux() {
    // After attached(...) as above:
    //   WindowAdded @2 "build" → tabs.len() == 4, tab 3 is in the "demo" group, active_tab_index still 1;
    //   WindowRenamed @1 "review" → tab titled "review";
    //   SessionRenamed "work" → the group name == Some("work");
    //   WindowClosed @0 → tabs.len() == 3 and no tab titled "zsh";
    //   Detached → tabs.len() == 1.
}

#[test]
fn reattach_reopens_closed_windows_without_duplicates() {
    // attached(@0, @1); the user closes the @1 tab (close_tab); attached(@0, @1) again
    //   → tabs.len() == 3 and exactly one tab per window.
}

#[test]
fn snapshot_excludes_tmux_window_tabs() {
    // After attached(@0, @1): workspace.snapshot(window_id, false, ctx).tabs.len() == 1
}

#[test]
fn attach_failure_shows_toast_and_opens_no_tab() {
    // AttachFailed { owner_window: window_id, message: "tmux refused to attach: no sessions" }
    //   → tabs.len() == 1 and the toast stack holds one toast with that message.
    // The same event with another owner_window adds no toast.
}

#[test]
fn attach_action_is_inert_when_control_mode_is_off() {
    // With the setting off, handle_action(&WorkspaceAction::AttachTmuxSession) spawns nothing
    // and tabs.len() is unchanged.
}
```

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo nextest run -p warp test_tmux_ snapshot_excludes_tmux reattach_reopens attach_failure_shows attach_action_is_inert`
  Expected: compile errors: no `handle_tmux_control_event`.

- [ ] **Step 3: Implement**

  Events are handled only when `owner_window == self.window_id` (events without an owner, such as `WindowClosed`, act on whatever this workspace has in `tmux_window_tabs`).

  - **`Attached`** is idempotent:
    - For each window without a live tab, build `PaneGroup::new_for_tmux_window` (arguments as in `add_tab_for_joining_shared_session`, view.rs:4687).
    - Push it with `TabData::new`, push its id to `tab_mru_order`, and subscribe `handle_file_tree_event`.
    - `set_custom_title(Some(name))`, and record it in `tmux_window_tabs`.
    - When `FeatureFlag::GroupedTabs` is on, `put_tab_in_named_group(index, &session_name)`.
    - Activate the first newly created tab, if any.
  - **`WindowAdded`:** insert at `index_after_group(group)` (or at the end) without activating.
  - **`WindowRenamed`:** `set_custom_title`.
  - **`SessionRenamed`:** rename the group that holds the key's tabs.
  - **`WindowClosed`:** `close_tab(index, true, false, ctx)` and remove the map entry.
  - **`Detached`:** the same for every tab of the key.
  - **`AttachFailed`:** `toast_stack.add_ephemeral_toast(DismissibleToast::error(message), ctx)`.

  `attach_tmux_session` returns at once unless `tmux_control_mode_enabled(ctx)`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo nextest run -p warp test_tmux_ snapshot_excludes_tmux reattach_reopens attach_failure_shows attach_action_is_inert`
  Expected: 6 passed, plus the existing `test_tmux_adopt_*` tests unchanged and passing.

- [ ] **Step 5: Commit**

```bash
git add app/src/workspace
git commit -m "Attach a tmux session from the palette: its windows open as tabs in one group"
```

---

### Task 11: Exit-criterion check, lint and format

**Files:** none new.

- [ ] **Step 1: Run the full targeted suite**

  Run: `cargo nextest run -p tmux_control && cargo nextest run -p warp tmux raw_mode running_summary_skips_raw_sessions`
  Expected: all pass, and `test_tmux_adopt_*` is still green.

- [ ] **Step 2: Run Clippy**

  Run: `cargo clippy -p tmux_control --all-targets --tests -- -D warnings && cargo clippy -p warp --all-targets --tests -- -D warnings`
  Expected: no warnings in files this plan touches. As of `5e0c06d6`, `-D warnings` already
  aborts on errors in `warpui_core`, which this plan doesn't touch. If that still happens, rerun
  without `-D warnings` and check that nothing is reported in changed files.

- [ ] **Step 3: Check the exit criterion by hand** (owner's Mac, tmux ≥ 3.4)
  1. Run `tmux new -d -s demo`, then `tmux new-window -t demo -n second`.
  2. In one `demo` window, `cd` to a project and run `claude`.
  3. `./script/run`. From the palette, run "Enable tmux control mode", then "Attach tmux session" from a tab not in tmux.
  4. Expected:
     - two tabs, `zsh`-ish and `second`, in a group named `demo`;
     - Claude Code's screen is restored on attach, typing works, resizing the window re-wraps Claude cleanly;
     - `tmux new-window -t demo -n third` in another terminal adds a tab;
     - `tmux rename-window -t demo:third x` renames it;
     - `tmux kill-window -t demo:x` removes it;
     - quitting Tarp shows no running-process warning, and `tmux ls` still lists `demo`.
  5. With the setting off, the old `integrations.tmux.enabled` sync behaves as before.

- [ ] **Step 4: Format once**

  Run: `./script/format`
  Expected: it exits 0. Commit any formatting changes:

```bash
git add -A && git commit -m "Format"
```
