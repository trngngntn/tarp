use std::process::Stdio;
use std::time::Duration;

use async_channel::Receiver;
use command::blocking::Command;

use crate::connection::{
    Connection, ConnectionConfig, ConnectionError, ConnectionEvent, PaneEvent, SpawnConfig,
};
use crate::ids::{PaneId, WindowId};
use crate::layout::PaneSize;
use crate::parser::Notification;
use crate::restore::RESET;
use crate::testing::recv_timeout;
use crate::{commands, tmux_binary};

const SECS: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_STEPS: u32 = 200;

fn size(columns: usize, rows: usize) -> PaneSize {
    PaneSize { columns, rows }
}

/// A private tmux server, killed on drop.
struct PrivateServer {
    socket: String,
}

impl PrivateServer {
    fn tmux(&self) -> Command {
        let mut command = Command::new(tmux_binary());
        command
            .args(["-L", &self.socket])
            .env_remove("TMUX")
            .stdin(Stdio::null());
        command
    }

    fn start(
        name: &str,
        width: u16,
        height: u16,
        command: &str,
        ready_text: &str,
    ) -> Option<PrivateServer> {
        if Command::new(tmux_binary()).arg("-V").output().is_err() {
            eprintln!("skipping: tmux not installed");
            return None;
        }
        let server = PrivateServer {
            socket: format!("tarp-test-{}-{name}", std::process::id()),
        };
        let started = server
            .tmux()
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "demo"])
            .args(["-x", &width.to_string(), "-y", &height.to_string(), command])
            .output()
            .unwrap();
        assert!(started.status.success(), "{started:?}");
        for _ in 0..POLL_STEPS {
            let captured = server.tmux().args(["capture-pane", "-p"]).output().unwrap();
            if String::from_utf8_lossy(&captured.stdout).contains(ready_text) {
                return Some(server);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        panic!("pane never printed {ready_text:?}");
    }

    fn config(&self, target: &str) -> SpawnConfig {
        SpawnConfig {
            binary: tmux_binary().into(),
            socket_name: Some(self.socket.clone()),
            target: Some(target.into()),
            connection: ConnectionConfig::default(),
        }
    }

    fn attach(&self) -> (Connection, Receiver<ConnectionEvent>) {
        let (connection, events) = Connection::spawn(self.config("demo")).unwrap();
        assert!(matches!(
            recv_timeout(&events, SECS),
            Some(ConnectionEvent::Ready { .. })
        ));
        (connection, events)
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        let _ = self.tmux().arg("kill-server").output();
    }
}

/// Drains `events` for up to `SECS`, reporting whether `pred` matched.
fn saw(events: &Receiver<ConnectionEvent>, pred: impl Fn(&ConnectionEvent) -> bool) -> bool {
    std::iter::from_fn(|| recv_timeout(events, SECS))
        .take(POLL_STEPS as usize)
        .any(|event| pred(&event))
}

/// The `L<n>` numbers in `text`, ignoring carriage returns and escape sequences.
fn l_numbers(text: &[u8]) -> Vec<u32> {
    let mut plain = String::new();
    let mut bytes = text.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            0x1b => {
                if bytes.next_if_eq(&b'[').is_some() {
                    while bytes.next_if(|b| !b.is_ascii_alphabetic()).is_some() {}
                }
                bytes.next();
            }
            b'\r' => {}
            byte => plain.push(byte as char),
        }
    }
    plain
        .lines()
        .filter_map(|line| line.strip_prefix('L')?.trim().parse().ok())
        .collect()
}

#[test]
fn attaches_and_follows_window_add_rename_and_close() {
    let Some(server) = PrivateServer::start("windows", 40, 5, "sleep 30", "") else {
        return;
    };
    let (connection, events) = server.attach();
    let windows = recv_timeout(&connection.command(commands::list_windows()), SECS)
        .unwrap()
        .unwrap();
    assert_eq!(windows.len(), 1);
    assert!(windows[0].starts_with(b"@0 %0 "));
    recv_timeout(
        &connection.command("new-window -d -n second 'sleep 30'".into()),
        SECS,
    );
    assert!(saw(&events, |e| matches!(
        e,
        ConnectionEvent::Notification(Notification::WindowAdd(WindowId(1)))
    )));
    recv_timeout(
        &connection.command("rename-window -t @1 third".into()),
        SECS,
    );
    assert!(saw(&events, |e| matches!(
        e,
        ConnectionEvent::Notification(Notification::WindowRenamed { window: WindowId(1), name })
            if name == "third"
    )));
    recv_timeout(&connection.command("kill-window -t @1".into()), SECS);
    assert!(saw(&events, |e| matches!(
        e,
        ConnectionEvent::Notification(Notification::WindowClose(WindowId(1)))
    )));
}

#[test]
fn restore_reproduces_screen_and_cursor() {
    let Some(server) = PrivateServer::start("restore", 20, 3, "printf 'ab\\ncd'; sleep 30", "cd")
    else {
        return;
    };
    let (connection, _events) = server.attach();
    let rx = connection.subscribe_pane(PaneId(0));
    connection.resize_window(WindowId(0), 20, 3);
    connection.restore_pane(PaneId(0));
    assert_eq!(
        recv_timeout(&rx, SECS),
        Some(PaneEvent::Restored {
            bytes: [RESET, b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H"].concat(),
            size: size(20, 3),
        })
    );
}

#[test]
fn input_reaches_the_pane() {
    let Some(server) = PrivateServer::start("input", 40, 5, "cat", "") else {
        return;
    };
    let (connection, _events) = server.attach();
    let rx = connection.subscribe_pane(PaneId(0));
    connection.send_input(PaneId(0), b"hi\r");
    let mut got = Vec::new();
    while !got.windows(2).any(|pair| pair == b"hi") {
        let Some(PaneEvent::Output(bytes)) = recv_timeout(&rx, SECS) else {
            break;
        };
        got.extend(bytes);
    }
    assert!(got.windows(2).any(|pair| pair == b"hi"));
}

#[test]
fn attach_refused_reports_tmux_reason() {
    let Some(server) = PrivateServer::start("refused", 40, 5, "sleep 30", "") else {
        return;
    };
    let (_connection, events) = Connection::spawn(server.config("nope")).unwrap();
    assert!(matches!(
        recv_timeout(&events, SECS),
        Some(ConnectionEvent::Failed(ConnectionError::AttachRefused(m)))
            if m == "can't find session: nope"
    ));
}

#[test]
fn restore_then_live_output_has_no_duplicate_lines() {
    const LIVE_CHUNKS: usize = 20;
    let script = "i=0; while [ $i -lt 3000 ]; do echo L$i; i=$((i+1)); sleep 0.01; done; sleep 30";
    let Some(server) = PrivateServer::start("dupes", 40, 5, script, "L") else {
        return;
    };
    let (connection, _events) = server.attach();
    let rx = connection.subscribe_pane(PaneId(0));
    connection.resize_window(WindowId(0), 40, 5);
    connection.restore_pane(PaneId(0));
    let mut restored = None;
    let mut live = Vec::new();
    let mut live_chunks = 0;
    while live_chunks < LIVE_CHUNKS {
        match recv_timeout(&rx, Duration::from_secs(3)) {
            Some(PaneEvent::Restored { bytes, .. }) => restored = Some(bytes),
            Some(PaneEvent::Output(bytes)) if restored.is_some() => {
                live.extend(bytes);
                live_chunks += 1;
            }
            Some(PaneEvent::Output(_)) => {}
            Some(PaneEvent::Paused) | None => break,
        }
    }
    let restored = l_numbers(&restored.expect("no Restored event"));
    let live = l_numbers(&live);
    assert!(live_chunks > 0, "no live output after the restore");
    assert!(!restored.is_empty() && !live.is_empty());
    let all: Vec<_> = restored.iter().chain(&live).collect();
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]), "{all:?}");
}

#[test]
fn resizing_a_window_restores_its_pane_at_the_new_size() {
    let Some(server) = PrivateServer::start("resize", 20, 3, "printf 'ab'; sleep 30", "ab") else {
        return;
    };
    let (connection, _events) = server.attach();
    let rx = connection.subscribe_pane(PaneId(0));
    connection.resize_window(WindowId(0), 20, 3);
    connection.restore_pane(PaneId(0));
    let mut restored = std::iter::from_fn(|| recv_timeout(&rx, SECS)).filter_map(|event| match event {
        PaneEvent::Restored { size, .. } => Some(size),
        PaneEvent::Output(_) | PaneEvent::Paused => None,
    });
    assert_eq!(restored.next(), Some(size(20, 3)));
    connection.resize_window(WindowId(0), 30, 4);
    assert_eq!(restored.next(), Some(size(30, 4)));
}
