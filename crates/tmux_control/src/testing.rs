//! An in-process stand-in for a tmux control-mode server.

use std::io::{BufRead, BufReader, PipeWriter, Write, pipe};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::commands;
use crate::connection::{Connection, ConnectionConfig, ConnectionEvent, version_supported};
use crate::layout::PaneSize;

const READ_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// The server side of a `Connection` built on pipes.
pub struct FakeTmux {
    to_client: Option<PipeWriter>,
    commands: mpsc::Receiver<String>,
    next_number: u64,
}

impl FakeTmux {
    pub fn start(
        config: ConnectionConfig,
    ) -> (
        FakeTmux,
        Connection,
        async_channel::Receiver<ConnectionEvent>,
    ) {
        let (client_reader, to_client) = pipe().expect("failed to create pipe");
        let (from_client, client_writer) = pipe().expect("failed to create pipe");
        let (commands_tx, commands) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(from_client).lines() {
                let Ok(line) = line else { break };
                if commands_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (connection, events) = Connection::from_io(client_reader, client_writer, config);
        let fake = FakeTmux {
            to_client: Some(to_client),
            commands,
            next_number: 1,
        };
        (fake, connection, events)
    }

    pub fn send(&mut self, line: &str) {
        let to_client = self.to_client.as_mut().expect("FakeTmux is closed");
        writeln!(to_client, "{line}").expect("failed to write to the client");
    }

    /// Panics if the client writes nothing within 5 s.
    pub fn read_command(&mut self) -> String {
        self.try_read_command(READ_COMMAND_TIMEOUT)
            .expect("timed out waiting for a tmux command")
    }

    pub fn try_read_command(&mut self, wait: Duration) -> Option<String> {
        self.commands.recv_timeout(wait).ok()
    }

    /// Makes the client see EOF.
    pub fn close(&mut self) {
        self.to_client = None;
    }

    /// Answers the oldest unanswered command successfully.
    pub fn reply(&mut self, body: &[&str]) {
        self.reply_block(body, "%end");
    }

    /// Answers the oldest unanswered command with an error.
    pub fn reply_error(&mut self, body: &[&str]) {
        self.reply_block(body, "%error");
    }

    fn reply_block(&mut self, body: &[&str], end: &str) {
        let number = self.next_number;
        self.next_number += 1;
        self.send(&format!("%begin 1 {number} 1"));
        for line in body {
            self.send(line);
        }
        self.send(&format!("{end} 1 {number} 1"));
    }

    /// Answers a `commands::restore_pane` line for a pane of `size` showing `visible`.
    pub fn reply_restore(&mut self, size: PaneSize, visible: &[&str]) {
        let state = format!(
            "0 0 0 0 0 0 {} 0 0 0 0 0 0 0 1 0 {} {}",
            size.rows,
            size.rows - 1,
            size.columns
        );
        for body in [&[][..], &[state.as_str()], &[], visible, &[], &[]] {
            self.reply(body);
        }
    }

    /// Attaches the client to `session`, reporting `version`, and leaves `Ready` unread.
    pub fn handshake(&mut self, session: &str, version: &str) {
        self.send("%begin 1 0 0");
        self.send("%end 1 0 0");
        self.send(&format!("%session-changed $0 {session}"));
        assert_eq!(self.read_command(), commands::version_query());
        self.reply(&[version]);
        if version_supported(version) {
            assert_eq!(
                self.read_command(),
                commands::pause_after(commands::PAUSE_AFTER_SECS)
            );
            self.reply(&[]);
        }
    }
}

/// Polls `rx` for up to `wait`. Returns `None` on timeout or when the channel is closed and empty.
pub fn recv_timeout<T>(rx: &async_channel::Receiver<T>, wait: Duration) -> Option<T> {
    let mut remaining = wait;
    loop {
        match rx.try_recv() {
            Ok(value) => return Some(value),
            Err(async_channel::TryRecvError::Closed) => return None,
            Err(async_channel::TryRecvError::Empty) if remaining.is_zero() => return None,
            Err(async_channel::TryRecvError::Empty) => {
                let step = POLL_INTERVAL.min(remaining);
                thread::sleep(step);
                remaining -= step;
            }
        }
    }
}
