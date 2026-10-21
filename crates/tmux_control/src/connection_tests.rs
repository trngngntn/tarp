use std::time::Duration;

use super::*;
use crate::commands;
use crate::restore::RESET;
use crate::testing::{FakeTmux, recv_timeout};

const W: Duration = Duration::from_millis(200);

/// Bounded stand-in for `recv_blocking().is_err()`, so a regression fails instead of hanging.
fn closes_within<T>(rx: &async_channel::Receiver<T>, wait: Duration) -> bool {
    let step = Duration::from_millis(1);
    for _ in 0..=wait.as_millis() {
        if rx.is_closed() && rx.is_empty() {
            return true;
        }
        std::thread::sleep(step);
    }
    false
}

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
    assert_eq!(
        recv_timeout(&listed, W),
        Some(Ok(vec![b"@0 %0 zsh".to_vec()]))
    );
    assert!(
        matches!(recv_timeout(&events, W), Some(ConnectionEvent::Ready { version, session: SessionId(0), session_name })
        if version == "3.8" && session_name == "demo")
    );
}

#[test]
fn refuses_tmux_older_than_3_4() {
    let (mut tmux, connection, events) = FakeTmux::start(ConnectionConfig::default());
    let _queued = connection.command(commands::list_windows());
    tmux.handshake("demo", "3.3a");
    assert!(matches!(recv_timeout(&events, W),
        Some(ConnectionEvent::Failed(ConnectionError::VersionTooOld { found })) if found == "3.3a"));
    assert_eq!(tmux.read_command(), "detach-client");
    assert_eq!(tmux.try_read_command(W), None);
}

#[test]
fn unparseable_version_is_accepted() {
    let (mut tmux, _connection, events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "openbsd-7.5");
    assert!(matches!(recv_timeout(&events, W),
        Some(ConnectionEvent::Ready { version, .. }) if version == "openbsd-7.5"));
}

#[test]
fn attach_refused_when_the_first_block_is_an_error() {
    let (mut tmux, _connection, events) = FakeTmux::start(ConnectionConfig::default());
    for line in [
        "%begin 1 315 0",
        "can't find session: nope",
        "%error 1 315 0",
        "%exit",
    ] {
        tmux.send(line);
    }
    tmux.close();
    assert!(matches!(recv_timeout(&events, W),
        Some(ConnectionEvent::Failed(ConnectionError::AttachRefused(m))) if m == "can't find session: nope"));
    assert!(matches!(
        recv_timeout(&events, W),
        Some(ConnectionEvent::Closed { reason: None })
    ));
    assert_eq!(tmux.try_read_command(W), None);
}

#[test]
fn replies_resolve_in_order_and_errors_carry_the_tmux_message() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let (a, b) = (
        connection.command("a".into()),
        connection.command("bogus-command".into()),
    );
    assert_eq!(
        (tmux.read_command(), tmux.read_command()),
        ("a".into(), "bogus-command".into())
    );
    tmux.send("%begin 5 400 0");
    tmux.send("%end 5 400 0");
    tmux.reply(&["one"]);
    tmux.reply_error(&["parse error: unknown command: bogus-command"]);
    assert_eq!(recv_timeout(&a, W), Some(Ok(vec![b"one".to_vec()])));
    assert_eq!(
        recv_timeout(&b, W),
        Some(Err(CommandError::Tmux(
            "parse error: unknown command: bogus-command".into()
        )))
    );
}

#[test]
fn pane_output_is_unescaped_and_routed_to_its_subscriber() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(3));
    tmux.send(r"%output %3 a\033[1mb");
    assert_eq!(
        recv_timeout(&rx, W),
        Some(PaneEvent::Output(b"a\x1b[1mb".to_vec()))
    );
    tmux.send("%output %4 x"); // no subscriber: dropped
    assert_eq!(recv_timeout(&rx, W), None);
}

#[test]
fn restore_is_one_line_and_delivers_restored_before_later_output() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(0));
    connection.restore_pane(PaneId(0));
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(0)));
    tmux.send("%output %0 stale"); // dropped: it is already in the capture
    tmux.reply(&[]); // pause
    tmux.send("%pause %0"); // ignored while restoring
    tmux.reply(&["2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2"]); // state
    tmux.reply(&["ignored"]); // history
    tmux.reply(&["ab", "cd", ""]); // visible
    tmux.reply(&[]); // saved normal
    tmux.reply(&[]); // continue
    tmux.send("%continue %0");
    tmux.send("%output %0 live");
    assert_eq!(
        recv_timeout(&rx, W),
        Some(PaneEvent::Restored(
            [RESET, b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H"].concat()
        ))
    );
    assert_eq!(
        recv_timeout(&rx, W),
        Some(PaneEvent::Output(b"live".to_vec()))
    );
    assert_eq!(recv_timeout(&rx, W), None);
}

#[test]
fn input_becomes_send_keys_chunks() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    connection.send_input(PaneId(3), &[b'a'; 1500]);
    assert_eq!(tmux.read_command().matches(" 61").count(), 1000);
    assert_eq!(tmux.read_command().matches(" 61").count(), 500);
}

#[test]
fn resize_is_refresh_client() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    connection.resize_window(WindowId(2), 120, 40);
    assert_eq!(tmux.read_command(), "refresh-client -C @2:120x40");
}

#[test]
fn a_full_pane_channel_pauses_the_pane_until_drained() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig {
        pane_channel_capacity: 2,
    });
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(1));
    for _ in 0..3 {
        tmux.send("%output %1 x");
    }
    assert_eq!(tmux.read_command(), "refresh-client -A '%1:pause'");
    assert_eq!(rx.len(), 2);
    assert!(connection.resume_if_paused(PaneId(1)));
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(1)));
    assert!(!connection.resume_if_paused(PaneId(1)));
}

#[test]
fn tmux_pause_wakes_the_pane() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(1));
    tmux.send("%pause %1");
    assert_eq!(recv_timeout(&rx, W), Some(PaneEvent::Paused));
    assert!(connection.resume_if_paused(PaneId(1)));
}

#[test]
fn eof_fails_pending_commands_and_reports_the_exit_reason() {
    let (mut tmux, connection, events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let (pending, pane) = (
        connection.command("x".into()),
        connection.subscribe_pane(PaneId(1)),
    );
    assert_eq!(tmux.read_command(), "x");
    tmux.send("%exit detached");
    tmux.close();
    assert_eq!(
        recv_timeout(&pending, W),
        Some(Err(CommandError::Disconnected))
    );
    let closed = std::iter::from_fn(|| recv_timeout(&events, W))
        .find(|event| matches!(event, ConnectionEvent::Closed { .. }));
    assert!(matches!(closed, Some(ConnectionEvent::Closed { reason: Some(r) }) if r == "detached"));
    assert!(closes_within(&pane, W));
}

#[test]
fn unsubscribe_closes_the_pane_receiver() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(1));
    connection.unsubscribe_pane(PaneId(1));
    assert!(closes_within(&rx, W));
}

#[test]
fn a_failed_restore_part_skips_the_rest_of_its_line_and_resumes_the_pane() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(0));
    connection.restore_pane(PaneId(0));
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(0)));
    tmux.reply(&[]); // pause
    tmux.reply(&["2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2"]); // state
    tmux.reply_error(&["boom"]); // history; tmux skips the rest of the line
    assert_eq!(tmux.read_command(), commands::continue_pane(PaneId(0)));
    let next = connection.command("x".into());
    assert_eq!(tmux.read_command(), "x");
    tmux.reply(&[]); // continue
    tmux.reply(&["got x"]);
    assert_eq!(recv_timeout(&next, W), Some(Ok(vec![b"got x".to_vec()])));
    tmux.send("%output %0 live");
    assert_eq!(
        recv_timeout(&rx, W),
        Some(PaneEvent::Output(b"live".to_vec()))
    );
}

#[test]
fn an_error_inside_a_line_fails_its_later_commands_and_keeps_replies_paired() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let (txs, rxs): (Vec<_>, Vec<_>) = (0..3).map(|_| async_channel::bounded(1)).unzip();
    connection.queue(
        "a ; b ; c".into(),
        txs.into_iter().map(Tag::Reply).collect(),
    );
    let next = connection.command("next".into());
    assert_eq!(tmux.read_command(), "a ; b ; c");
    assert_eq!(tmux.read_command(), "next");
    tmux.reply(&["A"]);
    tmux.reply_error(&["b failed"]);
    tmux.reply(&["N"]);
    let failed = Some(Err(CommandError::Tmux("b failed".into())));
    assert_eq!(recv_timeout(&rxs[0], W), Some(Ok(vec![b"A".to_vec()])));
    assert_eq!(recv_timeout(&rxs[1], W), failed);
    assert_eq!(recv_timeout(&rxs[2], W), failed);
    assert_eq!(recv_timeout(&next, W), Some(Ok(vec![b"N".to_vec()])));
}

#[test]
fn restore_while_restoring_is_ignored() {
    let (mut tmux, connection, _events) = FakeTmux::start(ConnectionConfig::default());
    tmux.handshake("demo", "3.8");
    let rx = connection.subscribe_pane(PaneId(0));
    connection.restore_pane(PaneId(0));
    connection.restore_pane(PaneId(0));
    assert_eq!(tmux.read_command(), commands::restore_pane(PaneId(0)));
    assert_eq!(tmux.try_read_command(W), None);
    for body in [
        &[][..],
        &["2 1 0 0 0 0 3 0 0 0 0 0 0 0 1 0 2"],
        &[],
        &["ab", "cd", ""],
        &[],
        &[],
    ] {
        tmux.reply(body);
    }
    assert_eq!(
        recv_timeout(&rx, W),
        Some(PaneEvent::Restored(
            [RESET, b"\x1b[H\x1b[2J\x1b[3Jab\r\ncd\r\n\x1b[2;3H"].concat()
        ))
    );
    assert_eq!(recv_timeout(&rx, W), None);
}
