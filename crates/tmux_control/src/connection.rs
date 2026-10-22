use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use async_channel::TrySendError;
use command::blocking::Command;
use parking_lot::Mutex;

use crate::commands;
use crate::ids::{PaneId, SessionId, WindowId};
use crate::layout::{PaneSize, pane_sizes};
use crate::parser::{CommandReply, ControlEvent, ControlParser, Notification};
use crate::queue::CommandQueue;
use crate::restore::{PaneCapture, PaneState, restore_bytes};

/// How long to wait for tmux's stderr once its stdout closed, since the two pipes close
/// independently.
const STDERR_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug)]
pub struct ConnectionConfig {
    /// Events buffered per pane before tmux is asked to pause it.
    pub pane_channel_capacity: usize,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            pane_channel_capacity: 256,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpawnConfig {
    pub binary: String,
    /// The `-L` socket name; `None` uses tmux's default server.
    pub socket_name: Option<String>,
    /// The session to attach to; `None` lets tmux pick.
    pub target: Option<String>,
    pub connection: ConnectionConfig,
}

#[derive(Debug)]
pub enum ConnectionEvent {
    Ready {
        version: String,
        session: SessionId,
        session_name: String,
    },
    /// Never `Output`, `Pause` or `Continue`: those go to pane subscribers.
    Notification(Notification),
    Failed(ConnectionError),
    Closed {
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("tmux {found} is too old; Tarp needs tmux 3.4 or newer")]
    VersionTooOld { found: String },
    #[error("tmux refused to attach: {0}")]
    AttachRefused(String),
    #[error("could not start tmux: {0}")]
    Spawn(String),
}

#[derive(Debug, PartialEq)]
pub enum PaneEvent {
    Output(Vec<u8>),
    /// Bytes that redraw the pane from scratch at `size`; output after it continues from there.
    Restored { bytes: Vec<u8>, size: PaneSize },
    /// tmux stopped sending output; call `Connection::resume_if_paused` once caught up.
    Paused,
}

pub type CommandResult = Result<Vec<Vec<u8>>, CommandError>;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("{0}")]
    Tmux(String),
    #[error("tmux connection closed")]
    Disconnected,
}

/// Accepts versions that don't parse, such as distribution builds, rather than refuse a working
/// tmux.
pub(crate) fn version_supported(version: &str) -> bool {
    commands::parse_version(version).is_none_or(|parsed| parsed >= commands::MIN_VERSION)
}

#[derive(Clone, Copy, Debug)]
enum RestorePart {
    Pause,
    State,
    History,
    Visible,
    SavedNormal,
    Continue,
}

/// In the order `commands::restore_pane` replies.
const RESTORE_PARTS: [RestorePart; commands::RESTORE_REPLIES] = [
    RestorePart::Pause,
    RestorePart::State,
    RestorePart::History,
    RestorePart::Visible,
    RestorePart::SavedNormal,
    RestorePart::Continue,
];

/// What to do with the reply to a written command.
enum Tag {
    Ignore,
    Reply(async_channel::Sender<CommandResult>),
    Version,
    Restore { pane: PaneId, part: RestorePart },
}

/// A tag with the written line it belongs to.
struct Queued {
    line: u64,
    tag: Tag,
}

fn fail(tags: impl IntoIterator<Item = Tag>) {
    for tag in tags {
        match tag {
            Tag::Reply(reply) => {
                let _ = reply.try_send(Err(CommandError::Disconnected));
            }
            Tag::Ignore | Tag::Version | Tag::Restore { .. } => {}
        }
    }
}

enum WriterMsg {
    /// Written as soon as received, ahead of held lines.
    Probe(String, Tag),
    /// Held until `Release`.
    Line {
        line: String,
        tags: Vec<Tag>,
    },
    Release {
        first: Vec<(String, Tag)>,
    },
    Shutdown,
}

impl WriterMsg {
    fn into_tags(self) -> Vec<Tag> {
        match self {
            WriterMsg::Probe(_, tag) => vec![tag],
            WriterMsg::Line { tags, .. } => tags,
            WriterMsg::Release { first } => first.into_iter().map(|(_, tag)| tag).collect(),
            WriterMsg::Shutdown => Vec::new(),
        }
    }
}

enum PaneMode {
    Live {
        paused: bool,
    },
    Restoring {
        capture: PaneCapture,
        /// Set when a part was unreadable; the rest of the line still runs but delivers nothing.
        failed: bool,
        /// The pane's size from a layout change during the restore, which the capture may predate.
        resized_to: Option<PaneSize>,
    },
}

struct PaneRoute {
    sender: async_channel::Sender<PaneEvent>,
    mode: PaneMode,
    /// The size of the last restore delivered.
    size: Option<PaneSize>,
}

impl PaneRoute {
    /// Returns false if a restore is already in flight.
    fn begin_restore(&mut self) -> bool {
        if let PaneMode::Restoring { .. } = self.mode {
            return false;
        }
        self.mode = PaneMode::Restoring {
            capture: PaneCapture {
                state: PaneState::default(),
                history: Vec::new(),
                visible: Vec::new(),
                saved_normal: Vec::new(),
            },
            failed: false,
            resized_to: None,
        };
        true
    }

    /// Restores the pane when tmux resized it, since tmux doesn't resend a resized pane's content.
    fn on_resize(&mut self, pane: PaneId, size: PaneSize, writer: &mpsc::Sender<WriterMsg>) {
        match &mut self.mode {
            PaneMode::Live { .. } => {
                if self.size.is_some_and(|known| known != size) && self.begin_restore() {
                    let _ = writer.send(restore_line(pane));
                }
            }
            PaneMode::Restoring { resized_to, .. } => *resized_to = Some(size),
        }
    }

    /// Pauses the pane in tmux when the subscriber falls behind; it catches up with a restore.
    fn deliver(&mut self, pane: PaneId, event: PaneEvent, writer: &mpsc::Sender<WriterMsg>) {
        match self.sender.try_send(event) {
            Ok(()) | Err(TrySendError::Closed(_)) => {}
            Err(TrySendError::Full(_)) => {
                self.mode = PaneMode::Live { paused: true };
                let _ = writer.send(WriterMsg::Line {
                    line: commands::pause_pane(pane),
                    tags: vec![Tag::Ignore],
                });
            }
        }
    }
}

struct Shared {
    /// Pushed only by the writer thread, just before each write, so it matches the write order.
    queue: CommandQueue<Queued>,
    panes: HashMap<PaneId, PaneRoute>,
    closed: bool,
}

/// A tmux control-mode client. Clones share one connection.
#[derive(Clone)]
pub struct Connection {
    shared: Arc<Mutex<Shared>>,
    writer: mpsc::Sender<WriterMsg>,
    config: ConnectionConfig,
}

impl Connection {
    /// `stderr`, when given, supplies the `Closed` reason if tmux exits without one.
    pub(crate) fn start(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        stderr: Option<Box<dyn Read + Send>>,
        config: ConnectionConfig,
    ) -> (Connection, async_channel::Receiver<ConnectionEvent>) {
        let shared = Arc::new(Mutex::new(Shared {
            queue: CommandQueue::new(),
            panes: HashMap::new(),
            closed: false,
        }));
        let (writer_tx, writer_rx) = mpsc::channel();
        let (events_tx, events_rx) = async_channel::unbounded();

        let stderr = stderr.map(|mut stderr| {
            let (tx, rx) = mpsc::sync_channel(1);
            spawn("tmux-stderr", move || {
                let mut text = Vec::new();
                let _ = stderr.read_to_end(&mut text);
                let _ = tx.send(String::from_utf8_lossy(&text).trim().to_owned());
            });
            rx
        });

        let mut writer_thread = WriterThread {
            shared: shared.clone(),
            writer,
            next_line: 0,
        };
        spawn("tmux-writer", move || writer_thread.run(writer_rx));

        let reader_thread = ReaderThread {
            shared: shared.clone(),
            writer: writer_tx.clone(),
            events: events_tx,
            stderr,
            attached: false,
            session: None,
            version: None,
            ready: false,
            exit_reason: None,
        };
        spawn("tmux-reader", move || reader_thread.run(reader));

        let connection = Connection {
            shared,
            writer: writer_tx,
            config,
        };
        (connection, events_rx)
    }

    pub fn from_io(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        config: ConnectionConfig,
    ) -> (Connection, async_channel::Receiver<ConnectionEvent>) {
        Self::start(reader, writer, None, config)
    }

    /// Runs `tmux -C attach` over pipes. The returned events end with `Closed` when tmux exits.
    pub fn spawn(
        config: SpawnConfig,
    ) -> Result<(Connection, async_channel::Receiver<ConnectionEvent>), ConnectionError> {
        let mut command = Command::new(&config.binary);
        if let Some(socket) = &config.socket_name {
            command.args(["-L", socket]);
        }
        command.args(["-C", "attach"]);
        if let Some(target) = &config.target {
            command.args(["-t", target]);
        }
        let mut child = command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| ConnectionError::Spawn(err.to_string()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ConnectionError::Spawn("tmux pipes unavailable".into()));
        };
        spawn("tmux-reaper", move || {
            let _ = child.wait();
        });
        Ok(Self::start(
            stdout,
            stdin,
            Some(Box::new(stderr)),
            config.connection,
        ))
    }

    /// `line` must hold a single tmux command: each command in a line gets its own reply.
    pub fn command(&self, line: String) -> async_channel::Receiver<CommandResult> {
        let (tx, rx) = async_channel::bounded(1);
        self.queue(line, vec![Tag::Reply(tx)]);
        rx
    }

    /// Replaces any earlier subscriber of `pane`.
    pub fn subscribe_pane(&self, pane: PaneId) -> async_channel::Receiver<PaneEvent> {
        let (sender, rx) = async_channel::bounded(self.config.pane_channel_capacity);
        let mut shared = self.shared.lock();
        if !shared.closed {
            let route = PaneRoute {
                sender,
                mode: PaneMode::Live { paused: false },
                size: None,
            };
            shared.panes.insert(pane, route);
        }
        rx
    }

    pub fn unsubscribe_pane(&self, pane: PaneId) {
        self.shared.lock().panes.remove(&pane);
    }

    /// Sends the subscriber a `Restored` snapshot, dropping output until it arrives. Does nothing
    /// for a pane without a subscriber or with a restore already in flight.
    pub fn restore_pane(&self, pane: PaneId) {
        let started =
            (self.shared.lock().panes.get_mut(&pane)).is_some_and(PaneRoute::begin_restore);
        if started {
            self.queue_restore(pane);
        }
    }

    pub fn send_input(&self, pane: PaneId, bytes: &[u8]) {
        for line in commands::send_keys(pane, bytes) {
            self.queue(line, vec![Tag::Ignore]);
        }
    }

    pub fn resize_window(&self, window: WindowId, columns: usize, rows: usize) {
        self.queue(
            commands::resize_window(window, columns, rows),
            vec![Tag::Ignore],
        );
    }

    /// Restores a paused pane and returns true, or returns false if it wasn't paused.
    pub fn resume_if_paused(&self, pane: PaneId) -> bool {
        {
            let mut shared = self.shared.lock();
            let Some(route) = shared.panes.get_mut(&pane) else {
                return false;
            };
            match route.mode {
                PaneMode::Live { paused: true } => route.begin_restore(),
                PaneMode::Live { paused: false } | PaneMode::Restoring { .. } => return false,
            };
        }
        self.queue_restore(pane);
        true
    }

    pub fn detach(&self) {
        self.queue(commands::detach_client(), vec![Tag::Ignore]);
    }

    fn queue_restore(&self, pane: PaneId) {
        self.send(restore_line(pane));
    }

    fn queue(&self, line: String, tags: Vec<Tag>) {
        self.send(WriterMsg::Line { line, tags });
    }

    fn send(&self, msg: WriterMsg) {
        if let Err(mpsc::SendError(msg)) = self.writer.send(msg) {
            fail(msg.into_tags());
        }
    }
}

fn restore_line(pane: PaneId) -> WriterMsg {
    WriterMsg::Line {
        line: commands::restore_pane(pane),
        tags: RESTORE_PARTS.map(|part| Tag::Restore { pane, part }).into(),
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .unwrap_or_else(|err| panic!("failed to spawn {name} thread: {err}"));
}

struct WriterThread<W> {
    shared: Arc<Mutex<Shared>>,
    writer: W,
    next_line: u64,
}

impl<W: Write> WriterThread<W> {
    fn run(&mut self, messages: mpsc::Receiver<WriterMsg>) {
        let mut held = Vec::new();
        let mut released = false;
        for msg in messages.iter() {
            match msg {
                WriterMsg::Probe(line, tag) => self.write(&line, vec![tag]),
                WriterMsg::Line { line, tags } if released => self.write(&line, tags),
                WriterMsg::Line { line, tags } => held.push((line, tags)),
                WriterMsg::Release { first } => {
                    for (line, tag) in first {
                        self.write(&line, vec![tag]);
                    }
                    for (line, tags) in held.drain(..) {
                        self.write(&line, tags);
                    }
                    released = true;
                }
                WriterMsg::Shutdown => break,
            }
        }
        fail(held.into_iter().flat_map(|(_, tags)| tags));
        fail(messages.try_iter().flat_map(WriterMsg::into_tags));
    }

    /// Queues the tags before writing, outside the lock so a stalled tmux can't block callers. No
    /// reply can precede its write, and tags of a failed write are failed when tmux closes.
    fn write(&mut self, line: &str, tags: Vec<Tag>) {
        {
            let mut shared = self.shared.lock();
            if shared.closed {
                drop(shared);
                fail(tags);
                return;
            }
            for tag in tags {
                shared.queue.push(Queued {
                    line: self.next_line,
                    tag,
                });
            }
        }
        self.next_line += 1;
        let written = self
            .writer
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.writer.flush());
        if let Err(err) = written {
            log::warn!("failed to write tmux command: {err}");
        }
    }
}

struct ReaderThread {
    shared: Arc<Mutex<Shared>>,
    writer: mpsc::Sender<WriterMsg>,
    events: async_channel::Sender<ConnectionEvent>,
    stderr: Option<mpsc::Receiver<String>>,
    /// Set by the first block, which tmux sends on attach.
    attached: bool,
    /// From `%session-changed` until `Ready` is emitted.
    session: Option<(SessionId, String)>,
    /// Set once the version is accepted.
    version: Option<String>,
    ready: bool,
    exit_reason: Option<String>,
}

impl ReaderThread {
    fn run(mut self, reader: impl Read) {
        let mut reader = BufReader::new(reader);
        let mut parser = ControlParser::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) => {
                    log::warn!("failed to read from tmux: {err}");
                    break;
                }
            }
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            match parser.push_line(&line) {
                Some(ControlEvent::Reply(reply)) => self.on_reply(reply),
                Some(ControlEvent::Notification(notification)) => {
                    self.on_notification(notification)
                }
                None => {}
            }
        }
        self.finish();
    }

    fn emit(&self, event: ConnectionEvent) {
        let _ = self.events.try_send(event);
    }

    fn on_reply(&mut self, reply: CommandReply) {
        if !self.attached {
            self.attached = true;
            if reply.success {
                let _ = self
                    .writer
                    .send(WriterMsg::Probe(commands::version_query(), Tag::Version));
            } else {
                self.emit(ConnectionEvent::Failed(ConnectionError::AttachRefused(
                    join_body(&reply.body),
                )));
            }
            return;
        }
        let (tag, skipped) = {
            let mut shared = self.shared.lock();
            let Some(Queued { line, tag }) = shared.queue.complete(&reply) else {
                return;
            };
            // tmux skips the rest of a line after a failed command, so those get no reply.
            let skipped = if reply.success {
                Vec::new()
            } else {
                shared.queue.pop_front_while(|queued| queued.line == line)
            };
            (tag, skipped)
        };
        for Queued { tag, .. } in skipped {
            match tag {
                Tag::Reply(sender) => {
                    let _ = sender.try_send(Err(CommandError::Tmux(join_body(&reply.body))));
                }
                // The failed part already ended the restore.
                Tag::Ignore | Tag::Version | Tag::Restore { .. } => {}
            }
        }
        match tag {
            Tag::Ignore => {
                if !reply.success {
                    log::debug!("tmux command failed: {}", join_body(&reply.body));
                }
            }
            Tag::Reply(sender) => {
                let result = if reply.success {
                    Ok(reply.body)
                } else {
                    Err(CommandError::Tmux(join_body(&reply.body)))
                };
                let _ = sender.try_send(result);
            }
            Tag::Version => self.on_version(reply),
            Tag::Restore { pane, part } => self.on_restore_part(pane, part, reply),
        }
    }

    fn on_version(&mut self, reply: CommandReply) {
        let version = reply
            .body
            .first()
            .map(|line| String::from_utf8_lossy(line).trim().to_owned())
            .unwrap_or_default();
        if !version_supported(&version) {
            self.emit(ConnectionEvent::Failed(ConnectionError::VersionTooOld {
                found: version,
            }));
            let _ = self
                .writer
                .send(WriterMsg::Probe(commands::detach_client(), Tag::Ignore));
            return;
        }
        let pause_after = commands::pause_after(commands::PAUSE_AFTER_SECS);
        let _ = self.writer.send(WriterMsg::Release {
            first: vec![(pause_after, Tag::Ignore)],
        });
        self.version = Some(version);
        self.emit_ready_if_known();
    }

    fn emit_ready_if_known(&mut self) {
        match (self.version.take(), self.session.take()) {
            (Some(version), Some((session, session_name))) => {
                self.ready = true;
                self.emit(ConnectionEvent::Ready {
                    version,
                    session,
                    session_name,
                });
            }
            (version, session) => {
                self.version = version;
                self.session = session;
            }
        }
    }

    fn on_restore_part(&mut self, pane: PaneId, part: RestorePart, reply: CommandReply) {
        let mut shared = self.shared.lock();
        let Some(route) = shared.panes.get_mut(&pane) else {
            return;
        };
        let PaneMode::Restoring {
            capture,
            failed,
            resized_to,
        } = &mut route.mode
        else {
            return;
        };
        if !reply.success {
            log::warn!(
                "restoring {pane} failed at {part:?}: {}",
                join_body(&reply.body)
            );
            route.mode = PaneMode::Live { paused: false };
            // tmux skipped the rest of the line, including its continue.
            let _ = self.writer.send(WriterMsg::Line {
                line: commands::continue_pane(pane),
                tags: vec![Tag::Ignore],
            });
            return;
        }
        match part {
            RestorePart::Pause => {}
            RestorePart::State => {
                match reply.body.first().and_then(|line| PaneState::parse(line)) {
                    Some(state) => capture.state = state,
                    None => {
                        log::warn!("restoring {pane} failed: unreadable pane state");
                        *failed = true;
                    }
                }
            }
            RestorePart::History => capture.history = reply.body,
            RestorePart::Visible => capture.visible = reply.body,
            RestorePart::SavedNormal => capture.saved_normal = reply.body,
            RestorePart::Continue => {
                let size = capture.state.size();
                let restored = (!*failed).then(|| PaneEvent::Restored {
                    bytes: restore_bytes(capture),
                    size,
                });
                let stale = resized_to.is_some_and(|resized_to| resized_to != size);
                route.mode = PaneMode::Live { paused: false };
                if let Some(restored) = restored {
                    route.size = Some(size);
                    route.deliver(pane, restored, &self.writer);
                }
                if stale && route.begin_restore() {
                    let _ = self.writer.send(restore_line(pane));
                }
            }
        }
    }

    fn on_notification(&mut self, notification: Notification) {
        match notification {
            Notification::Output { pane, data } => {
                let mut shared = self.shared.lock();
                if let Some(route) = shared.panes.get_mut(&pane)
                    && let PaneMode::Live { paused: false } = route.mode
                {
                    route.deliver(pane, PaneEvent::Output(data), &self.writer);
                }
            }
            Notification::Pause(pane) => {
                let mut shared = self.shared.lock();
                if let Some(route) = shared.panes.get_mut(&pane)
                    && let PaneMode::Live { paused } = &mut route.mode
                {
                    *paused = true;
                    let _ = route.sender.try_send(PaneEvent::Paused);
                }
            }
            Notification::Continue(_) => {}
            Notification::LayoutChange { window, layout } => {
                {
                    let mut shared = self.shared.lock();
                    for (pane, size) in pane_sizes(&layout).unwrap_or_default() {
                        if let Some(route) = shared.panes.get_mut(&pane) {
                            route.on_resize(pane, size, &self.writer);
                        }
                    }
                }
                self.emit(ConnectionEvent::Notification(Notification::LayoutChange {
                    window,
                    layout,
                }));
            }
            Notification::Exit { reason } => self.exit_reason = reason,
            Notification::SessionChanged { session, name } if !self.ready => {
                self.session = Some((session, name));
                self.emit_ready_if_known();
            }
            other => self.emit(ConnectionEvent::Notification(other)),
        }
    }

    fn finish(mut self) {
        let pending = {
            let mut shared = self.shared.lock();
            shared.closed = true;
            shared.panes.clear();
            shared
                .queue
                .drain()
                .map(|queued| queued.tag)
                .collect::<Vec<_>>()
        };
        fail(pending);
        let _ = self.writer.send(WriterMsg::Shutdown);
        let reason = self.exit_reason.take().or_else(|| {
            self.stderr
                .as_ref()?
                .recv_timeout(STDERR_GRACE)
                .ok()
                .filter(|text| !text.is_empty())
        });
        self.emit(ConnectionEvent::Closed { reason });
    }
}

fn join_body(body: &[Vec<u8>]) -> String {
    body.iter()
        .map(|line| String::from_utf8_lossy(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
