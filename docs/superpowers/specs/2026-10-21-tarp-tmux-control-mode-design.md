# Tarp: tmux control-mode tabs, local and remote

Status: design, not yet planned. Date: 2026-10-21.

## Intent

Tarp is a fork of Harp, itself an unofficial fork of Warp. The owner runs many Claude Code
sessions in tabs on a work Mac and wants to continue them from a home Mac. Today that means
ssh-ing in, `cd`-ing to each project folder and running `claude --resume` for each one.

The goal is iTerm2's tmux integration with Harp's sidebar in front of it:
- Every work tab lives in tmux on the work Mac, so its process survives the app quitting or the
  owner leaving.
- A Tarp on another Mac connects over ssh and shows the same sessions as sidebar tabs, with the
  same groups, names, order and agent status.
- Clicking a tab puts the owner in that live Claude session.

What the owner values in Warp is the **sidebar UI**: grouping, naming, ordering, unread and
inactive marks, and the agent status rings. That includes notifications from Claude Code.
Warp's blocks, input editor and shell integration are **not** needed inside these tabs.

## Goals

1. **Tab per window.** A tmux **window** is a Tarp tab. A tmux **session** is a sidebar
   **group**. The owner chose this model.
2. **tmux as the source of truth.** On a tmux-enabled host, new tabs are created as tmux windows,
   so every session can be resumed later from anywhere.
3. **Remote hosts.** A remote host is reached with the system `ssh` (Tailscale in the owner's
   case). Its sessions and windows appear in the sidebar exactly like local ones.
4. **Two-way layout sync.** Names, groups, order, unread and inactive state live in tmux. Changes
   from either Tarp, or from plain tmux, show up everywhere.
5. **Agent status and notifications for remote panes.** Working, needs-you and done work the same
   as they do locally.
6. **Survive disconnects.** When the ssh connection drops, tabs stay and show "reconnecting". On
   reconnect they restore their screen and scrollback.

## Non-goals (v1)

- Pasting images from the local machine into a remote Claude session. This is a planned later
  feature, and the design must leave room for it (see "Future").
- Warp blocks, the input editor, AI features and completions inside tmux tabs.
- Hosts other than macOS for the Tarp app. The remote side only needs tmux, so a Linux work host
  should work but is untested.
- tmux older than 3.4.
- Migrating existing Harp-style tmux sessions (one tab per session, `@warp_group` labels). The
  owner starts fresh sessions under the new model; old sessions appear as one-tab groups.

## Anti-goals (failure even if it "works")

- The owner still has to `cd` and `claude --resume` after connecting.
- Grouping or names are lost or scrambled at home, or don't round-trip back to work.
- Status rings that lag noticeably or are wrong for remote tabs.
- Shelling out to the tmux CLI on a timer per tab. That was acceptable locally in Harp, but it
  is not acceptable over ssh.
- A tab that flashes, loses scrollback, or garbles a full-screen TUI (Claude Code) after
  attach, resize or reconnect.

## Verified facts this design rests on

These were checked on 2026-10-21 against tmux 3.8 on macOS. Spike scripts are kept out of the
repo.

**tmux control-mode protocol**
- **Output.** `%output %<pane> <data>` carries the exact bytes the pane program wrote. Bytes below
  0x20 and the backslash are 3-digit octal escapes (`\033`, `\134`); everything else is raw,
  including UTF-8. A line can end mid escape sequence, so the VT parser must stay stateful
  across lines.
- **OSC passthrough.** OSC sequences, including `777;notify;warp://cli-agent;…`, arrive in
  `%output` whatever `allow-passthrough` is set to. Sequences already wrapped for tmux
  (`ESC Ptmux; … ESC \`) arrive still wrapped. Claude Code's Warp plugin and Harp's
  `integrations/claude-code/warp-emit.sh` wrap them when `$TMUX` is set, so the client must
  unwrap them.
- **Attach.** Attaching replays no screen. A client must call `capture-pane -p -e -J -S -`
  (add `-a` for the alternate screen) and read cursor and mode state from `list-panes -F` or
  `display-message -p` formats.
- **Input and resize.** Input works with `send-keys -H <hex>` and `send-keys -l <text>`. Resize
  works with `refresh-client -C WxH` and per window with `refresh-client -C @w:WxH`.
- **Events.**
  - Pushed: `%window-add`, `%window-close`, `%window-renamed`, `%layout-change`,
    `%session-renamed`, `%session-changed`, `%sessions-changed` (no payload; re-list) and
    `%unlinked-window-*`.
  - Not pushed: user option changes (`set @x`). `refresh-client -B` subscriptions cover only the
    attached session and are polled by tmux about once a second.
- **Flow control.** `refresh-client -f pause-after=N` switches output to `%extended-output`. A
  slow reader gets `%pause %P`, and `refresh-client -A %P:continue` resumes.
- **Transport.** Control mode works over plain pipes with `tmux -C`, no tty needed. Over ssh it
  has not been tried yet.

**Existing code**
- **`remote_tty` precedent.** `app/src/terminal/remote_tty/` is a working non-PTY tab backend. It
  is fed by a websocket, parses bytes into the normal `TerminalModel` and sends
  `Message::Input` / `Message::Resize` back. It is selected at compile time
  (`app/src/pane_group/mod.rs` ~6112) and uses one connection per tab.
- **Generic event loop.** `EventLoop` is generic over `EventedPty` (`crates/warp_terminal/src/
  local_tty/event_loop.rs`). `PtyController` talks to any backend through `EventLoopSender`.
- **Notifications are byte-source agnostic.** OSC 777 handling is in the shared parser
  (`crates/warp_terminal/src/model/ansi/mod.rs` ~1033) and flows to
  `handle_cli_agent_notification` in `app/src/terminal/view.rs`. Any byte source gets agent
  notifications for free.
- **Bootstrap risk.** A tab waits for Warp's shell bootstrap (DCS hooks). Until then output is
  hidden and input is gated (`app/src/terminal/model/block.rs` ~566, `view.rs` ~10036). tmux tabs
  never bootstrap, so they need an explicit raw mode.
- **Harp's current tmux integration** (`app/src/workspace/view/tab_grouping.rs`). It maps one tmux
  **session** to one tab by matching process trees, polls the tmux CLI every 10 s, stores
  `@warp_group`, `@warp_pos`, `@warp_unread` and `@warp_inactive` on sessions, uses
  `@warp_groups` and `@warp_layout_rev` globally, and supports adoption through `@warp_adopt`.

**iTerm2 as prior art** (GPLv2; design reference only, no code copied)
- **Connection model.** One control client attaches to one session and shows its windows as
  tabs. Switching sessions uses `attach-session` over the same connection.
- **Startup.** Writes are deferred until the first `%begin/%end` block and `%session-changed`.
  It probes `#{version}` before version-gated features and runs a 5 s unresponsive-command
  watchdog.
- **Restoring a pane.**
  - `capture-pane -peqJ` for the normal screen, plus `-a` for the alternate screen.
  - A `list-panes -F` dump of mode state: alt screen, cursor, scroll region, mouse and bracketed
    paste.
  - `capture-pane -p -P -C` for output tmux has not flushed yet.
  - Live `%output` is buffered until the restore finishes.
- **Input.** Control bytes go through `send -H`, literal text through `send -l`, in runs of at
  most about 1000 bytes.
- **Back-pressure.** Uses `pause-after`, re-captures the pane after `%pause`, and avoids
  unbounded queues.

## Decided design (owner-approved)

- **Model.** tmux window = tab, tmux session = group.
- **Transport.** tmux control mode, one control connection per tmux session, the same model as
  iTerm2. Remote hosts go through the system `ssh`.
- **Scope.** Only the sidebar UI and agent status need to work inside tmux tabs.
- **Two-way sync.** Layout state lives in tmux and syncs both ways.

## Builder's design calls

These were made by the designer, not by the owner. Change them freely if the code argues
otherwise.

1. **Local tmux tabs also use control mode.** Locally Tarp runs `tmux -C attach -t $S` over
   pipes, so local and remote share one code path. Tabs outside tmux stay as today's local PTY
   tabs.
2. **Transport command.**
   - Local: `tmux -C …` over pipes.
   - Remote: `ssh -T <host> tmux -C …`, using the owner's ssh config.
   - Connections to one host share a single ssh connection through a Tarp-owned ControlMaster
     socket.
3. **Hosts setting.** Hosts are declared in `~/.tarp/settings.toml`:
   ```toml
   [integrations.tmux]
   control_mode = true            # new; gates everything in this doc

   [[integrations.tmux.hosts]]
   name = "work"
   ssh = "work-mac"               # ssh destination (alias, Tailscale name, user@host)
   ```
   The local host is implicit when `control_mode = true`. Per AGENTS.md, the toggle also gets a
   Command Palette entry.
4. **Feature flag.** A `FeatureFlag::TmuxControlMode` (dogfood) also gates the code. The flag is
   for the code path; the setting is the owner's switch.
5. **Sidebar shape.**
   - Each host is a sidebar section with a small header showing its name and connection state.
   - That host's groups (tmux sessions) sit under it.
   - With only the local host, no header is shown.
6. **State mapping** (tmux ↔ Tarp). All options Tarp owns use a new `@tarp_` prefix, so a Harp
   build talking to the same tmux server can't misread or overwrite them, and the reverse.
   - **Group name:** session name. Renaming a group runs `rename-session`.
   - **Group order:** global `@tarp_groups`, session names one per line.
   - **Tab order inside a group:** window index. Reordering uses `move-window` / `swap-window`.
     Moving a tab to another group is `move-window -s @w -t <session>:`.
   - **Tab name:** window name. Renaming a tab runs `rename-window`. Tarp sets
     `automatic-rename off` on a window it renames.
   - **Unread and inactive:** window options `@tarp_unread` and `@tarp_inactive` (`set -w`).
   - **New tab:** `new-window` in the target session, in the folder of the active tab.
   - **New group:** `new-session -d -s <name>`.
   - **Closing a tab:** `kill-window`. Closing the last tab of a group kills the session.
     Detaching is a separate action.
   - **Pinned tabs:** not mapped in v1; they stay local-only.
7. **Panes.** v1 handles single-pane windows fully. A window with several tmux panes shows its
   active pane and a small "split in tmux" marker. Tarp's own split action is disabled in tmux
   tabs. Mapping tmux panes to Tarp split panes comes later.
8. **Discovering groups.**
   - **Per-host index channel.** Each host has one index channel: a control client attached to
     any one session. It re-lists sessions on `%sessions-changed` and at startup with
     `list-sessions -F` (tab-separated).
   - **One client per session.** Each session that has a group in the sidebar gets its own
     control client. That client receives `%output` for all of its windows, which the status
     rings need.
   - **Option polling.** Option changes made outside Tarp are picked up by a `show-options` poll
     every 2 s inside the existing connections. No new processes are spawned.
9. **Size policy.**
   - Tarp sets each window's size with `refresh-client -C @w:WxH` for the visible tab.
   - Window size follows tmux's default (`window-size latest`): when work and home Tarps are
     both attached, the most recently active client wins.
   - Background tabs keep their last size and are resized when shown.
10. **Raw mode for tabs.** A tmux tab's `TerminalModel` skips Warp bootstrap and starts as a plain
    terminal. Output is shown, input goes straight through, and there are no blocks or input
    editor.
11. **Agent status.**
    - **Notifications.** They come from OSC 777 in `%output`. The connection unwraps
      `ESC Ptmux;` DCS wrappers before parsing.
    - **"Pending" detection.** Uses `#{pane_current_command}` from `list-panes -F`; if that
      proves unreliable for Claude Code, `run-shell 'ps …'` over the same connection.
    - **"Needs you" from `~/.claude/sessions/*.json`.** Read on the tmux host with `run-shell`
      over the same connection, every 2 s. This matches Harp's local logic.
    - Every check runs on the host where Claude runs. No extra ssh processes are spawned.
12. **Reconnect.**
    - **On drop.** Tabs keep their last screen, dimmed, under a "reconnecting" state. Tarp
      retries with backoff from 1 s up to 30 s.
    - **On reconnect.** Each pane is restored as iTerm2 does, and live output is buffered during
      the restore.
    - **Session gone.** If a session no longer exists, its tabs close with a notice.
13. **Coexisting with Harp's session-based tmux sync.**
    - When `control_mode` is on, the old poll-based sync, adoption and process-tree matching in
      `tab_grouping.rs` are turned off.
    - Old session-level `@warp_*` options are ignored.
    - `@warp_adopt` is superseded: any session appears automatically.
14. **Minimum tmux version: 3.4**, because of per-window `refresh-client -C @w:WxH`, pause mode
    and `-B`. Tarp probes `#{version}` on connect and shows a clear error below the floor.
15. **Concurrency.**
    - Each connection runs a reader task and a writer task off the UI thread.
    - Commands go through a strict FIFO queue matched to `%begin/%end/%error` by command number.
    - `%begin` blocks the server starts itself (flags bit clear) are not matched to commands.
    - Per-pane output goes to that tab's event loop through a bounded channel. When the channel
      is full, Tarp leans on tmux pause mode rather than buffering without limit.

16. **Where tmux tabs live.**
    - Control connections are app-wide, one set per host, not per window.
    - Host sections show in one Tarp window: the first window opened, or the one the owner
      picks. Other windows hold only non-tmux tabs.
    - Every session on a host appears by default. "Hide group" is a local, per-host preference
      Tarp stores itself. It does not touch tmux, and the session keeps running.
17. **Non-tmux tabs.**
    - With `control_mode` on, Cmd+T creates a tmux window in the active tab's group, or in the
      local host's default session if the active tab isn't in tmux.
    - "New local tab (no tmux)" in the new-tab menu and the Command Palette creates a plain PTY
      tab. Those tabs sit in a "Local" section above the host sections and keep today's Harp
      grouping.
    - Warp's own session restore (sqlite) restores only non-tmux tabs. tmux tabs are rebuilt
      from tmux on startup.
18. **Status of a multi-pane tab.** It combines all panes, and the most urgent wins: needs-you,
    then working, then done, then idle.

## Suggested phasing

Each phase ends usable.

1. **Local control-mode tabs.**
   - Protocol module: parser for `%output` escapes and wrapper unwrapping, command queue,
     notifications.
   - A tmux tab backend modelled on `remote_tty`, chosen at runtime per tab.
   - Raw mode, input, resize, attach-time restore.
   - Exit criterion: one local session's windows show as tabs, and Claude Code renders and runs
     correctly.
2. **Sidebar mapping.** Hosts, groups and windows; create, close, rename, reorder and move; group
   order; unread and inactive flags; two-way sync with plain tmux.
3. **Remote hosts.** The hosts setting, ssh transport with ControlMaster, the per-host index
   channel, reconnect.
4. **Agent status.** Unwrapping notifications, "pending" detection, Claude "needs you" through
   `run-shell`. Retire the old sync when `control_mode` is on.

## Testing

- **Protocol parser.** Unit tests against recorded control-mode transcripts from a real tmux:
  escapes, split escape sequences, wrapped OSC, interleaved notifications inside `%begin/%end`,
  `%pause`.
- **Integration tests.** Use a private server (`tmux -L <name>`) so a developer's real sessions
  are never touched.
- **Owner check for phase 3.** Over ssh, connect from the home Mac to the work Mac through
  Tailscale.

## Left to the builder

- Module layout, type names, and how the runtime backend choice replaces the compile-time
  `cfg_if` in `pane_group/mod.rs`.
- Exact `list-panes -F` / `list-sessions -F` formats and parsing.
- Status indicators and the "reconnecting" visuals, following `gui-ui-guidelines`.
- Whether `run-shell` or one extra ssh exec is better for reading `~/.claude/sessions` on the
  host. The constraint is no per-tab timers that spawn processes.

## Future: image paste into remote sessions

Claude Code reads images from its own machine. When an image is pasted into a remote tmux tab,
Tarp would:
1. Copy the image to a temp file on the host, over the host's existing ssh ControlMaster with
   `ssh … 'cat > /tmp/…'`.
2. Type the remote path into the pane.

Nothing in v1 may block this. That means keeping a per-host handle to the ssh transport that
can run one-off commands.
